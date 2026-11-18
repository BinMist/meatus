#![forbid(unsafe_code)]
//! 服务端 SOCKS5 入口：只支持 CONNECT，目标经过 CIDR 白名单后写入
//! `StreamHeader.dst_hint`，由 client 从内网侧发起连接。

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use anyhow::{Context, bail};
use meatus_core::{
    OpenHandle, RateLimiter, TargetPolicy, UsageRecord, copy_bidirectional_limited, write_frame,
};
use meatus_proto::v1::{StreamHeader, StreamKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::ServerState;
use crate::metrics::ProxyMetrics;

const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// 当前唯一的 SOCKS5 路由。M4 的单 client 场景；多租户路由留给后续按
/// SOCKS5 用户名或 tenant 选择。
#[derive(Clone)]
pub struct Socks5Route {
    pub proxy_name: String,
    pub tenant_id: String,
    pub client_id: String,
    pub run_id: String,
    pub open: OpenHandle,
    pub limiter: Arc<RateLimiter>,
    pub metrics: Arc<ProxyMetrics>,
    pub next_trace: Arc<AtomicU64>,
    pub policy: Arc<TargetPolicy>,
    pub permits: Arc<Semaphore>,
}

#[derive(Default)]
pub struct Socks5Registry {
    route: RwLock<Option<Socks5Route>>,
}

impl Socks5Registry {
    pub fn register(&self, route: Socks5Route) -> Result<(), String> {
        let mut slot = self
            .route
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_some() {
            return Err("another socks5 proxy is already registered".to_string());
        }
        *slot = Some(route);
        Ok(())
    }

    pub fn unregister(&self, proxy_name: &str, tenant_id: &str, client_id: &str, run_id: &str) {
        let mut slot = self
            .route
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.as_ref().is_some_and(|route| {
            route.proxy_name == proxy_name
                && route.tenant_id == tenant_id
                && route.client_id == client_id
                && route.run_id == run_id
        }) {
            *slot = None;
        }
    }

    fn current(&self) -> Option<Socks5Route> {
        self.route
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

pub async fn serve(
    listener: TcpListener,
    state: Arc<ServerState>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(err) => {
                        warn!(error = %err, "socks5 accept failed");
                        continue;
                    }
                };
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Err(err) = handle(stream, peer, state).await {
                        debug!(%peer, error = %err, "socks5 connection ended");
                    }
                });
            }
        }
    }
    info!("socks5 listener stopped");
    Ok(())
}

async fn handle(
    mut user: TcpStream,
    peer: SocketAddr,
    state: Arc<ServerState>,
) -> anyhow::Result<()> {
    user.set_nodelay(true).context("set TCP_NODELAY")?;
    let greeting = timeout(READ_TIMEOUT, read_greeting(&mut user))
        .await
        .context("socks5 greeting timed out")??;
    if !greeting {
        return Ok(());
    }

    let route = state.socks5.current();
    let Some(route) = route else {
        let _ = reply(&mut user, 0x01).await;
        bail!("no socks5 proxy registered");
    };

    let target = match timeout(READ_TIMEOUT, read_request(&mut user)).await {
        Ok(Ok(target)) => target,
        Ok(Err(err)) => {
            let _ = reply(&mut user, 0x07).await;
            return Err(err);
        }
        Err(_) => {
            let _ = reply(&mut user, 0x01).await;
            bail!("socks5 request timed out");
        }
    };

    if !route.policy.allows(target.ip()) {
        let _ = reply(&mut user, 0x02).await;
        warn!(proxy = %route.proxy_name, %target, "socks5 target denied by allow_cidrs");
        return Ok(());
    }

    let permit = match Arc::clone(&route.permits).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            let _ = reply(&mut user, 0x01).await;
            return Ok(());
        }
    };

    let mut tunnel = match timeout(OPEN_TIMEOUT, route.open.open()).await {
        Ok(Ok(tunnel)) => tunnel,
        Ok(Err(err)) => {
            let _ = reply(&mut user, 0x01).await;
            return Err(err).context("open socks5 tunnel stream");
        }
        Err(_) => {
            let _ = reply(&mut user, 0x01).await;
            bail!("timed out opening socks5 tunnel stream");
        }
    };

    let header = StreamHeader {
        proxy_name: route.proxy_name.clone(),
        kind: StreamKind::UserConn as i32,
        src_addr: peer.to_string(),
        dst_hint: target.to_string(),
        created_ms: now_ms(),
        trace_id: format!("{:016x}", route.next_trace.fetch_add(1, Ordering::Relaxed)),
        udp_session_id: 0,
    };
    write_frame(&mut tunnel, &header)
        .await
        .context("write socks5 stream header")?;
    reply(&mut user, 0x00).await?;

    route.metrics.connection_started();
    let result = copy_bidirectional_limited(&mut user, &mut tunnel, route.limiter.as_ref()).await;
    match &result {
        Ok((bytes_in, bytes_out)) => {
            route.metrics.add_bytes(*bytes_in, *bytes_out);
            state.usage.record(&UsageRecord {
                tenant_id: route.tenant_id.clone(),
                client_id: route.client_id.clone(),
                proxy_name: route.proxy_name.clone(),
                bytes_in: *bytes_in,
                bytes_out: *bytes_out,
                connections: 1,
                at: SystemTime::now(),
            });
        }
        Err(err) => debug!(proxy = %route.proxy_name, error = %err, "socks5 tunnel copy failed"),
    }
    route.metrics.connection_finished();
    drop(permit);
    result.context("socks5 bidirectional copy")?;
    Ok(())
}

async fn read_greeting(user: &mut TcpStream) -> anyhow::Result<bool> {
    let version = user.read_u8().await?;
    if version != 5 {
        bail!("unsupported socks version {version}");
    }
    let methods = user.read_u8().await? as usize;
    let mut buf = vec![0u8; methods];
    user.read_exact(&mut buf).await?;
    if !buf.contains(&0) {
        user.write_all(&[5, 0xff]).await?;
        return Ok(false);
    }
    user.write_all(&[5, 0]).await?;
    Ok(true)
}

async fn read_request(user: &mut TcpStream) -> anyhow::Result<SocketAddr> {
    let version = user.read_u8().await?;
    let command = user.read_u8().await?;
    let reserved = user.read_u8().await?;
    let atyp = user.read_u8().await?;
    if version != 5 || reserved != 0 {
        bail!("malformed socks5 request");
    }
    if command != 1 {
        bail!("only socks5 CONNECT is supported, got command {command}");
    }

    let ip = match atyp {
        1 => {
            let mut buf = [0u8; 4];
            user.read_exact(&mut buf).await?;
            IpAddr::from(buf)
        }
        4 => {
            let mut buf = [0u8; 16];
            user.read_exact(&mut buf).await?;
            IpAddr::from(buf)
        }
        3 => {
            let len = user.read_u8().await? as usize;
            let mut buf = vec![0u8; len];
            user.read_exact(&mut buf).await?;
            bail!(
                "socks5 domain targets are not supported: {:?}",
                String::from_utf8_lossy(&buf)
            );
        }
        other => bail!("unsupported socks5 address type {other}"),
    };
    let port = u16::from_be_bytes([user.read_u8().await?, user.read_u8().await?]);
    Ok(SocketAddr::new(ip, port))
}

async fn reply(user: &mut TcpStream, code: u8) -> anyhow::Result<()> {
    user.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    user.flush().await?;
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}
