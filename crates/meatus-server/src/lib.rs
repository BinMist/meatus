#![forbid(unsafe_code)]
//! relay 侧运行时：TLS 1.3 控制面、yamux 复用与公网端口映射。

mod acme;
mod admin;
mod admin_bus;
pub mod audit;
mod control;
mod drain;
mod handshake;
mod http;
pub mod logbuf;
mod metrics;
mod pool;
mod quic;
pub mod quota;
mod relay;
mod relay_cert;
mod relay_health;
mod relay_peer;
mod routes;
mod runtime;
mod socks5;
mod status;
mod tls;

pub use relay::RelayContext;
pub use runtime::{ConfigReloader, LogReloadHandle, ReloadSnapshot, RuntimeConfig};

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use handshake::{HANDSHAKE_FAIL_DELAY, HandshakeLimiter};
use logbuf::LogBuffer;
use meatus_core::{
    AllowAllQuota, AuthProvider, ConfigError, LocalPortAllocator, LocalTokenAuth, NameAllocator,
    NullUsageSink, QuotaPolicy, ServerConfig, UsageSink, transport,
};
use metrics::Metrics;
use pool::SessionPools;
use routes::DomainRouter;
use socks5::Socks5Registry;
use status::StatusRegistry;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_util::compat::TokioAsyncReadCompatExt;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CONTROL_STREAM_TIMEOUT: Duration = Duration::from_secs(10);

pub struct ServerState {
    pub config: ServerConfig,
    /// 启动时使用的配置文件路径；面板在线写回需要它。
    pub config_path: Option<PathBuf>,
    pub allocator: Arc<dyn NameAllocator>,
    pub quota: Arc<dyn QuotaPolicy>,
    pub quotas: Arc<quota::MonthlyQuota>,
    pub usage: Arc<dyn UsageSink>,
    pub metrics: Arc<Metrics>,
    pub(crate) handshake_limiter: Arc<HandshakeLimiter>,
    pub domains: Arc<DomainRouter>,
    pub(crate) status: Arc<StatusRegistry>,
    pub(crate) socks5: Arc<Socks5Registry>,
    pub(crate) pools: Arc<SessionPools>,
    /// 验签后的 relay 组成员上下文；standalone 为 `None`。
    pub relay: Option<Arc<RelayContext>>,
    /// secondary 只读证书库；primary 或 standalone 为 `None`。
    pub(crate) relay_certs: Option<Arc<relay_cert::RelayCertStore>>,
    /// relay peer HMAC 密钥；standalone 或 cert_sync = off 时为 `None`。
    pub(crate) peer_auth: Option<Arc<relay_peer::PeerAuth>>,
    pub acme: Option<Arc<acme::AcmeManager>>,
    pub logs: Arc<LogBuffer>,
    pub audit: Arc<audit::AuditLog>,
    pub(crate) admin_bus: Arc<admin_bus::AdminBus>,
    /// 当前生效的配置与认证；SIGHUP / 面板重载共用。
    pub runtime: Arc<RuntimeConfig>,
    /// 配置热加载器；没有配置文件路径时为 `None`。
    pub reloader: Option<Arc<ConfigReloader>>,
    /// CLI 注入的日志级别热加载句柄。
    pub log_reload: Option<LogReloadHandle>,
    /// 优雅 drain 状态，进程信号与管理面共享。
    pub drain: drain::DrainState,
}

impl ServerState {
    pub fn new(config: ServerConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        let bundle = meatus_core::relay_bundle::verify_for_config(&config)
            .map_err(|err| ConfigError::Invalid(format!("relay bundle: {err}")))?;
        let relay = relay::build_context(&config, bundle)?;

        let auth: Arc<dyn AuthProvider> = Arc::new(LocalTokenAuth::from_config(&config)?);
        let allocator = LocalPortAllocator::new(config.ports.range)
            .map_err(|err| ConfigError::Invalid(err.to_string()))?;
        let acme = if config.acme.enabled {
            Some(Arc::new(acme::AcmeManager::new(&config).map_err(
                |err| ConfigError::Invalid(format!("acme init: {err}")),
            )?))
        } else {
            None
        };

        let runtime = RuntimeConfig::new(config.clone(), Arc::clone(&auth), "startup");
        let metrics = Arc::new(Metrics::new());
        if let Some(context) = relay.as_ref() {
            let section = config
                .relay
                .as_ref()
                .expect("relay context requires [relay]");
            metrics.configure_relay(
                context.id(),
                context.group(),
                context.role().as_str(),
                context.epoch(),
                section.cert_sync.as_str(),
                context.config_hash(),
                section.rotation_fail_threshold,
                section.rotation_recover_threshold,
                context
                    .peers()
                    .map(|peer| metrics::RelayPeerSeed {
                        id: peer.id.clone(),
                        role: peer.role.as_str().to_string(),
                        peer_addr: peer.peer_addr.clone(),
                    })
                    .collect(),
            );
        }
        let pools = Arc::new(SessionPools::with_stats(metrics.pool_stats()));

        Ok(Self {
            config,
            config_path: None,
            allocator: Arc::new(allocator),
            quota: Arc::new(AllowAllQuota),
            quotas: Arc::new(quota::MonthlyQuota::default()),
            usage: Arc::new(NullUsageSink),
            metrics,
            handshake_limiter: Arc::new(HandshakeLimiter::default()),
            domains: Arc::new(DomainRouter::new()),
            status: Arc::new(StatusRegistry::default()),
            socks5: Arc::new(Socks5Registry::default()),
            pools,
            relay,
            relay_certs: None,
            peer_auth: None,
            acme,
            logs: Arc::new(LogBuffer::default()),
            audit: Arc::new(audit::AuditLog::default()),
            admin_bus: Arc::new(admin_bus::AdminBus::default()),
            runtime,
            reloader: None,
            log_reload: None,
            drain: drain::DrainState::default(),
        })
    }

    /// HTTPS 终结用的 TLS 配置：primary 用 ACME resolver，secondary 用同步证书库。
    #[must_use]
    pub(crate) fn https_tls_config(&self) -> Option<Arc<rustls::ServerConfig>> {
        if let Some(acme) = &self.acme {
            return Some(acme.tls_config());
        }
        self.relay_certs.as_ref().map(|store| store.tls_config())
    }

    /// 记录启动配置文件路径，并建立 SIGHUP / 面板共用的热加载器。
    #[must_use]
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let format = meatus_core::ConfigFormat::from_path(&path);
        self.reloader = Some(ConfigReloader::new(
            path.clone(),
            format,
            Arc::clone(&self.runtime),
            self.log_reload.clone(),
        ));
        self.config_path = Some(path.clone());
        let audit_dir = self
            .config
            .audit
            .dir
            .clone()
            .or_else(|| path.parent().map(|parent| parent.join("audit")));
        self.audit = Arc::new(audit::AuditLog::new(audit_dir));
        self
    }

    /// 注入日志级别热加载句柄；必须在 `with_config_path` 之前调用。
    pub fn set_log_reload(&mut self, handle: Option<LogReloadHandle>) {
        self.log_reload = handle;
    }

    /// 启动时会尝试占用的监听地址，`meatus check --dry-run` 也用它。
    #[must_use]
    pub fn planned_listeners(&self) -> Vec<String> {
        let bind = meatus_core::config::host_port(
            &self.config.server.bind_addr,
            self.config.server.control_port,
        );
        let mut listeners = vec![format!("control tcp://{bind}")];
        if self.config.server.quic {
            listeners.push(format!("control udp://{bind}"));
        }

        if self.config.http.enabled {
            listeners.push(format!("http   tcp://{}", self.config.http.bind));
            listeners.push(format!("https  tcp://{}", self.config.http.https_bind));
        }
        if self.config.socks5.enabled {
            listeners.push(format!("socks5 tcp://{}", self.config.socks5.bind));
        }
        let scheme = if self.config.admin.tls.enabled {
            "https"
        } else {
            "http"
        };
        listeners.push(format!("admin  {scheme}://{}", self.config.admin.bind));
        listeners.push(format!(
            "metrics  {scheme}://{}/metrics",
            self.config.admin.bind
        ));
        if let Some(relay) = self.config.relay.as_ref() {
            listeners.push(format!("relay health tcp://{}", relay.health_bind));
        }

        listeners
    }
}

pub async fn run(state: ServerState) -> anyhow::Result<()> {
    const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let drain = state.drain.clone();
    let metrics = Arc::clone(&state.metrics);
    let drain_grace = Duration::from_secs(state.config.server.drain_grace_s);

    let run = run_until(state, run_cancel);
    tokio::pin!(run);

    tokio::select! {
        result = &mut run => result,
        _ = shutdown_signal() => {
            // 先摘 readiness，给外部 LB 留出摘后端的时间；期间仍接受新连接。
            drain.pre_drain();
            metrics.set_drain_phase(3);
            info!(
                grace_s = drain_grace.as_secs(),
                "shutdown requested; readiness withdrawn for load balancer grace period"
            );
            if !drain_grace.is_zero() {
                tokio::time::sleep(drain_grace).await;
            }
            drain.begin();
            metrics.set_drain_phase(1);
            info!("drain grace elapsed; draining existing streams");
            let deadline = tokio::time::Instant::now() + DRAIN_TIMEOUT;
            while metrics.data_active_total() > 0 && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if metrics.data_active_total() > 0 {
                warn!(
                    active = metrics.data_active_total(),
                    "drain timeout; forcing shutdown"
                );
            }
            drain.finish();
            metrics.set_drain_phase(2);
            cancel.cancel();
            run.await
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            warn!(error = %err, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                warn!(error = %err, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// 可在测试中通过 `CancellationToken` 停止的服务端入口。
pub async fn run_until(mut state: ServerState, cancel: CancellationToken) -> anyhow::Result<()> {
    state.config.validate_runtime()?;
    let acceptor = TlsAcceptor::from(tls::load(&state.config)?);

    let mut peer_listener = None;
    if let Some(relay) = state.relay.as_ref() {
        if relay.role() == meatus_core::RelayRole::Secondary {
            state.relay_certs = Some(Arc::new(relay_cert::RelayCertStore::new(
                &state.config.acme.cert_dir,
            )?));
            info!(
                dir = %state.config.acme.cert_dir.display(),
                "secondary relay cert store ready"
            );
        }
        if state
            .config
            .relay
            .as_ref()
            .is_some_and(|section| section.cert_sync != meatus_core::RelayCertSync::Off)
        {
            let auth = relay_peer::PeerAuth::load(
                &state
                    .config
                    .relay
                    .as_ref()
                    .expect("relay config exists")
                    .peer_secret_file,
            )?;
            state.peer_auth = Some(auth);
        }
        let health_bind = state
            .config
            .relay
            .as_ref()
            .expect("relay config exists")
            .health_bind
            .clone();
        let listener = TcpListener::bind(&health_bind)
            .await
            .with_context(|| format!("bind relay health listener {health_bind}"))?;
        let local = listener.local_addr().context("relay health local_addr")?;
        info!(%local, "relay peer listener ready: health");
        peer_listener = Some(listener);
    }

    let bind = meatus_core::config::host_port(
        &state.config.server.bind_addr,
        state.config.server.control_port,
    );
    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind control listener {bind}"))?;
    let local = listener.local_addr().context("control local_addr")?;
    info!(%local, "control listener ready: TLS + yamux");
    let quic_endpoint = if state.config.server.quic {
        Some(quic::endpoint(&state.config)?)
    } else {
        None
    };

    let admin_listener = TcpListener::bind(&state.config.admin.bind)
        .await
        .with_context(|| format!("bind admin listener {}", state.config.admin.bind))?;
    let admin_local = admin_listener.local_addr().context("admin local_addr")?;
    let admin_scheme = if state.config.admin.tls.enabled {
        "https"
    } else {
        "http"
    };
    info!(%admin_local, scheme = admin_scheme, "admin listener ready: panel + /metrics /healthz /api/v1/*");
    if let Some(relay) = &state.relay {
        info!(
            relay_id = %relay.id(),
            group = %relay.group(),
            role = relay.role().as_str(),
            epoch = relay.epoch(),
            peers = relay.peers().count(),
            "relay group member runtime ready"
        );
    }
    let config_path = state.config_path.clone();
    let state = Arc::new(state);
    let admin_state = admin::AdminState::new(
        Arc::clone(&state.metrics),
        state.drain.clone(),
        Arc::clone(&state.status),
        Arc::clone(&state.quotas),
        Arc::clone(&state.audit),
        Arc::new(state.config.clone()),
        Arc::clone(&state.runtime),
        Arc::clone(&state.logs),
        Arc::clone(&state.admin_bus),
        state.acme.clone(),
        state.reloader.clone(),
    )
    .with_config_path(config_path);
    let admin_task = tokio::spawn(admin::serve(
        admin_listener,
        admin_state,
        cancel.child_token(),
    ));

    let mut data_tasks = Vec::new();
    if let Some(peer_listener) = peer_listener {
        let peer_state = Arc::clone(&state);
        let peer_cancel = cancel.child_token();
        data_tasks.push(tokio::spawn(async move {
            if let Err(err) = relay_peer::serve(peer_listener, peer_state, peer_cancel).await {
                warn!(error = %err, "relay peer listener stopped");
            }
        }));
        let sync_state = Arc::clone(&state);
        let sync_cancel = cancel.child_token();
        data_tasks.push(tokio::spawn(async move {
            if let Err(err) = relay_peer::run_sync(sync_state, sync_cancel).await {
                warn!(error = %err, "relay cert sync stopped");
            }
        }));
        let health_state = Arc::clone(&state);
        let health_cancel = cancel.child_token();
        data_tasks.push(tokio::spawn(async move {
            if let Err(err) = relay_health::run(health_state, health_cancel).await {
                warn!(error = %err, "relay health probe stopped");
            }
        }));
    }
    if let Some(endpoint) = quic_endpoint {
        let quic_state = Arc::clone(&state);
        let quic_cancel = cancel.child_token();
        data_tasks.push(tokio::spawn(async move {
            if let Err(err) = quic::serve(endpoint, quic_state, quic_cancel).await {
                warn!(error = %err, "QUIC listener stopped");
            }
        }));
    }
    #[cfg(unix)]
    if let Some(reloader) = state.reloader.clone() {
        let reload_cancel = cancel.child_token();
        data_tasks.push(tokio::spawn(async move {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
                Ok(mut signal) => {
                    info!("SIGHUP hot reload listening");
                    loop {
                        tokio::select! {
                            _ = reload_cancel.cancelled() => break,
                            _ = signal.recv() => {
                                match reloader.reload() {
                                    Ok(snapshot) => info!(
                                        revision = %snapshot.revision,
                                        reload_count = snapshot.reload_count,
                                        restart_required = ?snapshot.restart_required,
                                        "SIGHUP reload applied"
                                    ),
                                    Err(err) => warn!(error = %err, "SIGHUP reload rejected; keep old config"),
                                }
                            }
                        }
                    }
                }
                Err(err) => warn!(error = %err, "cannot listen for SIGHUP"),
            }
        }));
    }

    if state.config.http.enabled {
        let http_listener = TcpListener::bind(&state.config.http.bind)
            .await
            .with_context(|| format!("bind http listener {}", state.config.http.bind))?;
        let http_local = http_listener.local_addr().context("http local_addr")?;
        info!(%http_local, "http listener ready: Host routing");
        data_tasks.push(tokio::spawn(http::serve_http(
            http_listener,
            Arc::clone(&state),
            cancel.child_token(),
        )));

        let https_listener = TcpListener::bind(&state.config.http.https_bind)
            .await
            .with_context(|| format!("bind https listener {}", state.config.http.https_bind))?;
        let https_local = https_listener.local_addr().context("https local_addr")?;
        info!(%https_local, "https listener ready: SNI passthrough");
        data_tasks.push(tokio::spawn(http::serve_https(
            https_listener,
            Arc::clone(&state),
            cancel.child_token(),
        )));
    }

    if state.config.socks5.enabled {
        let socks_bind = state.config.socks5.bind.clone();
        let socks_listener = TcpListener::bind(&socks_bind)
            .await
            .with_context(|| format!("bind socks5 listener {socks_bind}"))?;
        let socks_local = socks_listener.local_addr().context("socks5 local_addr")?;
        info!(%socks_local, "socks5 listener ready: CONNECT only");
        let socks_state = Arc::clone(&state);
        let socks_cancel = cancel.child_token();
        data_tasks.push(tokio::spawn(async move {
            if let Err(err) = socks5::serve(socks_listener, socks_state, socks_cancel).await {
                warn!(error = %err, "socks5 listener stopped");
            }
        }));
    }

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = state.drain.stopped_accepting() => break,
            accepted = listener.accept() => {
                let (tcp, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(err) => {
                        warn!(error = %err, "control accept failed");
                        continue;
                    }
                };
                let acceptor = acceptor.clone();
                let state = Arc::clone(&state);
                let session_cancel = cancel.child_token();
                tokio::spawn(async move {
                    if let Err(err) = serve_connection(tcp, peer, acceptor, state, session_cancel).await {
                        warn!(%peer, error = %err, "control connection ended");
                    }
                });
            }
        }
    }

    if state.drain.is_draining() {
        drop(listener);
        cancel.cancelled().await;
    }
    admin_task.abort();
    for task in data_tasks {
        task.abort();
    }
    info!("control listener stopped");
    Ok(())
}

async fn serve_connection(
    tcp: TcpStream,
    peer: std::net::SocketAddr,
    acceptor: TlsAcceptor,
    state: Arc<ServerState>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let _ = tcp.set_nodelay(true);
    let tls_stream = match timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => {
            state.metrics.handshake_failed("tls");
            warn!(%peer, error = %err, "TLS handshake failed");
            tokio::time::sleep(HANDSHAKE_FAIL_DELAY).await;
            return Err(err).context("TLS handshake failed");
        }
        Err(err) => {
            state.metrics.handshake_failed("tls_timeout");
            warn!(%peer, error = %err, "TLS handshake timed out");
            tokio::time::sleep(HANDSHAKE_FAIL_DELAY).await;
            return Err(err).context("TLS handshake timed out");
        }
    };

    let connection = transport::attach(tls_stream.compat(), transport::Role::Server);
    serve_transport(connection, peer, state, cancel, "tcp").await
}

async fn serve_transport(
    mut connection: transport::Connection,
    peer: std::net::SocketAddr,
    state: Arc<ServerState>,
    cancel: CancellationToken,
    transport_kind: &'static str,
) -> anyhow::Result<()> {
    state.metrics.transport_connected(transport_kind);
    let control = match timeout(CONTROL_STREAM_TIMEOUT, connection.accept()).await {
        Ok(Some(control)) => control,
        Ok(None) => {
            state.metrics.handshake_failed("control_stream");
            warn!(%peer, "connection closed before control stream");
            tokio::time::sleep(HANDSHAKE_FAIL_DELAY).await;
            return Err(anyhow!("connection closed before control stream"));
        }
        Err(err) => {
            state.metrics.handshake_failed("control_stream_timeout");
            warn!(%peer, error = %err, "control stream timed out");
            tokio::time::sleep(HANDSHAKE_FAIL_DELAY).await;
            return Err(err).context("client did not open a control stream");
        }
    };
    info!(%peer, "control stream accepted");

    let open = connection.open_handle();
    let closed = CancellationToken::new();
    let drain_closed = closed.clone();
    let drain = tokio::spawn(async move {
        while connection.accept().await.is_some() {}
        drain_closed.cancel();
    });
    let ctx = control::SessionContext {
        state,
        open,
        cancel,
        closed,
        peer: peer.ip(),
    };
    let result = control::serve(control, ctx).await;
    drain.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use meatus_core::parse_server_config;

    #[test]
    fn state_builds_and_plans_listeners() {
        let config = parse_server_config(
            r#"
[server]
control_port = 7000

[auth]
clients = [
  { id = "nas", token_sha256 = "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b" },
]

[http]
enabled = true
domain = "*.tunnel.example.com"
"#,
        )
        .expect("valid config");

        let state = ServerState::new(config).expect("state");
        let listeners = state.planned_listeners();
        assert!(listeners.iter().any(|l| l.starts_with("control")));
        assert!(listeners.iter().any(|l| l.starts_with("http")));
        assert!(listeners.iter().any(|l| l.starts_with("admin")));
    }
}
