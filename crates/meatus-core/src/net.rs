#![forbid(unsafe_code)]
//! SOCKS5 目标地址白名单。
//!
//! `allow_cidrs` 为空时只允许常见内网/回环/链路本地地址，避免把 SOCKS5
//! 变成一个无约束的公网出口；显式配置后以用户列表为准。

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
}
