#![forbid(unsafe_code)]
//! Meatus 内核：配置模型、租户命名空间、以及为多租户与外部控制面预留的五个扩展接缝。

pub mod auth;
pub mod backup;
pub mod config;
pub mod config_edit;
pub mod ids;
mod multiplex;
pub mod net;
pub mod pin;
pub mod pool;
pub mod quic;
pub mod rate;
pub mod relay_bundle;
pub mod seams;
pub mod tenant;
pub mod transport;
pub mod wire;

pub use auth::{proof, token_key, verify_proof_with_key};
pub use config::{
    AcmeChallenge, AcmeSection, AdminApiToken, AdminSection, AdminTlsSection, ApiTokenScope,
    AuthMode, AuthSection, ClientConfig, ClientEntry, ClientLimits, ClientPolicy, ClientSection,
    ConfigError, ConfigFormat, ConfigKind, CtrlSection, HttpSection, LogFormat, LogSection,
    PortReleasePolicy, PortsSection, ProxyOptions, ProxySpec, ProxyType, QuotaAction,
    RelayCertSync, RelayRole, RelaySection, ServerConfig, ServerSection, Socks5Section,
    TransportMode, detect_kind, detect_kind_with_format, load_client_config,
    load_client_config_with_format, load_server_config, load_server_config_with_format,
    parse_client_config, parse_client_config_with_format, parse_server_config,
    parse_server_config_with_format, resolve_config_format,
};
pub use config_edit::{
    remove_client_proxy, remove_server_client, set_admin_api_tokens, upsert_client_proxy,
    upsert_server_client, write_server_config,
};
pub use multiplex::{Inbound, MultiplexError, MultiplexedStream, OpenHandle};
pub use net::{DomainPolicy, TargetPolicy, normalize_domain};
pub use pin::parse_spki_pin;
pub use pool::{ConnectionPool, PoolConnId, PoolError, PoolGuard, PoolSnapshot, PoolStats};
pub use quic::QuicStream;
pub use rate::{RateLimiter, copy_bidirectional_limited};
pub use relay_bundle::{
    BUNDLE_VERSION, BundleError, RelayBundle, RelayBundleBody, RelayMember, RelaySigningKey,
    canonical_body_bytes, check_epoch_rollback, compute_config_hash, current_epoch,
    epoch_state_path, load_bundle, now_rfc3339, parse_bundle, parse_public_key,
    verify_against_config, verify_bundle, verify_for_config, write_bundle, write_epoch_state,
};
pub use seams::{
    AllocError, AllowAllQuota, AuthError, AuthProvider, ClientIdentity, ConfigSnapshot,
    ConfigSource, FileConfigSource, LocalPortAllocator, LocalTokenAuth, NameAllocator,
    NullUsageSink, QuotaError, QuotaPolicy, QuotaRequest, StaticQuota, UsageRecord, UsageSink,
};
pub use tenant::{DEFAULT_TENANT, scoped_key};
pub use transport::{Connection, PoolKey, PoolKeyError, Role};
pub use wire::{WireError, read_frame, write_frame};
