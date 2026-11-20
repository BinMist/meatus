#![forbid(unsafe_code)]
//! SOCKS5 目标地址白名单。
//!
//! `allow_cidrs` 为空时只允许常见内网/回环/链路本地地址，避免把 SOCKS5
//! 变成一个无约束的公网出口；显式配置后以用户列表为准。
//!
//! 域名目标还需要 `allow_domains` 显式放行；域名只做格式与后缀校验，
//! 真正的 DNS 解析和 CIDR 二次校验在 client 侧完成。

use std::net::IpAddr;

/// 一个 CIDR 网段。地址与前缀长度保持原始形式，判断时按同一地址族比较。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Network {
    addr: IpAddr,
    prefix: u8,
}

impl Network {
    fn contains(self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.prefix)
                };
                (u32::from(net) & mask) == (u32::from(ip) & mask)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - self.prefix)
                };
                (u128::from(net) & mask) == (u128::from(ip) & mask)
            }
            _ => false,
        }
    }
}

/// 目标地址策略。默认只允许私网、回环与链路本地地址。
#[derive(Debug, Clone)]
pub struct TargetPolicy {
    networks: Vec<Network>,
}

impl Default for TargetPolicy {
    fn default() -> Self {
        Self::from_allow_cidrs(&[]).expect("built-in CIDRs are valid")
    }
}

impl TargetPolicy {
    /// 解析配置中的 `allow_cidrs`。空列表使用安全的内网默认集合。
    pub fn from_allow_cidrs(cidrs: &[String]) -> Result<Self, String> {
        let values: Vec<&str> = if cidrs.is_empty() {
            vec![
                "10.0.0.0/8",
                "100.64.0.0/10",
                "127.0.0.0/8",
                "169.254.0.0/16",
                "172.16.0.0/12",
                "192.168.0.0/16",
                "::1/128",
                "fc00::/7",
                "fe80::/10",
            ]
        } else {
            cidrs
                .iter()
                .map(String::as_str)
                .filter(|value| !value.trim().is_empty())
                .collect()
        };

        if values.is_empty() {
            return Err("allow_cidrs must not be empty".to_string());
        }

        let mut networks = Vec::with_capacity(values.len());
        for value in values {
            networks.push(parse_network(value.trim())?);
        }
        Ok(Self { networks })
    }

    /// 判断目标 IP 是否命中白名单。
    #[must_use]
    pub fn allows(&self, ip: IpAddr) -> bool {
        self.networks.iter().any(|network| network.contains(ip))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.networks.is_empty()
    }
}

/// SOCKS5 域名目标白名单。
///
/// 支持精确域名与 `*.` 后缀；空列表表示不允许任何域名目标。
/// `*.example.com` 只匹配子域名，不匹配 `example.com` 本身。
#[derive(Debug, Clone, Default)]
pub struct DomainPolicy {
    patterns: Vec<DomainPattern>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DomainPattern {
    suffix: String,
    wildcard: bool,
}

impl DomainPolicy {
    pub fn from_allow_domains(domains: &[String]) -> Result<Self, String> {
        if domains.len() > 16 {
            return Err("allow_domains supports at most 16 entries".to_string());
        }
        let mut patterns = Vec::with_capacity(domains.len());
        for value in domains {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            patterns.push(parse_domain_pattern(value)?);
        }
        Ok(Self { patterns })
    }

    #[must_use]
    pub fn allows(&self, domain: &str) -> bool {
        let Ok(domain) = normalize_domain(domain) else {
            return false;
        };
        self.patterns.iter().any(|pattern| {
            if pattern.wildcard {
                domain
                    .strip_suffix(&pattern.suffix)
                    .is_some_and(|prefix| prefix.ends_with('.'))
                    && domain.len() > pattern.suffix.len()
            } else {
                domain == pattern.suffix
            }
        })
    }

    /// 允许客户端声明一个域名或通配域名。
    ///
    /// 通配声明（`*.example.com`）只有在白名单中存在**完全相同**的通配项时才放行，
    /// 不能借更宽或更窄的 `*.suffix` 扩大授权范围；精确域名仍按 [`Self::allows`] 匹配。
    #[must_use]
    pub fn allows_claimed_domain(&self, domain: &str) -> bool {
        match parse_domain_pattern(domain) {
            Ok(pattern) if pattern.wildcard => {
                self.patterns.iter().any(|allowed| allowed == &pattern)
            }
            _ => self.allows(domain),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

fn parse_domain_pattern(value: &str) -> Result<DomainPattern, String> {
    let wildcard = value.starts_with("*.");
    let raw = if wildcard { &value[2..] } else { value };
    let suffix = normalize_domain(raw)
        .map_err(|err| format!("invalid allow_domains entry {value:?}: {err}"))?;
    if wildcard && suffix.is_empty() {
        return Err("wildcard domain must include a suffix".to_string());
    }
    Ok(DomainPattern { suffix, wildcard })
}

/// 规范化并校验一个 DNS 主机名；返回小写、无尾点的形式。
pub fn normalize_domain(value: &str) -> Result<String, String> {
    let value = value.trim().trim_end_matches('.');
    if value.is_empty() {
        return Err("domain must not be empty".to_string());
    }
    if value.len() > 253 {
        return Err("domain is longer than 253 bytes".to_string());
    }
    if value.parse::<IpAddr>().is_ok() {
        return Err("domain must not be an IP literal".to_string());
    }
    if value.contains("://") || value.contains('/') || value.contains(char::is_whitespace) {
        return Err("domain contains invalid characters".to_string());
    }
    let value = value.to_ascii_lowercase();
    if value.starts_with('.') || value.ends_with('.') || value.contains("..") {
        return Err("domain has an empty label".to_string());
    }
    for label in value.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err("domain label length must be 1..63".to_string());
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("domain label must not start or end with '-'".to_string());
        }
        if !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err("domain label contains unsupported characters".to_string());
        }
    }
    Ok(value)
}

fn parse_network(value: &str) -> Result<Network, String> {
    let (addr, prefix) = match value.split_once('/') {
        Some((addr, prefix)) => (addr, Some(prefix)),
        None => (value, None),
    };
    let addr: IpAddr = addr
        .parse()
        .map_err(|err| format!("invalid CIDR address {value:?}: {err}"))?;
    let max_prefix = if addr.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        Some(prefix) => prefix
            .parse::<u8>()
            .map_err(|err| format!("invalid CIDR prefix {value:?}: {err}"))?,
        None => max_prefix,
    };
    if prefix > max_prefix {
        return Err(format!(
            "CIDR prefix {prefix} is too large for {value:?} (max {max_prefix})"
        ));
    }
    Ok(Network { addr, prefix })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn default_policy_allows_private_and_rejects_public() {
        let policy = TargetPolicy::default();
        assert!(policy.allows(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(policy.allows(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))));
        assert!(policy.allows(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(!policy.allows(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!policy.allows(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
    }

    #[test]
    fn explicit_policy_replaces_defaults() {
        let policy =
            TargetPolicy::from_allow_cidrs(&["203.0.113.0/24".to_string()]).expect("valid");
        assert!(policy.allows(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))));
        assert!(!policy.allows(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
    }

    #[test]
    fn invalid_cidr_is_rejected() {
        assert!(TargetPolicy::from_allow_cidrs(&["10.0.0.0/33".to_string()]).is_err());
        assert!(TargetPolicy::from_allow_cidrs(&["not-an-ip".to_string()]).is_err());
    }

    #[test]
    fn single_ip_and_zero_prefix_work() {
        let policy = TargetPolicy::from_allow_cidrs(&[
            " 192.0.2.10 ".to_string(),
            "198.51.100.0/24".to_string(),
        ])
        .expect("valid");
        assert!(policy.allows(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))));
        assert!(!policy.allows(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11))));
        assert!(policy.allows(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1))));
    }

    #[test]
    fn domain_policy_supports_exact_and_wildcard() {
        let policy = DomainPolicy::from_allow_domains(&[
            "db.internal.example".to_string(),
            "*.apps.example".to_string(),
        ])
        .expect("valid");
        assert!(policy.allows("db.internal.example"));
        assert!(policy.allows("a.apps.example"));
        assert!(policy.allows("A.APPS.EXAMPLE."));
        assert!(!policy.allows("apps.example"));
        assert!(!policy.allows("notapps.example"));
        assert!(!policy.allows("db.internal.example.evil"));
    }

    #[test]
    fn domain_policy_allows_only_equivalent_wildcard_claims() {
        let policy = DomainPolicy::from_allow_domains(&[
            "*.example.com".to_string(),
            "db.internal.example".to_string(),
        ])
        .expect("valid");

        assert!(policy.allows_claimed_domain("*.example.com"));
        assert!(policy.allows_claimed_domain("*.EXAMPLE.COM."));
        assert!(policy.allows_claimed_domain("a.example.com"));
        assert!(policy.allows_claimed_domain("db.internal.example"));
        assert!(!policy.allows_claimed_domain("*.other.example.com"));
        assert!(!policy.allows_claimed_domain("*.db.internal.example"));
        assert!(!policy.allows_claimed_domain("*.example.com.evil"));
    }

    #[test]
    fn empty_domain_policy_rejects_wildcard_claims() {
        let policy = DomainPolicy::from_allow_domains(&[]).expect("valid");
        assert!(!policy.allows_claimed_domain("*.example.com"));
        assert!(!policy.allows_claimed_domain("a.example.com"));
    }

    #[test]
    fn empty_domain_policy_rejects_everything() {
        let policy = DomainPolicy::from_allow_domains(&[]).expect("valid");
        assert!(policy.is_empty());
        assert!(!policy.allows("example.com"));
    }

    #[test]
    fn invalid_domains_are_rejected() {
        assert!(DomainPolicy::from_allow_domains(&["203.0.113.10".to_string()]).is_err());
        assert!(DomainPolicy::from_allow_domains(&["bad domain".to_string()]).is_err());
        assert!(DomainPolicy::from_allow_domains(&["-bad.example".to_string()]).is_err());
        assert!(DomainPolicy::from_allow_domains(&["*.example.com".to_string()]).is_ok());
    }
}
