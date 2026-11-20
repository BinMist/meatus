#![forbid(unsafe_code)]
//! QUIC 控制面监听；D1-02 只建立传输连接，应用层握手在 D1-04 接入。

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, anyhow};
use meatus_core::{ServerConfig, quic as core_quic};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::ServerState;

pub fn endpoint(config: &ServerConfig) -> anyhow::Result<quinn::Endpoint> {
    let cert = config
        .server
        .tls_cert
        .as_ref()
        .context("server.tls_cert is required for QUIC")?;
    let key = config
        .server
        .tls_key
        .as_ref()
        .context("server.tls_key is required for QUIC")?;
    let (certs, key) = crate::tls::load_der(cert, key)?;
    let bind = format!("{}:{}", config.server.bind_addr, config.server.control_port);
    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("parse QUIC bind {bind}"))?;
    let endpoint = core_quic::server_endpoint(addr, certs, key)
        .with_context(|| format!("bind QUIC {bind}"))?;
    let local = endpoint.local_addr().context("QUIC local_addr")?;
    info!(%local, "control listener ready: QUIC");
    Ok(endpoint)
}

pub async fn serve(
    endpoint: quinn::Endpoint,
    _state: Arc<ServerState>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                let peer = incoming.remote_address();
                tokio::spawn(async move {
                    match incoming.await {
                        Ok(connection) => {
                            info!(%peer, "QUIC connection established");
                            let _ = connection.closed().await;
                        }
                        Err(err) => warn!(%peer, error = %err, "QUIC handshake failed"),
                    }
                });
            }
        }
    }
    endpoint.close(0u32.into(), b"server shutdown");
    Err(anyhow!("QUIC listener stopped"))
}
