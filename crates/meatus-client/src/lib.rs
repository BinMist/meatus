#![forbid(unsafe_code)]
//! 客户端侧运行时：TLS 拨号、控制面、公网端口到本地服务的隧道。

mod probe;
pub mod quic;
mod session;
mod stats;
mod tls;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
    dial: Arc<DialStatus>,
}

/// 客户端入口切换状态；只保留脱敏后的地址，不含 token。
#[derive(Debug, Default)]
pub struct DialStatus {
    entry: Mutex<String>,
    entry_index: AtomicUsize,
    entry_total: AtomicUsize,
    failures: AtomicU64,
    next_retry_ms: AtomicU64,
}

/// 某一时刻的入口状态快照，供日志 / 外部状态展示使用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialSnapshot {
    pub current_entry: String,
    pub entry_index: usize,
    pub entry_total: usize,
    pub failures: u64,
    pub next_retry_unix_ms: u64,
}

impl DialStatus {
    #[must_use]
    fn redacted_status(&self) -> String {
        self.entry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_entry(&self, addr: &str, index: usize, total: usize) {
        let mut entry = self
            .entry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *entry = redact_addr(addr);
        self.entry_index.store(index, Ordering::Relaxed);
        self.entry_total.store(total, Ordering::Relaxed);
    }

    fn schedule_next_retry(&self, next_retry: Duration) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        self.next_retry_ms.store(
            now.as_millis() as u64 + next_retry.as_millis() as u64,
            Ordering::Relaxed,
        );
    }

    fn record_failure(&self, next_retry: Duration) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        self.schedule_next_retry(next_retry);
    }

    fn record_success(&self) {
        self.failures.store(0, Ordering::Relaxed);
        self.next_retry_ms.store(0, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self) -> DialSnapshot {
        DialSnapshot {
            current_entry: self.redacted_status(),
            entry_index: self.entry_index.load(Ordering::Relaxed),
            entry_total: self.entry_total.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            next_retry_unix_ms: self.next_retry_ms.load(Ordering::Relaxed),
        }
    }
}

/// 入口地址脱敏：去掉可能存在的 userinfo，只保留 host:port。
fn redact_addr(addr: &str) -> String {
    addr.rsplit('@').next().unwrap_or(addr).to_string()
}

impl ClientState {
    pub fn new(config: ClientConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            config_path: None,
            dial: Arc::new(DialStatus::default()),
        })
    }

    /// 入口切换状态，可在运行期间读取；不含 token 等敏感信息。
    #[must_use]
    pub fn dial_snapshot(&self) -> DialSnapshot {
        self.dial.snapshot()
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
    let dial = Arc::clone(&state.dial);
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

    let mut dial_addrs = config.dial_addrs();
    info!(
        server = %dial_addrs.join(","),
        proxies = config.proxies.len(),
        config = ?config_path,
        "client starting"
    );

    let mut backoff_secs = 1u64;
    let mut dial_index = 0usize;
    loop {
        if cancel.is_cancelled() {
            if let Some(task) = &reload_task {
                task.abort();
            }
            return Ok(());
        }

        let selected_index = dial_index % dial_addrs.len();
        dial.set_entry(
            &dial_addrs[selected_index],
            selected_index,
            dial_addrs.len(),
        );
        let started = Instant::now();
        let result = tokio::select! {
            _ = cancel.cancelled() => {
                if let Some(task) = &reload_task {
                    task.abort();
                }
                return Ok(());
            }
            result = session::run_session(
                &config,
                &dial_addrs[dial_index % dial_addrs.len()],
                &cancel,
                &mut reload_rx,
                config_path.as_deref(),
            ) => result,
        };
        let stable = started.elapsed() >= Duration::from_secs(30);
        let session_failed = result.is_err();

        let attempted = dial_addrs[selected_index].clone();
        match result {
            Ok(()) => info!(
                server = %attempted,
                entry_index = selected_index,
                entry_total = dial_addrs.len(),
                "control session closed, reconnecting"
            ),
            Err(err) => warn!(
                server = %attempted,
                entry_index = selected_index,
                entry_total = dial_addrs.len(),
                failures = dial.snapshot().failures.saturating_add(1),
                error = %err,
                "control session failed, reconnecting"
            ),
        }
        // 入口轮转：当前入口结束或失败后，下次一定尝试下一个，避免死守失效入口。
        dial_index = (dial_index + 1) % dial_addrs.len();

        let delay = jittered(backoff_secs);
        if session_failed {
            dial.record_failure(delay);
        } else {
            dial.schedule_next_retry(delay);
        }
        info!(
            seconds = delay.as_secs_f32(),
            entry_index = selected_index,
            entry_total = dial_addrs.len(),
            failures = dial.snapshot().failures,
            "reconnect backoff"
        );
        tokio::select! {
            _ = cancel.cancelled() => {
                if let Some(task) = &reload_task {
                    task.abort();
                }
                return Ok(());
            }
            _ = sleep(delay) => {}
        }

        if stable {
            backoff_secs = 1;
            dial.record_success();
        } else {
            backoff_secs = (backoff_secs * 2).min(30);
        }

        // 文件可能已被修改但 SIGHUP 丢失；重连时重新读取最新配置。
        if let Some(path) = &config_path {
            match load_client_config(path) {
                Ok(new_config) => {
                    info!(path = %path.display(), "client config reloaded for reconnect");
                    config = Arc::new(new_config);
                    dial_addrs = config.dial_addrs();
                    dial_index %= dial_addrs.len();
                    info!(entries = dial_addrs.len(), "client dial entries reloaded");
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
    fn dial_status_redacts_userinfo_and_tracks_failures() {
        let status = DialStatus::default();
        status.set_entry("user:pass@entry.example.com:7000", 1, 3);
        let snapshot = status.snapshot();
        assert_eq!(snapshot.current_entry, "entry.example.com:7000");
        assert_eq!(snapshot.entry_index, 1);
        assert_eq!(snapshot.entry_total, 3);
        assert_eq!(snapshot.failures, 0);
        status.record_failure(Duration::from_secs(2));
        assert_eq!(status.snapshot().failures, 1);
        assert!(status.snapshot().next_retry_unix_ms > 0);
        status.record_success();
        assert_eq!(status.snapshot().failures, 0);
        assert_eq!(status.snapshot().next_retry_unix_ms, 0);
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
