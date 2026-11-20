#![forbid(unsafe_code)]
//! QUIC 控制面拨号；TLS 校验复用 `tls::client_config`。

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, anyhow};
use meatus_core::ClientConfig;
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig as QuinnClientConfig, Connection, Endpoint};
use tokio::time::timeout;

const QUIC_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 保持端点与连接存活；丢弃后连接关闭。
pub struct QuicClient {
    _endpoint: Endpoint,
    connection: Connection,
}

impl QuicClient {
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// 打开一条双向流；v2 的 yamux 控制面承载在这条流上。
    pub async fn open_bi(&self) -> anyhow::Result<meatus_core::QuicStream> {
        let (send, recv) = self
            .connection
            .open_bi()
            .await
            .context("open QUIC bidirectional stream")?;
        Ok(meatus_core::QuicStream::new(send, recv))
    }
}

/// 拨号 `server_addr` 的 UDP 端口，SNI 与证书规则与 TCP 一致。
pub async fn connect(config: &ClientConfig, server_addr: &str) -> anyhow::Result<QuicClient> {
    let (tls, server_name) = super::tls::client_config(config)?;
    let tls = QuicClientConfig::try_from((*tls).clone())
        .map_err(|err| anyhow!("build quic client config: {err}"))?;
    let mut client = QuinnClientConfig::new(std::sync::Arc::new(tls));
    client.transport_config(std::sync::Arc::new(quinn::TransportConfig::default()));
    let endpoint = Endpoint::client(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
        .context("bind quic client endpoint")?;
    let addr = tokio::net::lookup_host(server_addr)
        .await
        .with_context(|| format!("resolve quic target {server_addr}"))?
        .next()
        .ok_or_else(|| anyhow!("no address for quic target {server_addr}"))?;
    let connecting = endpoint
        .connect_with(client, addr, &server_name.to_str())
        .with_context(|| format!("parse quic target {server_addr}"))?;
    let connection = timeout(QUIC_CONNECT_TIMEOUT, connecting)
        .await
        .with_context(|| format!("quic connect {server_addr} timed out"))?
        .context("quic handshake")?;
    Ok(QuicClient {
        _endpoint: endpoint,
        connection,
    })
}
