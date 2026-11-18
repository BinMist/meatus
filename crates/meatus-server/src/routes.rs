#![forbid(unsafe_code)]
//! HTTP / HTTPS 域名路由表。
//!
//! 域名全局唯一（按入口类型区分），精确域名优先于泛域名；泛域名只接受
//! `*.example.com` 形式，匹配任意层级的子域名。

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

use meatus_core::{OpenHandle, RateLimiter};
use tokio::sync::Semaphore;

use crate::metrics::ProxyMetrics;

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
    pub metrics: Arc<ProxyMetrics>,
    pub next_trace: Arc<AtomicU64>,
    pub connections: Arc<Semaphore>,
}

pub struct DomainRouter {
    routes: RwLock<HashMap<String, Arc<DomainRoute>>>,
}

impl DomainRouter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            routes: RwLock::new(HashMap::new()),
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
}
