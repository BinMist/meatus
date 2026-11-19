#![forbid(unsafe_code)]
//! 服务端运行时配置：保存当前已生效配置、认证提供者与热加载状态。

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use meatus_core::{
    AuthProvider, ConfigError, ConfigFormat, LocalTokenAuth, ServerConfig,
    parse_server_config_with_format,
};
use sha2::{Digest, Sha256};

/// 日志级别热加载句柄；由 CLI 在初始化 tracing subscriber 后注入。
pub type LogReloadHandle = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// 面板与 SIGHUP 共用的配置热加载状态。
#[derive(Debug, Clone)]
pub struct ReloadSnapshot {
    pub revision: String,
    pub last_reload_ms: Option<i64>,
    pub last_error: Option<String>,
    pub restart_required: Vec<String>,
    pub reload_count: u64,
}

pub struct RuntimeConfig {
    /// 配置与认证提供者放在同一把锁里，热加载时对外始终成对生效。
    active: RwLock<(Arc<ServerConfig>, Arc<dyn AuthProvider>)>,
    revision: RwLock<String>,
    last_reload_ms: RwLock<Option<i64>>,
    last_error: RwLock<Option<String>>,
    restart_required: RwLock<Vec<String>>,
    reload_count: RwLock<u64>,
}

impl RuntimeConfig {
    #[must_use]
    pub fn new(
        config: ServerConfig,
        auth: Arc<dyn AuthProvider>,
        revision: impl Into<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            active: RwLock::new((Arc::new(config), auth)),
            revision: RwLock::new(revision.into()),
            last_reload_ms: RwLock::new(None),
            last_error: RwLock::new(None),
            restart_required: RwLock::new(Vec::new()),
            reload_count: RwLock::new(0),
        })
    }

    /// 当前期望配置。监听地址等启动期字段仍以 `ServerState::config` 为准。
    #[must_use]
    pub fn config(&self) -> Arc<ServerConfig> {
        Arc::clone(&self.active.read().expect("runtime config lock poisoned").0)
    }

    /// 当前生效的认证提供者；新建连接使用它，已有连接不受影响。
    #[must_use]
    pub fn auth(&self) -> Arc<dyn AuthProvider> {
        Arc::clone(&self.active.read().expect("runtime auth lock poisoned").1)
    }

    #[must_use]
    pub fn revision(&self) -> String {
        self.revision
            .read()
            .expect("runtime revision lock poisoned")
            .clone()
    }

    #[must_use]
    pub fn snapshot(&self) -> ReloadSnapshot {
        ReloadSnapshot {
            revision: self.revision(),
            last_reload_ms: *self
                .last_reload_ms
                .read()
                .expect("runtime reload time lock poisoned"),
            last_error: self
                .last_error
                .read()
                .expect("runtime error lock poisoned")
                .clone(),
            restart_required: self
                .restart_required
                .read()
                .expect("runtime restart lock poisoned")
                .clone(),
            reload_count: *self
                .reload_count
                .read()
                .expect("runtime reload count lock poisoned"),
        }
    }

    pub fn record_failure(&self, error: impl Into<String>) {
        *self
            .last_error
            .write()
            .expect("runtime error lock poisoned") = Some(error.into());
    }

    fn set_initial_revision(&self, revision: String) {
        *self
            .revision
            .write()
            .expect("runtime revision lock poisoned") = revision;
    }

    fn apply(
        &self,
        config: ServerConfig,
        auth: Arc<dyn AuthProvider>,
        revision: String,
        restart_required: Vec<String>,
    ) -> ReloadSnapshot {
        *self.active.write().expect("runtime config lock poisoned") = (Arc::new(config), auth);
        *self
            .revision
            .write()
            .expect("runtime revision lock poisoned") = revision;
        let now = now_ms();
        *self
            .last_reload_ms
            .write()
            .expect("runtime reload time lock poisoned") = Some(now);
        *self
            .last_error
            .write()
            .expect("runtime error lock poisoned") = None;
        *self
            .restart_required
            .write()
            .expect("runtime restart lock poisoned") = restart_required.clone();
        *self
            .reload_count
            .write()
            .expect("runtime reload count lock poisoned") += 1;
        self.snapshot()
    }
}

/// 从配置文件热加载认证与日志；坏配置保留旧配置。
pub struct ConfigReloader {
    path: PathBuf,
    format: ConfigFormat,
    runtime: Arc<RuntimeConfig>,
    log_reload: Option<LogReloadHandle>,
}

impl ConfigReloader {
    #[must_use]
    pub fn new(
        path: PathBuf,
        format: ConfigFormat,
        runtime: Arc<RuntimeConfig>,
        log_reload: Option<LogReloadHandle>,
    ) -> Arc<Self> {
        if let Ok(raw) = fs::read_to_string(&path) {
            runtime.set_initial_revision(revision_of(&raw));
        }
        Arc::new(Self {
            path,
            format,
            runtime,
            log_reload,
        })
    }

    #[must_use]
    pub fn runtime(&self) -> Arc<RuntimeConfig> {
        Arc::clone(&self.runtime)
    }

    /// 执行一次热加载。失败时记录错误，但不替换任何已生效配置。
    pub fn reload(&self) -> Result<ReloadSnapshot, ConfigError> {
        match self.reload_inner() {
            Ok(snapshot) => Ok(snapshot),
            Err(error) => {
                self.runtime.record_failure(error.to_string());
                Err(error)
            }
        }
    }

    fn reload_inner(&self) -> Result<ReloadSnapshot, ConfigError> {
        let raw = fs::read_to_string(&self.path).map_err(|source| ConfigError::Io {
            path: self.path.clone(),
            source,
        })?;
        let revision = revision_of(&raw);
        let next = parse_server_config_with_format(&raw, self.format)?;
        let next_auth: Arc<dyn AuthProvider> = Arc::new(LocalTokenAuth::from_config(&next)?);

        let current = self.runtime.config();
        let mut restart_required = restart_fields(&current, &next);

        if current.log.level != next.log.level {
            if let Some(reload) = &self.log_reload {
                reload(&next.log.level)
                    .map_err(|error| ConfigError::Invalid(format!("apply log level: {error}")))?;
            } else {
                restart_required.push("log.level".to_string());
            }
        }
        if current.log.format != next.log.format {
            restart_required.push("log.format".to_string());
        }
        restart_required.sort();
        restart_required.dedup();

        Ok(self
            .runtime
            .apply(next, next_auth, revision, restart_required))
    }
}

/// 需要重启才能生效的配置段；auth / log.level 不在此列。
#[must_use]
pub fn restart_fields(current: &ServerConfig, next: &ServerConfig) -> Vec<String> {
    let mut fields = Vec::new();
    if current.server != next.server {
        fields.push("server".to_string());
    }
    if current.admin != next.admin {
        fields.push("admin".to_string());
    }
    if current.http != next.http {
        fields.push("http".to_string());
    }
    if current.socks5 != next.socks5 {
        fields.push("socks5".to_string());
    }
    if current.acme != next.acme {
        fields.push("acme".to_string());
    }
    if current.ports != next.ports {
        fields.push("ports".to_string());
    }
    if current.ctrl != next.ctrl {
        fields.push("ctrl".to_string());
    }
    fields
}

fn revision_of(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    format!("{:x}", digest)[..12].to_string()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use meatus_core::{LocalTokenAuth, parse_server_config, proof};

    fn config_text(control_port: u16, token_sha256: &str) -> String {
        format!(
            r#"
[server]
bind_addr = "127.0.0.1"
control_port = {control_port}
tls_cert = "/tmp/meatus-test.crt"
tls_key = "/tmp/meatus-test.key"

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{token_sha256}" }},
]

[ports]
range = [20000, 20010]
"#
        )
    }

    fn reloader(path: PathBuf, runtime: Arc<RuntimeConfig>) -> Arc<ConfigReloader> {
        ConfigReloader::new(path, ConfigFormat::Toml, runtime, None)
    }

    #[tokio::test]
    async fn reload_swaps_auth_and_keeps_previous_on_bad_config() {
        let dir = std::env::temp_dir().join(format!("meatus-runtime-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("server.toml");
        let old_hash =
            "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b".to_string();
        let new_hash =
            "9c0ee26e4a1fbb028187486a7ea91f81f8ab81fcf467cba75107dbd3a64244d7".to_string();
        std::fs::write(&path, config_text(7000, &old_hash)).expect("write old config");

        let config = parse_server_config(&std::fs::read_to_string(&path).expect("read config"))
            .expect("parse config");
        let auth: Arc<dyn AuthProvider> =
            Arc::new(LocalTokenAuth::from_config(&config).expect("auth"));
        let runtime = RuntimeConfig::new(config, auth, "startup");
        let reloader = reloader(path.clone(), Arc::clone(&runtime));
        let nonce = [9u8; 32];

        assert!(
            runtime
                .auth()
                .verify_hmac("default", "nas", &nonce, &proof("secret", &nonce))
                .await
                .is_ok()
        );

        std::fs::write(&path, config_text(7000, &new_hash)).expect("write new config");
        let snapshot = reloader.reload().expect("reload");
        assert_eq!(snapshot.reload_count, 1);
        assert_ne!(snapshot.revision, "startup");
        assert!(snapshot.last_error.is_none());

        assert!(
            runtime
                .auth()
                .verify_hmac("default", "nas", &nonce, &proof("secret", &nonce))
                .await
                .is_err()
        );
        assert!(
            runtime
                .auth()
                .verify_hmac("default", "nas", &nonce, &proof("other-secret", &nonce))
                .await
                .is_ok()
        );

        std::fs::write(&path, "not valid toml [[[").expect("write bad config");
        let error = reloader.reload().expect_err("bad config must be rejected");
        assert!(error.to_string().contains("TOML"));
        let after = runtime.snapshot();
        assert_eq!(after.revision, snapshot.revision);
        assert_eq!(after.reload_count, 1);
        assert!(after.last_error.is_some());
        assert!(
            runtime
                .auth()
                .verify_hmac("default", "nas", &nonce, &proof("other-secret", &nonce))
                .await
                .is_ok()
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn restart_fields_ignore_auth_and_log_level() {
        let base = parse_server_config(&config_text(7000, &"00".repeat(32))).expect("base");
        let mut next = base.clone();
        next.log.level = "warn".to_string();
        assert!(restart_fields(&base, &next).is_empty());

        next.http.bind = "0.0.0.0:8080".to_string();
        let fields = restart_fields(&base, &next);
        assert_eq!(fields, vec!["http".to_string()]);
    }
}
