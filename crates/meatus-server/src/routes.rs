#![forbid(unsafe_code)]
//! HTTP / HTTPS 域名路由表。
//!
//! 域名全局唯一（按入口类型区分），精确域名优先于泛域名；泛域名只接受
//! `*.example.com` 形式，匹配任意层级的子域名。

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use meatus_core::{OpenHandle, RateLimiter};
use tokio::sync::Semaphore;

use crate::metrics::ProxyMetrics;
use crate::quota::ProxyQuota;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    Http,
    Https,
}

impl RouteKind {
    fn key(self, domain: &str) -> String {
        let prefix = match self {
            Self::Http => "http",
            Self::Https => "https",
        };
        format!("{prefix}:{domain}")
    }
}

#[derive(Clone)]
pub struct DomainRoute {
    pub domain: String,
    pub kind: RouteKind,
    pub proxy_name: String,
    pub tenant_id: String,
    pub client_id: String,
    pub open: OpenHandle,
    pub limiter: Arc<RateLimiter>,
    pub conn_limiter: Arc<RateLimiter>,
    pub quota: Arc<ProxyQuota>,
    pub metrics: Arc<ProxyMetrics>,
    pub next_trace: Arc<AtomicU64>,
    pub connections: Arc<Semaphore>,
}

impl DomainRoute {
    #[must_use]
    pub fn allow_connection(&self) -> bool {
        self.conn_limiter.try_consume(1) && self.quota.allow_connection()
    }
}

pub struct DomainRouter {
    routes: RwLock<HashMap<String, Arc<DomainRoute>>>,
    /// 刚离线、仍处于保留期的域名；HTTP 入口对命中项返回 503。
    offline: RwLock<HashMap<String, Instant>>,
}

impl DomainRouter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            routes: RwLock::new(HashMap::new()),
            offline: RwLock::new(HashMap::new()),
        }
    }

    /// 注册一组域名；任一域名冲突则整组回滚，避免半注册状态。
    pub fn register(&self, route: &DomainRoute, domains: &[String]) -> Result<(), String> {
        let mut routes = self
            .routes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut normalized: Vec<String> = Vec::with_capacity(domains.len());
        for domain in domains {
            let candidate = normalize_domain(domain)?;
            let key = route.kind.key(&candidate);
            if routes.contains_key(&key) {
                return Err(format!("domain {candidate} is already routed"));
            }
            for existing in routes.values() {
                if existing.kind == route.kind && overlaps(&candidate, &existing.domain) {
                    return Err(format!(
                        "domain {candidate} overlaps with {}",
                        existing.domain
                    ));
                }
            }
            for earlier in &normalized {
                if overlaps(&candidate, earlier.as_str()) {
                    return Err(format!("domain {candidate} overlaps with {earlier}"));
                }
            }
            normalized.push(candidate);
        }
        for domain in normalized {
            self.offline
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&route.kind.key(&domain));
            let mut clone = route.clone();
            clone.domain.clone_from(&domain);
            routes.insert(route.kind.key(&domain), Arc::new(clone));
        }
        Ok(())
    }

    /// 返回被移除的域名，供 ACME 释放对应状态机。
    pub fn unregister_proxy(
        &self,
        tenant_id: &str,
        client_id: &str,
        proxy_name: &str,
    ) -> Vec<String> {
        let mut routes = self
            .routes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut removed = Vec::new();
        routes.retain(|_, route| {
            let belongs = route.tenant_id == tenant_id
                && route.client_id == client_id
                && route.proxy_name == proxy_name;
            if belongs {
                removed.push(route.domain.clone());
            }
            !belongs
        });
        removed
    }

    /// 客户端离线且端口保留期内，记录域名，供 HTTP 入口返回明确的 503。
    pub fn mark_offline(&self, domains: &[String], hold: Duration) {
        let Some(until) = Instant::now().checked_add(hold) else {
            return;
        };
        let mut offline = self
            .offline
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for domain in domains {
            offline.insert(RouteKind::Http.key(domain), until);
            offline.insert(RouteKind::Https.key(domain), until);
        }
    }

    #[must_use]
    pub fn is_offline(&self, host: &str, kind: RouteKind) -> bool {
        let host = normalize_host(host);
        if host.is_empty() {
            return false;
        }
        let now = Instant::now();
        let mut offline = self
            .offline
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        offline.retain(|_, until| *until > now);
        if offline.contains_key(&kind.key(&host)) {
            return true;
        }
        let prefix = match kind {
            RouteKind::Http => "http:",
            RouteKind::Https => "https:",
        };
        for key in offline.keys() {
            let Some(domain) = key.strip_prefix(prefix) else {
                continue;
            };
            let Some(suffix) = domain.strip_prefix("*.") else {
                continue;
            };
            if host.len() > suffix.len() && host.ends_with(&format!(".{suffix}")) {
                return true;
            }
        }
        false
    }

    /// 精确匹配优先，其次取最长泛域名后缀。
    #[must_use]
    pub fn resolve(&self, host: &str, kind: RouteKind) -> Option<Arc<DomainRoute>> {
        let host = normalize_host(host);
        if host.is_empty() {
            return None;
        }
        let routes = self
            .routes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(route) = routes.get(&kind.key(&host)) {
            return Some(Arc::clone(route));
        }

        let mut best: Option<&Arc<DomainRoute>> = None;
        for route in routes.values() {
            if route.kind != kind {
                continue;
            }
            let Some(suffix) = route.domain.strip_prefix("*.") else {
                continue;
            };
            if host.len() > suffix.len()
                && host.ends_with(&format!(".{suffix}"))
                && best.is_none_or(|current| route.domain.len() > current.domain.len())
            {
                best = Some(route);
            }
        }
        best.cloned()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.routes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for DomainRouter {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_domain(domain: &str) -> Result<String, String> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() || domain.contains(char::is_whitespace) || domain.contains('/') {
        return Err(format!("invalid domain {domain:?}"));
    }
    if let Some(suffix) = domain.strip_prefix("*.") {
        if suffix.is_empty() || suffix.contains('*') || !suffix.contains('.') {
            return Err(format!("invalid wildcard domain {domain:?}"));
        }
        return Ok(format!("*.{suffix}"));
    }
    if domain.contains('*') || !domain.contains('.') {
        return Err(format!("invalid domain {domain:?}"));
    }
    Ok(domain)
}

fn normalize_host(host: &str) -> String {
    let mut host = host.trim().to_ascii_lowercase();
    if host.starts_with('[') {
        if let Some(end) = host.find(']') {
            host.truncate(end + 1);
        }
        return host;
    }
    if let Some((base, port)) = host.rsplit_once(':')
        && port.parse::<u16>().is_ok()
    {
        host = base.to_string();
    }
    host.trim_end_matches('.').to_string()
}

/// 精确域名与泛域名可以共存，解析时精确优先；只有两个泛域名互相覆盖时才冲突。
fn overlaps(candidate: &str, existing: &str) -> bool {
    let (Some(candidate), Some(existing)) =
        (candidate.strip_prefix("*."), existing.strip_prefix("*."))
    else {
        return false;
    };
    candidate.ends_with(&format!(".{existing}")) || existing.ends_with(&format!(".{candidate}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_domains_and_hosts() {
        assert_eq!(normalize_domain("Example.COM.").unwrap(), "example.com");
        assert_eq!(
            normalize_domain("*.Tunnel.Example.com").unwrap(),
            "*.tunnel.example.com"
        );
        assert_eq!(normalize_host("Example.com:8080"), "example.com");
        assert_eq!(normalize_host("[::1]:443"), "[::1]");
        assert!(normalize_domain("foo").is_err());
        assert!(normalize_domain("*").is_err());
    }

    #[test]
    fn wildcard_overlap_is_detected_but_exact_is_priority() {
        assert!(!overlaps("*.example.com", "api.example.com"));
        assert!(!overlaps("api.example.com", "*.example.com"));
        assert!(overlaps("*.a.example.com", "*.example.com"));
        assert!(!overlaps("api.other.com", "*.example.com"));
    }

    #[test]
    fn offline_domains_are_marked_temporarily() {
        let router = DomainRouter::new();
        router.mark_offline(
            &["app.example.com".to_string(), "*.example.com".to_string()],
            Duration::from_secs(60),
        );
        assert!(router.is_offline("app.example.com:80", RouteKind::Http));
        assert!(router.is_offline("api.example.com", RouteKind::Http));
        assert!(!router.is_offline("other.test", RouteKind::Http));

        router.mark_offline(&["gone.other.test".to_string()], Duration::ZERO);
        assert!(!router.is_offline("gone.other.test", RouteKind::Http));
    }
}
