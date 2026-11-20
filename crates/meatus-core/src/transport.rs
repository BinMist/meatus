#![forbid(unsafe_code)]
//! 传输层抽象与连接池键。
//!
//! 上层只依赖 [`Connection`]、[`MultiplexedStream`] 和 [`PoolKey`]，不直接
//! 出现 yamux / quinn 类型。当前实现仍是 TCP + TLS + yamux；QUIC 在 D1 接入
//! 时复用同一组公开类型。

use std::fmt;

use futures_io::{AsyncRead, AsyncWrite};

use crate::multiplex;

pub use crate::multiplex::{Inbound, MultiplexError, MultiplexedStream, OpenHandle};

/// 传输连接在握手阶段扮演的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Client,
    Server,
}

/// 已建立的多路复用连接：可主动开流，也可接收对端开流。
pub struct Connection {
    open: OpenHandle,
    inbound: Inbound,
}

impl Connection {
    /// 打开一条新的数据流。
    pub async fn open(&self) -> Result<MultiplexedStream, MultiplexError> {
        self.open.open().await
    }

    /// 可克隆的开流句柄；用于会话主体与本地下行任务共享同一连接。
    #[must_use]
    pub fn open_handle(&self) -> OpenHandle {
        self.open.clone()
    }

    /// 接收对端打开的数据流；连接关闭时返回 `None`。
    pub async fn accept(&mut self) -> Option<MultiplexedStream> {
        self.inbound.recv().await
    }
}

/// 把一个已就绪的字节流接入多路复用层。
///
/// `io` 由调用方负责 TLS / QUIC 握手；本函数不关心底层实现。
#[must_use]
pub fn attach<T>(io: T, role: Role) -> Connection
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mode = match role {
        Role::Client => yamux::Mode::Client,
        Role::Server => yamux::Mode::Server,
    };
    let (open, inbound) = multiplex::spawn(io, mode);
    Connection { open, inbound }
}

/// 连接池键：`proxy/host`。
///
/// `proxy` 保持配置中的大小写；`host` 去空白并转小写，域名大小写不敏感。
/// 代理名不允许包含 `/`，因此键不会产生歧义。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PoolKey {
    proxy: String,
    host: String,
}

impl PoolKey {
    /// 构造并规范化池键。
    pub fn new(proxy: &str, host: &str) -> Result<Self, PoolKeyError> {
        let proxy = proxy.trim();
        if proxy.is_empty() {
            return Err(PoolKeyError::EmptyProxy);
        }
        if proxy.contains('/') {
            return Err(PoolKeyError::ProxySeparator);
        }
        let host = host.trim();
        if host.is_empty() {
            return Err(PoolKeyError::EmptyHost);
        }
        if host.contains('/') {
            return Err(PoolKeyError::HostSeparator);
        }
        Ok(Self {
            proxy: proxy.to_string(),
            host: host.to_ascii_lowercase(),
        })
    }

    #[must_use]
    pub fn proxy(&self) -> &str {
        &self.proxy
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }
}

impl fmt::Display for PoolKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.proxy, self.host)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PoolKeyError {
    #[error("pool key proxy must not be empty")]
    EmptyProxy,
    #[error("pool key proxy must not contain '/'")]
    ProxySeparator,
    #[error("pool key host must not be empty")]
    EmptyHost,
    #[error("pool key host must not contain '/'")]
    HostSeparator,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_proxy_slash_normalized_host() {
        let key = PoolKey::new("web", "  Example.COM:443  ").expect("valid key");
        assert_eq!(key.to_string(), "web/example.com:443");
        assert_eq!(key.proxy(), "web");
        assert_eq!(key.host(), "example.com:443");
    }

    #[test]
    fn proxy_case_is_preserved_but_host_is_not() {
        let key = PoolKey::new("Web-A", "API.Example.COM:8443").expect("valid key");
        assert_eq!(key.to_string(), "Web-A/api.example.com:8443");
    }

    #[test]
    fn same_proxy_different_hosts_are_distinct() {
        let a = PoolKey::new("socks", "a.example.com:80").expect("a");
        let b = PoolKey::new("socks", "b.example.com:80").expect("b");
        assert_ne!(a, b);
    }

    #[test]
    fn invalid_components_are_rejected() {
        assert_eq!(PoolKey::new(" ", "host"), Err(PoolKeyError::EmptyProxy));
        assert_eq!(
            PoolKey::new("web/api", "host"),
            Err(PoolKeyError::ProxySeparator)
        );
        assert_eq!(PoolKey::new("web", " "), Err(PoolKeyError::EmptyHost));
        assert_eq!(
            PoolKey::new("web", "host/path"),
            Err(PoolKeyError::HostSeparator)
        );
    }
}
