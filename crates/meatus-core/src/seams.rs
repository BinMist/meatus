//! 扩展接缝：鉴权、配置来源、用量、命名分配、配额。
//!
//! v0.x 全部走本地实现，但 relay 只依赖 trait，不直接读文件或全局单例，
//! 这样未来由外部控制面接管时不需要改数据面。

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tracing::trace;

use crate::config::{ClientPolicy, ConfigError, ServerConfig, parse_server_config};

// ---------------------------------------------------------------------------
// AuthProvider
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    pub tenant_id: String,
    pub client_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("unknown client")]
    UnknownClient,
    #[error("token mismatch")]
    TokenMismatch,
    #[error("auth provider unavailable: {0}")]
    Unavailable(String),
}

#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// 校验客户端令牌。HMAC 挑战应答在 M1 接入，本方法只回答“这个 token 是否属于该身份”。
    async fn verify_token(
        &self,
        tenant_id: &str,
        client_id: &str,
        token: &str,
    ) -> Result<ClientIdentity, AuthError>;

    /// 校验 `HMAC-SHA256(sha256(token), nonce)` 证明。
    async fn verify_hmac(
        &self,
        tenant_id: &str,
        client_id: &str,
        nonce: &[u8],
        proof: &[u8],
    ) -> Result<ClientIdentity, AuthError>;

    /// 该 client 的授权快照；默认返回不限制，保持 remote provider 兼容。
    fn client_policy(&self, _tenant_id: &str, _client_id: &str) -> ClientPolicy {
        ClientPolicy::default()
    }
}

struct Credential {
    tenant_id: String,
    client_id: String,
    token_sha256: [u8; 32],
    policy: ClientPolicy,
}

/// 本地鉴权：token 的 SHA-256 存在配置里，比较用常量时间。
pub struct LocalTokenAuth {
    credentials: Vec<Credential>,
}

impl LocalTokenAuth {
    pub fn from_config(config: &ServerConfig) -> Result<Self, ConfigError> {
        let mut credentials = Vec::new();
        for (client_id, tenant_id, hash) in config.auth.credential_entries() {
            let raw = hex::decode(hash.trim()).map_err(|err| {
                ConfigError::Invalid(format!("token_sha256 for {client_id} is not hex: {err}"))
            })?;
            let token_sha256: [u8; 32] = raw.try_into().map_err(|_| {
                ConfigError::Invalid(format!("token_sha256 for {client_id} must be 32 bytes"))
            })?;
            credentials.push(Credential {
                policy: config
                    .auth
                    .clients
                    .iter()
                    .find(|entry| entry.tenant == tenant_id && entry.id == client_id)
                    .map_or_else(ClientPolicy::default, crate::config::ClientEntry::policy),
                tenant_id,
                client_id,
                token_sha256,
            });
        }
        Ok(Self { credentials })
    }
}

#[async_trait]
impl AuthProvider for LocalTokenAuth {
    async fn verify_token(
        &self,
        tenant_id: &str,
        client_id: &str,
        token: &str,
    ) -> Result<ClientIdentity, AuthError> {
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut seen = false;

        for credential in &self.credentials {
            if credential.tenant_id == tenant_id && credential.client_id == client_id {
                seen = true;
                if bool::from(credential.token_sha256.ct_eq(&digest)) {
                    return Ok(ClientIdentity {
                        tenant_id: tenant_id.to_string(),
                        client_id: client_id.to_string(),
                    });
                }
            }
        }

        if seen {
            Err(AuthError::TokenMismatch)
        } else {
            Err(AuthError::UnknownClient)
        }
    }

    async fn verify_hmac(
        &self,
        tenant_id: &str,
        client_id: &str,
        nonce: &[u8],
        proof: &[u8],
    ) -> Result<ClientIdentity, AuthError> {
        let mut seen = false;

        for credential in &self.credentials {
            if credential.tenant_id == tenant_id && credential.client_id == client_id {
                seen = true;
                if crate::auth::verify_proof_with_key(&credential.token_sha256, nonce, proof) {
                    return Ok(ClientIdentity {
                        tenant_id: tenant_id.to_string(),
                        client_id: client_id.to_string(),
                    });
                }
            }
        }

        if seen {
            Err(AuthError::TokenMismatch)
        } else {
            Err(AuthError::UnknownClient)
        }
    }

    fn client_policy(&self, tenant_id: &str, client_id: &str) -> ClientPolicy {
        self.credentials
            .iter()
            .find(|credential| {
                credential.tenant_id == tenant_id && credential.client_id == client_id
            })
            .map_or_else(ClientPolicy::default, |credential| {
                credential.policy.clone()
            })
    }
}

// ---------------------------------------------------------------------------
// ConfigSource
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ConfigSnapshot {
    /// 配置版本号，用于热更新与审计（文件实现取内容哈希前 12 位）。
    pub revision: String,
    pub server: ServerConfig,
}

pub trait ConfigSource: Send + Sync {
    fn load(&self) -> Result<ConfigSnapshot, ConfigError>;
}

pub struct FileConfigSource {
    path: PathBuf,
}

impl FileConfigSource {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl ConfigSource for FileConfigSource {
    fn load(&self) -> Result<ConfigSnapshot, ConfigError> {
        let raw = fs::read_to_string(&self.path).map_err(|source| ConfigError::Io {
            path: self.path.clone(),
            source,
        })?;
        let digest = hex::encode(Sha256::digest(raw.as_bytes()));
        let revision = digest[..12].to_string();
        let server = parse_server_config(&raw)?;
        Ok(ConfigSnapshot { revision, server })
    }
}

// ---------------------------------------------------------------------------
// UsageSink
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct UsageRecord {
    pub tenant_id: String,
    pub client_id: String,
    pub proxy_name: String,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub connections: u32,
    pub at: SystemTime,
}

pub trait UsageSink: Send + Sync {
    fn record(&self, record: &UsageRecord);
}

/// v0.x 默认实现：只落 trace，未来可替换为外部控制面上报。
pub struct NullUsageSink;

impl UsageSink for NullUsageSink {
    fn record(&self, record: &UsageRecord) {
        trace!(
            tenant = %record.tenant_id,
            client = %record.client_id,
            proxy = %record.proxy_name,
            bytes_in = record.bytes_in,
            bytes_out = record.bytes_out,
            connections = record.connections,
            "usage"
        );
    }
}

// ---------------------------------------------------------------------------
// NameAllocator
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum AllocError {
    #[error("port {0} is outside the configured range")]
    OutOfRange(u16),
    #[error("port {0} is already leased")]
    Taken(u16),
    #[error("port pool exhausted")]
    Exhausted,
}

struct Lease {
    tenant_id: String,
    proxy_name: String,
}

struct AllocatorInner {
    leases: HashMap<u16, Lease>,
    cursor: u16,
}

pub struct LocalPortAllocator {
    start: u16,
    end: u16,
    inner: Mutex<AllocatorInner>,
}

impl LocalPortAllocator {
    pub fn new(range: [u16; 2]) -> Result<Self, AllocError> {
        let [start, end] = range;
        if start < 1024 || start > end {
            return Err(AllocError::OutOfRange(start));
        }
        Ok(Self {
            start,
            end,
            inner: Mutex::new(AllocatorInner {
                leases: HashMap::new(),
                cursor: start,
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, AllocatorInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn leased_count(&self) -> usize {
        self.lock().leases.len()
    }

    /// 查询某个端口的租约，用于状态展示与排障。
    #[must_use]
    pub fn lease_owner(&self, port: u16) -> Option<(String, String)> {
        self.lock()
            .leases
            .get(&port)
            .map(|lease| (lease.tenant_id.clone(), lease.proxy_name.clone()))
    }
}

impl LocalPortAllocator {
    fn alloc_port_between(
        &self,
        tenant_id: &str,
        proxy_name: &str,
        preferred: Option<u16>,
        start: u16,
        end: u16,
    ) -> Result<u16, AllocError> {
        let mut inner = self.lock();

        if let Some(port) = preferred {
            if port < start || port > end {
                return Err(AllocError::OutOfRange(port));
            }
            if let Some(lease) = inner.leases.get(&port) {
                if lease.tenant_id == tenant_id && lease.proxy_name == proxy_name {
                    return Ok(port);
                }
                return Err(AllocError::Taken(port));
            }
            inner.leases.insert(
                port,
                Lease {
                    tenant_id: tenant_id.to_string(),
                    proxy_name: proxy_name.to_string(),
                },
            );
            inner.cursor = if port == self.end {
                self.start
            } else {
                port + 1
            };
            return Ok(port);
        }

        // 保留期内重连且未指定 remote_port 时，优先取回原租约。
        for port in start..=end {
            if inner
                .leases
                .get(&port)
                .is_some_and(|lease| lease.tenant_id == tenant_id && lease.proxy_name == proxy_name)
            {
                return Ok(port);
            }
        }

        let span = u32::from(end) - u32::from(start) + 1;
        let cursor_start = inner.cursor.clamp(start, end);
        for offset in 0..span {
            let cursor_offset = (u32::from(cursor_start) - u32::from(start)) % span;
            let port = start + ((cursor_offset + offset) % span) as u16;
            if let Some(lease) = inner.leases.get(&port) {
                if lease.tenant_id == tenant_id && lease.proxy_name == proxy_name {
                    return Ok(port);
                }
                continue;
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = inner.leases.entry(port) {
                slot.insert(Lease {
                    tenant_id: tenant_id.to_string(),
                    proxy_name: proxy_name.to_string(),
                });
                inner.cursor = if port == self.end {
                    self.start
                } else {
                    port + 1
                };
                return Ok(port);
            }
        }

        Err(AllocError::Exhausted)
    }
}

impl NameAllocator for LocalPortAllocator {
    fn alloc_port(
        &self,
        tenant_id: &str,
        proxy_name: &str,
        preferred: Option<u16>,
    ) -> Result<u16, AllocError> {
        self.alloc_port_between(tenant_id, proxy_name, preferred, self.start, self.end)
    }

    fn alloc_port_in_range(
        &self,
        tenant_id: &str,
        proxy_name: &str,
        preferred: Option<u16>,
        allowed: Option<[u16; 2]>,
    ) -> Result<u16, AllocError> {
        let Some([allowed_start, allowed_end]) = allowed else {
            return self.alloc_port(tenant_id, proxy_name, preferred);
        };
        let start = self.start.max(allowed_start);
        let end = self.end.min(allowed_end);
        if start > end {
            return Err(AllocError::Exhausted);
        }
        self.alloc_port_between(tenant_id, proxy_name, preferred, start, end)
    }

    fn release_port(&self, tenant_id: &str, port: u16) -> bool {
        let mut inner = self.lock();
        let matches_tenant = inner
            .leases
            .get(&port)
            .is_some_and(|lease| lease.tenant_id == tenant_id);
        if matches_tenant {
            inner.leases.remove(&port);
        }
        matches_tenant
    }

    fn release_lease(&self, tenant_id: &str, proxy_name: &str, port: u16) -> bool {
        let mut inner = self.lock();
        let matches = inner
            .leases
            .get(&port)
            .is_some_and(|lease| lease.tenant_id == tenant_id && lease.proxy_name == proxy_name);
        if matches {
            inner.leases.remove(&port);
        }
        matches
    }
}

pub trait NameAllocator: Send + Sync {
    fn alloc_port(
        &self,
        tenant_id: &str,
        proxy_name: &str,
        preferred: Option<u16>,
    ) -> Result<u16, AllocError>;

    fn release_port(&self, tenant_id: &str, port: u16) -> bool;

    /// 在全局端口池与客户端授权范围的交集内分配端口。
    fn alloc_port_in_range(
        &self,
        tenant_id: &str,
        proxy_name: &str,
        preferred: Option<u16>,
        allowed: Option<[u16; 2]>,
    ) -> Result<u16, AllocError> {
        let Some([start, end]) = allowed else {
            return self.alloc_port(tenant_id, proxy_name, preferred);
        };
        let port = self.alloc_port(tenant_id, proxy_name, preferred)?;
        if port >= start && port <= end {
            Ok(port)
        } else {
            self.release_port(tenant_id, port);
            Err(AllocError::OutOfRange(port))
        }
    }

    /// 仅当租约仍属于该 tenant + proxy 时释放，避免延迟释放误删新租约。
    fn release_lease(&self, tenant_id: &str, proxy_name: &str, port: u16) -> bool {
        let _ = proxy_name;
        self.release_port(tenant_id, port)
    }
}

// ---------------------------------------------------------------------------
// QuotaPolicy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct QuotaRequest {
    pub tenant_id: String,
    pub current_proxies: u32,
    pub current_ports: u32,
    pub requested_ports: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum QuotaError {
    #[error("proxy quota exceeded for tenant {tenant_id}: limit {limit}, current {current}")]
    ProxiesExceeded {
        tenant_id: String,
        limit: u32,
        current: u32,
    },
    #[error("port quota exceeded for tenant {tenant_id}: limit {limit}, current {current}")]
    PortsExceeded {
        tenant_id: String,
        limit: u32,
        current: u32,
    },
}

pub trait QuotaPolicy: Send + Sync {
    fn check(&self, request: &QuotaRequest) -> Result<(), QuotaError>;
}

pub struct AllowAllQuota;

impl QuotaPolicy for AllowAllQuota {
    fn check(&self, _request: &QuotaRequest) -> Result<(), QuotaError> {
        Ok(())
    }
}

pub struct StaticQuota {
    pub max_proxies: u32,
    pub max_ports: u32,
}

impl QuotaPolicy for StaticQuota {
    fn check(&self, request: &QuotaRequest) -> Result<(), QuotaError> {
        if request.current_proxies + 1 > self.max_proxies {
            return Err(QuotaError::ProxiesExceeded {
                tenant_id: request.tenant_id.clone(),
                limit: self.max_proxies,
                current: request.current_proxies,
            });
        }
        if request.current_ports + request.requested_ports > self.max_ports {
            return Err(QuotaError::PortsExceeded {
                tenant_id: request.tenant_id.clone(),
                limit: self.max_ports,
                current: request.current_ports,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_server_config;

    fn server_config() -> ServerConfig {
        parse_server_config(
            r#"
[server]
control_port = 7000

[auth]
clients = [
  { id = "nas", tenant = "default", token_sha256 = "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b" },
]
"#,
        )
        .expect("valid server config")
    }

    #[tokio::test]
    async fn local_auth_accepts_matching_token() {
        let auth = LocalTokenAuth::from_config(&server_config()).expect("build auth");
        let identity = auth
            .verify_token("default", "nas", "secret")
            .await
            .expect("valid token");
        assert_eq!(identity.client_id, "nas");
    }

    #[tokio::test]
    async fn local_auth_rejects_wrong_token() {
        let auth = LocalTokenAuth::from_config(&server_config()).expect("build auth");
        let err = auth
            .verify_token("default", "nas", "wrong")
            .await
            .expect_err("wrong token");
        assert!(matches!(err, AuthError::TokenMismatch));
    }

    #[tokio::test]
    async fn local_auth_verifies_hmac_proof() {
        let auth = LocalTokenAuth::from_config(&server_config()).expect("build auth");
        let nonce = [9u8; 32];
        let proof = crate::auth::proof("secret", &nonce);
        auth.verify_hmac("default", "nas", &nonce, &proof)
            .await
            .expect("valid proof");

        let wrong = [0u8; 32];
        let err = auth
            .verify_hmac("default", "nas", &nonce, &wrong)
            .await
            .expect_err("wrong proof");
        assert!(matches!(err, AuthError::TokenMismatch));
    }

    #[test]
    fn allocator_prefers_requested_port() {
        let allocator = LocalPortAllocator::new([20000, 20010]).expect("allocator");
        let port = allocator
            .alloc_port("default", "ssh", Some(20005))
            .expect("preferred");
        assert_eq!(port, 20005);
        assert!(allocator.release_port("default", port));
    }

    #[test]
    fn allocator_rejects_conflict_and_out_of_range() {
        let allocator = LocalPortAllocator::new([20000, 20001]).expect("allocator");
        allocator
            .alloc_port("default", "a", Some(20000))
            .expect("first");
        assert!(matches!(
            allocator.alloc_port("default", "b", Some(20000)),
            Err(AllocError::Taken(20000))
        ));
        assert!(matches!(
            allocator.alloc_port("default", "b", Some(30000)),
            Err(AllocError::OutOfRange(30000))
        ));
    }

    #[test]
    fn allocator_reuses_held_lease_for_same_owner() {
        let allocator = LocalPortAllocator::new([20000, 20001]).expect("allocator");
        let port = allocator
            .alloc_port("default", "web", Some(20000))
            .expect("first");
        assert_eq!(
            allocator
                .alloc_port("default", "web", Some(20000))
                .expect("reclaim"),
            port
        );
        assert_eq!(
            allocator
                .alloc_port("default", "web", None)
                .expect("reclaim"),
            port
        );
        assert!(
            !allocator.release_lease("default", "other", port),
            "other proxy must not release this lease"
        );
        assert!(allocator.release_lease("default", "web", port));
        assert_eq!(allocator.leased_count(), 0);
    }

    #[test]
    fn allocator_exhausts_and_recycles() {
        let allocator = LocalPortAllocator::new([20000, 20001]).expect("allocator");
        let a = allocator.alloc_port("default", "a", None).expect("a");
        let b = allocator.alloc_port("default", "b", None).expect("b");
        assert!(matches!(
            allocator.alloc_port("default", "c", None),
            Err(AllocError::Exhausted)
        ));
        assert!(allocator.release_port("default", a));
        assert_eq!(allocator.leased_count(), 1);
        let c = allocator.alloc_port("default", "c", None).expect("c");
        assert_ne!(c, b);
    }

    #[test]
    fn release_is_tenant_scoped() {
        let allocator = LocalPortAllocator::new([20000, 20000]).expect("allocator");
        let port = allocator.alloc_port("t1", "a", None).expect("alloc");
        assert!(!allocator.release_port("t2", port));
        assert!(allocator.release_port("t1", port));
    }

    #[test]
    fn static_quota_blocks_over_limit() {
        let quota = StaticQuota {
            max_proxies: 2,
            max_ports: 3,
        };
        let ok = QuotaRequest {
            tenant_id: "default".into(),
            current_proxies: 1,
            current_ports: 1,
            requested_ports: 1,
        };
        assert!(quota.check(&ok).is_ok());

        let over = QuotaRequest {
            tenant_id: "default".into(),
            current_proxies: 2,
            ..ok.clone()
        };
        assert!(matches!(
            quota.check(&over),
            Err(QuotaError::ProxiesExceeded { .. })
        ));
    }
}
