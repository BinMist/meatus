#![forbid(unsafe_code)]
//! relay 侧运行时：TLS 1.3 控制面、yamux 复用与公网端口映射。

mod acme;
mod admin;
mod admin_bus;
mod control;
mod http;
pub mod logbuf;
mod metrics;
mod routes;
mod runtime;
mod socks5;
mod status;
mod tls;

pub use runtime::{ConfigReloader, LogReloadHandle, ReloadSnapshot, RuntimeConfig};

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use logbuf::LogBuffer;
use meatus_core::{
    AllowAllQuota, AuthProvider, ConfigError, LocalPortAllocator, LocalTokenAuth, NameAllocator,
    NullUsageSink, QuotaPolicy, ServerConfig, UsageSink, multiplex,
};
use metrics::Metrics;
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
    pub usage: Arc<dyn UsageSink>,
    pub metrics: Arc<Metrics>,
    pub domains: Arc<DomainRouter>,
    pub(crate) status: Arc<StatusRegistry>,
    pub(crate) socks5: Arc<Socks5Registry>,
    pub acme: Option<Arc<acme::AcmeManager>>,
    pub logs: Arc<LogBuffer>,
    pub(crate) admin_bus: Arc<admin_bus::AdminBus>,
    /// 当前生效的配置与认证；SIGHUP / 面板重载共用。
    pub runtime: Arc<RuntimeConfig>,
    /// 配置热加载器；没有配置文件路径时为 `None`。
    pub reloader: Option<Arc<ConfigReloader>>,
    /// CLI 注入的日志级别热加载句柄。
    pub log_reload: Option<LogReloadHandle>,
}

impl ServerState {
    pub fn new(config: ServerConfig) -> Result<Self, ConfigError> {
        config.validate()?;

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

        Ok(Self {
            config,
            config_path: None,
            allocator: Arc::new(allocator),
            quota: Arc::new(AllowAllQuota),
            usage: Arc::new(NullUsageSink),
            metrics: Arc::new(Metrics::new()),
            domains: Arc::new(DomainRouter::new()),
            status: Arc::new(StatusRegistry::default()),
            socks5: Arc::new(Socks5Registry::default()),
            acme,
            logs: Arc::new(LogBuffer::default()),
            admin_bus: Arc::new(admin_bus::AdminBus::default()),
            runtime,
            reloader: None,
            log_reload: None,
        })
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
        self.config_path = Some(path);
        self
    }

    /// 注入日志级别热加载句柄；必须在 `with_config_path` 之前调用。
    pub fn set_log_reload(&mut self, handle: Option<LogReloadHandle>) {
        self.log_reload = handle;
    }

    /// 启动时会尝试占用的监听地址，`meatus check --dry-run` 也用它。
    #[must_use]
    pub fn planned_listeners(&self) -> Vec<String> {
        let mut listeners = vec![format!(
            "control tcp://{}:{}",
            self.config.server.bind_addr, self.config.server.control_port
        )];

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

        listeners
    }
}

pub async fn run(state: ServerState) -> anyhow::Result<()> {
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();

    tokio::select! {
        result = run_until(state, run_cancel) => result,
        _ = shutdown_signal() => {
            cancel.cancel();
            info!("shutdown requested");
            Ok(())
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
pub async fn run_until(state: ServerState, cancel: CancellationToken) -> anyhow::Result<()> {
    state.config.validate_runtime()?;
    let acceptor = TlsAcceptor::from(tls::load(&state.config)?);

    let bind = format!(
        "{}:{}",
        state.config.server.bind_addr, state.config.server.control_port
    );
    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind control listener {bind}"))?;
    let local = listener.local_addr().context("control local_addr")?;
    info!(%local, "control listener ready: TLS + yamux");

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
    let config_path = state.config_path.clone();
    let state = Arc::new(state);
    let admin_state = admin::AdminState::new(
        Arc::clone(&state.metrics),
        Arc::clone(&state.status),
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
    let tls_stream = match timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => {
            state.metrics.handshake_failed("tls");
            return Err(err).context("TLS handshake failed");
        }
        Err(err) => {
            state.metrics.handshake_failed("tls_timeout");
            return Err(err).context("TLS handshake timed out");
        }
    };

    let (open, mut inbound) = multiplex::spawn(tls_stream.compat(), yamux::Mode::Server);

    let control = timeout(CONTROL_STREAM_TIMEOUT, inbound.recv())
        .await
        .context("client did not open a control stream")?
        .context("connection closed before control stream")?;
    info!(%peer, "control stream accepted");
    state.metrics.connected_client(1);
    struct ConnectedGuard(Arc<ServerState>);
    impl Drop for ConnectedGuard {
        fn drop(&mut self) {
            self.0.metrics.connected_client(-1);
        }
    }
    let _connected = ConnectedGuard(Arc::clone(&state));

    let drain = tokio::spawn(async move { while inbound.recv().await.is_some() {} });
    let ctx = control::SessionContext {
        state,
        open,
        cancel,
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
