//! MEATUS 协议版本与能力位协商。
//!
//! `Hello.protocol_version` 表示客户端支持的最高主版本；服务端在自身支持的
//! 区间内选择双方都支持的最高版本。`caps` 是大小写敏感的小写 token，未知
//! token 只记录、不拒绝握手。具体规则见 `docs/adr/0003-v2-protocol-version-capability-negotiation.md`。

use std::collections::HashSet;

/// 服务端支持的最低协议版本。
pub const MIN_PROTOCOL_VERSION: u32 = 1;

/// 当前协议主版本，与 `Hello.protocol_version` 对应。
pub const PROTOCOL_VERSION: u32 = 2;

/// v1 能力：TCP 代理。
pub const CAP_TCP: &str = "tcp";
/// v1 能力：UDP 代理。
pub const CAP_UDP: &str = "udp";
/// v1 能力：HTTP 代理。
pub const CAP_HTTP: &str = "http";
/// v1 能力：HTTPS 代理。
pub const CAP_HTTPS: &str = "https";
/// v1 能力：SOCKS5 代理。
pub const CAP_SOCKS5: &str = "socks5";
/// v1 能力：接受服务端诊断 `Probe`。
pub const CAP_PROBE: &str = "probe";
/// v2 能力：数据流携带 `trace_id` / `connection_id` 元数据。
pub const CAP_STREAM_META: &str = "stream_meta";
/// v2 能力：优雅 drain。
pub const CAP_DRAIN: &str = "drain";
/// v2 能力：连接池。
pub const CAP_CONN_POOL: &str = "conn_pool";
/// v2 能力：QUIC 传输。
pub const CAP_QUIC: &str = "quic";
/// v2 能力：多入口地址。
pub const CAP_MULTI_ADDR: &str = "multi_addr";
/// v2 能力：IPv6 监听与拨号。
pub const CAP_IPV6: &str = "ipv6";

/// 协议认可的能力位定义。`since` 是首次可用的协议主版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    pub name: &'static str,
    pub since: u32,
    pub description: &'static str,
}

/// 全部已知能力位。新增能力只追加，不复用旧名字。
pub const CAPABILITIES: &[Capability] = &[
    Capability {
        name: CAP_TCP,
        since: 1,
        description: "TCP proxy",
    },
    Capability {
        name: CAP_UDP,
        since: 1,
        description: "UDP proxy",
    },
    Capability {
        name: CAP_HTTP,
        since: 1,
        description: "HTTP proxy",
    },
    Capability {
        name: CAP_HTTPS,
        since: 1,
        description: "HTTPS proxy",
    },
    Capability {
        name: CAP_SOCKS5,
        since: 1,
        description: "SOCKS5 proxy",
    },
    Capability {
        name: CAP_PROBE,
        since: 1,
        description: "Accept server-side probe requests",
    },
    Capability {
        name: CAP_STREAM_META,
        since: 2,
        description: "Stream metadata with trace_id and connection_id",
    },
    Capability {
        name: CAP_DRAIN,
        since: 2,
        description: "Graceful drain",
    },
    Capability {
        name: CAP_CONN_POOL,
        since: 2,
        description: "Client connection pool",
    },
    Capability {
        name: CAP_QUIC,
        since: 2,
        description: "QUIC transport",
    },
    Capability {
        name: CAP_MULTI_ADDR,
        since: 2,
        description: "Multiple server addresses",
    },
    Capability {
        name: CAP_IPV6,
        since: 2,
        description: "IPv6 listen and dial",
    },
];

/// v1 客户端默认声明的能力：只包含已经实现的老能力。
pub const DEFAULT_CLIENT_CAPS: &[&str] =
    &[CAP_TCP, CAP_UDP, CAP_HTTP, CAP_HTTPS, CAP_SOCKS5, CAP_PROBE];

/// 协商结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiated {
    /// 双方共同选定的协议主版本。
    pub version: u32,
    /// 被接受的能力位，保持客户端声明顺序并去重。
    pub caps: Vec<String>,
    /// 未知或当前版本不支持的能力位，只用于日志。
    pub ignored_caps: Vec<String>,
}

/// 客户端版本落在服务端支持区间之外。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("client protocol {client} outside server range {server_min}-{server_max}")]
pub struct UnsupportedProtocolVersion {
    pub client: u32,
    pub server_min: u32,
    pub server_max: u32,
}

/// 按当前服务端版本协商。
pub fn negotiate(
    client_version: u32,
    client_caps: &[String],
) -> Result<Negotiated, UnsupportedProtocolVersion> {
    negotiate_with(
        client_version,
        client_caps,
        MIN_PROTOCOL_VERSION,
        PROTOCOL_VERSION,
    )
}

/// 按显式服务端区间协商，便于测试和未来版本组合。
pub fn negotiate_with(
    client_version: u32,
    client_caps: &[String],
    server_min: u32,
    server_max: u32,
) -> Result<Negotiated, UnsupportedProtocolVersion> {
    if server_min > server_max || client_version < server_min || client_version > server_max {
        return Err(UnsupportedProtocolVersion {
            client: client_version,
            server_min,
            server_max,
        });
    }

    // 客户端声明的是最高支持版本；服务端在交集内取最高。
    let version = client_version.min(server_max);
    let mut seen = HashSet::new();
    let mut caps = Vec::new();
    let mut ignored_caps = Vec::new();

    for raw in client_caps {
        let name = raw.trim();
        if name.is_empty() || !seen.insert(name) {
            continue;
        }
        match capability(name) {
            Some(spec) if spec.since <= version => caps.push(name.to_string()),
            _ => ignored_caps.push(name.to_string()),
        }
    }

    Ok(Negotiated {
        version,
        caps,
        ignored_caps,
    })
}

/// 查找已知能力位。
#[must_use]
pub fn capability(name: &str) -> Option<&'static Capability> {
    CAPABILITIES.iter().find(|spec| spec.name == name)
}

/// 当前协议版本可用的能力位。
pub fn capabilities_at(version: u32) -> impl Iterator<Item = &'static Capability> {
    CAPABILITIES
        .iter()
        .filter(move |spec| spec.since <= version)
}

/// 客户端默认声明的能力位。
#[must_use]
pub fn default_client_caps() -> Vec<String> {
    DEFAULT_CLIENT_CAPS
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn v1_client_selects_v1_and_drops_v2_caps() {
        let result = negotiate(
            1,
            &caps(&[CAP_TCP, CAP_STREAM_META, "vendor_unknown", CAP_PROBE]),
        )
        .expect("v1 client is supported");

        assert_eq!(result.version, 1);
        assert_eq!(result.caps, caps(&[CAP_TCP, CAP_PROBE]));
        assert_eq!(
            result.ignored_caps,
            caps(&[CAP_STREAM_META, "vendor_unknown"])
        );
    }

    #[test]
    fn v2_client_selects_v2_and_keeps_known_caps() {
        let result = negotiate(
            2,
            &caps(&[
                CAP_TCP,
                CAP_STREAM_META,
                CAP_DRAIN,
                CAP_CONN_POOL,
                CAP_QUIC,
                CAP_MULTI_ADDR,
                CAP_IPV6,
                CAP_PROBE,
            ]),
        )
        .expect("v2 client is supported");

        assert_eq!(result.version, 2);
        assert_eq!(
            result.caps,
            caps(&[
                CAP_TCP,
                CAP_STREAM_META,
                CAP_DRAIN,
                CAP_CONN_POOL,
                CAP_QUIC,
                CAP_MULTI_ADDR,
                CAP_IPV6,
                CAP_PROBE,
            ])
        );
        assert!(result.ignored_caps.is_empty());
    }

    #[test]
    fn v1_server_rejects_v2_client() {
        let err =
            negotiate_with(2, &caps(&[CAP_TCP]), 1, 1).expect_err("1.3 server only speaks v1");
        assert_eq!(
            err,
            UnsupportedProtocolVersion {
                client: 2,
                server_min: 1,
                server_max: 1,
            }
        );
    }

    #[test]
    fn unknown_versions_are_rejected() {
        for version in [0, 3] {
            let err = negotiate(version, &[]).expect_err("out-of-range version");
            assert_eq!(err.client, version);
            assert_eq!(err.server_min, MIN_PROTOCOL_VERSION);
            assert_eq!(err.server_max, PROTOCOL_VERSION);
        }
    }

    #[test]
    fn unknown_caps_are_ignored_without_failing() {
        let result = negotiate(
            2,
            &caps(&["", "  ", "TCP", "not_a_real_cap", " future_cap "]),
        )
        .expect("unknown caps must not fail negotiation");

        assert_eq!(result.version, 2);
        assert!(result.caps.is_empty());
        // 空白项被丢弃；大小写不同和未知 token 进入 ignored。
        assert_eq!(
            result.ignored_caps,
            caps(&["TCP", "not_a_real_cap", "future_cap"])
        );
    }

    #[test]
    fn duplicate_and_padded_caps_are_normalized() {
        let result = negotiate(1, &caps(&[" tcp ", CAP_TCP, "probe", CAP_PROBE, "tcp"]))
            .expect("duplicates are harmless");

        assert_eq!(result.caps, caps(&[CAP_TCP, CAP_PROBE]));
    }

    #[test]
    fn v2_caps_are_not_enabled_for_v1_session() {
        for name in [
            CAP_STREAM_META,
            CAP_DRAIN,
            CAP_CONN_POOL,
            CAP_QUIC,
            CAP_MULTI_ADDR,
            CAP_IPV6,
        ] {
            let result = negotiate(1, &caps(&[name])).expect("v1 supported");
            assert_eq!(result.version, 1);
            assert!(result.caps.is_empty(), "{name} must not be enabled in v1");
            assert_eq!(result.ignored_caps, caps(&[name]));
        }
    }

    #[test]
    fn empty_cap_list_is_valid() {
        let result = negotiate(1, &[]).expect("caps are optional");
        assert_eq!(result.version, 1);
        assert!(result.caps.is_empty());
        assert!(result.ignored_caps.is_empty());
    }

    #[test]
    fn capability_registry_matches_version_gate() {
        assert!(capabilities_at(1).all(|spec| spec.since == 1));
        assert!(capabilities_at(2).any(|spec| spec.name == CAP_STREAM_META));
        assert_eq!(capability(CAP_TCP).map(|spec| spec.since), Some(1));
        assert_eq!(capability(CAP_QUIC).map(|spec| spec.since), Some(2));
        assert!(capability("TCP").is_none());
    }
}
