#![forbid(unsafe_code)]
//! 客户端侧运行时：TLS 拨号、控制面、公网端口到本地服务的隧道。

mod session;
mod stats;
mod tls;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use meatus_core::{ClientConfig, ConfigError, ProxySpec, load_client_config};
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub struct ClientState {
    pub config: ClientConfig,
    /// 配置文件路径；存在时监听 SIGHUP 做代理差量热重载。
    pub config_path: Option<PathBuf>,
}

impl ClientState {
    pub fn new(config: ClientConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            config_path: None,
        })
    }

    #[must_use]
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = Some(path.into());
        self
    }

    /// 配置里声明的代理，按名称排序后返回，便于日志与状态展示稳定。
    #[must_use]
    pub fn proxy_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .config
            .proxies
            .iter()
            .map(|proxy| proxy.name.clone())
            .collect();
        names.sort();
        names
    }
}

pub async fn run(state: ClientState) -> anyhow::Result<()> {
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

/// 带指数退避重连的客户端入口，测试可用 `CancellationToken` 停止。
pub async fn run_until(state: ClientState, cancel: CancellationToken) -> anyhow::Result<()> {
    let config_path = state.config_path.clone();
    let mut config = Arc::new(state.config);
    let (reload_tx, mut reload_rx) = mpsc::channel::<Vec<ProxySpec>>(4);
    let reload_task = config_path.as_ref().map(|path| {
        tokio::spawn(reload_loop(
            path.clone(),
            reload_tx,
            cancel.clone(),
            Arc::clone(&config),
        ))
    });

    info!(
        server = %config.client.server_addr,
        proxies = config.proxies.len(),
        config = ?config_path,
        "client starting"
    );

    let mut backoff_secs = 1u64;
    loop {
        if cancel.is_cancelled() {
            if let Some(task) = &reload_task {
                task.abort();
            }
            return Ok(());
        }

        let started = Instant::now();
        let result = tokio::select! {
            _ = cancel.cancelled() => {
                if let Some(task) = &reload_task {
                    task.abort();
                }
                return Ok(());
            }
            result = session::run_session(&config, &cancel, &mut reload_rx, config_path.as_deref()) => result,
        };
        let stable = started.elapsed() >= Duration::from_secs(30);

        match result {
            Ok(()) => info!("control session closed, reconnecting"),
            Err(err) => warn!(error = %err, "control session failed, reconnecting"),
        }

        let delay = jittered(backoff_secs);
        info!(seconds = delay.as_secs_f32(), "reconnect backoff");
        tokio::select! {
            _ = cancel.cancelled() => {
                if let Some(task) = &reload_task {
                    task.abort();
                }
                return Ok(());
            }
            _ = sleep(delay) => {}
        }

        backoff_secs = if stable {
            1
        } else {
            (backoff_secs * 2).min(30)
        };

        // 文件可能已被修改但 SIGHUP 丢失；重连时重新读取最新配置。
        if let Some(path) = &config_path {
            match load_client_config(path) {
                Ok(new_config) => {
                    info!(path = %path.display(), "client config reloaded for reconnect");
                    config = Arc::new(new_config);
                }
                Err(err) => warn!(path = %path.display(), error = %err, "keep previous config"),
            }
        }
    }
}

async fn reload_loop(
    path: PathBuf,
    tx: mpsc::Sender<Vec<ProxySpec>>,
    cancel: CancellationToken,
    initial: Arc<ClientConfig>,
) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sighup = match signal(SignalKind::hangup()) {
            Ok(signal) => signal,
            Err(err) => {
                warn!(error = %err, "cannot listen for SIGHUP; hot reload disabled");
                return;
            }
        };
        info!(path = %path.display(), "SIGHUP hot reload listening");
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = sighup.recv() => {}
            }
            match load_client_config(&path) {
                Ok(next) => {
                    if next.client.server_addr != initial.client.server_addr
                        || next.client.server_name != initial.client.server_name
                        || next.client.server_ca_cert != initial.client.server_ca_cert
                        || next.client.server_public_key_pin != initial.client.server_public_key_pin
                        || next.client.token != initial.client.token
                        || next.client.client_id != initial.client.client_id
                        || next.client.tenant != initial.client.tenant
                    {
                        warn!(
                            path = %path.display(),
                            "connection-level client config changed; restart client to apply"
                        );
                    }
                    info!(path = %path.display(), proxies = next.proxies.len(), "SIGHUP reload");
                    if tx.send(next.proxies).await.is_err() {
                        return;
                    }
                }
                Err(err) => {
                    warn!(path = %path.display(), error = %err, "SIGHUP reload rejected; keep old config");
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, tx, cancel, initial);
    }
}

/// `base` 秒 ±20% 抖动，避免大量客户端同时重连。
fn jittered(base_secs: u64) -> Duration {
    let mut bytes = [0u8; 4];
    let percent = if getrandom::fill(&mut bytes).is_ok() {
        80 + u64::from(u32::from_le_bytes(bytes) % 41)
    } else {
        100
    };
    Duration::from_millis(base_secs * 1000 * percent / 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use meatus_core::parse_client_config;

    #[test]
    fn proxy_names_are_sorted() {
        let config = parse_client_config(
            r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"

[[proxy]]
name = "zeta"
type = "tcp"
local_addr = "127.0.0.1:22"

[[proxy]]
name = "alpha"
type = "tcp"
local_addr = "127.0.0.1:23"
"#,
        )
        .expect("valid config");

        let state = ClientState::new(config).expect("state");
        assert_eq!(state.proxy_names(), vec!["alpha", "zeta"]);
    }

    #[test]
    fn backoff_is_jittered_within_bounds() {
        for base in [1u64, 8, 30] {
            let delay = jittered(base);
            assert!(delay >= Duration::from_millis(base * 800));
            assert!(delay <= Duration::from_millis(base * 1200));
        }
    }
}
