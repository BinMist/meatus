//! TOML 配置模型、环境变量展开与校验。
//!
//! 约定：配置文件里允许写 `${VAR}`，启动时展开；变量缺失直接报错，不做空值兜底。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid YAML: {0}")]
    Yaml(String),
    #[error("missing environment variable: {0}")]
    MissingEnv(String),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

fn invalid(msg: impl Into<String>) -> ConfigError {
    ConfigError::Invalid(msg.into())
}

/// 配置类型，用于 `meatus check` 自动识别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
    Server,
    Client,
}

/// 配置格式；`Auto` 按扩展名和内容识别，默认回退 TOML。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Auto,
    Toml,
    Yaml,
}

impl ConfigFormat {
    #[must_use]
    pub fn from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("yaml" | "yml") => Self::Yaml,
            Some("toml") => Self::Toml,
            _ => Self::Auto,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Toml => "toml",
            Self::Yaml => "yaml",
        }
    }
}

fn default_tenant() -> String {
    crate::tenant::DEFAULT_TENANT.to_string()
}

// ---------------------------------------------------------------------------
// 服务端
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub server: ServerSection,
    /// V1.5 relay 组；缺省即 standalone。
    #[serde(default)]
    pub relay: Option<RelaySection>,
    #[serde(default)]
    pub tenant: TenantSection,
    pub auth: AuthSection,
    #[serde(default)]
    pub ports: PortsSection,
    #[serde(default)]
    pub http: HttpSection,
    #[serde(default)]
    pub acme: AcmeSection,
    #[serde(default)]
    pub socks5: Socks5Section,
    #[serde(default)]
    pub admin: AdminSection,
    #[serde(default)]
    pub ctrl: Option<CtrlSection>,
    #[serde(default)]
    pub log: LogSection,
    #[serde(default)]
    pub audit: AuditSection,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditSection {
    /// 审计 JSONL 目录；缺省为配置文件同级的 `audit/`。
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,
    #[serde(default = "default_control_port")]
    pub control_port: u16,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    /// 是否在控制端口同时监听 UDP/QUIC；默认关闭，不改变 1.3 行为。
    #[serde(default)]
    pub quic: bool,
    /// 收到停止信号后先摘除就绪、等待外部 LB 摘后端的时间；0 表示立即 drain。
    #[serde(default = "default_drain_grace_s")]
    pub drain_grace_s: u64,
}

fn default_bind_addr() -> String {
    "0.0.0.0".to_string()
}

fn default_control_port() -> u16 {
    7000
}

fn default_drain_grace_s() -> u64 {
    5
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            bind_addr: default_bind_addr(),
            control_port: default_control_port(),
            tls_cert: None,
            tls_key: None,
            quic: false,
            drain_grace_s: default_drain_grace_s(),
        }
    }
}

/// V1.5 relay 组配置；缺省时服务端保持 1.4 standalone 行为。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelaySection {
    /// 组内唯一 id，例如 `relay-a`。
    pub id: String,
    /// 组名，例如 `prod`。
    pub group: String,
    /// `primary` 才允许 ACME 下单；`secondary` 只读同步证书。
    pub role: RelayRole,
    /// 对端可访问的控制地址，host:port。
    pub advertise_addr: String,
    /// 签名配置包路径；解析与验签在 G1-02 实现。
    pub bundle: PathBuf,
    /// Ed25519 公钥，格式 `ed25519:BASE64`。
    pub bundle_pubkey: String,
    /// peer 健康/状态监听地址；默认只绑 loopback。
    #[serde(default = "default_relay_health_bind")]
    pub health_bind: String,
    /// peer 健康探测间隔，秒。
    #[serde(default = "default_relay_health_interval_s")]
    pub health_interval_s: u64,
    /// peer 健康探测超时，秒。
    #[serde(default = "default_relay_peer_timeout_s")]
    pub peer_timeout_s: u64,
    /// 连续探测失败达到该次数后，将 peer 从轮转池摘除。
    #[serde(default = "default_relay_rotation_fail_threshold")]
    pub rotation_fail_threshold: u32,
    /// 摘除后连续成功达到该次数，将 peer 恢复回轮转池。
    #[serde(default = "default_relay_rotation_recover_threshold")]
    pub rotation_recover_threshold: u32,
    /// group peer 鉴权密钥文件，32B，权限 0600。
    pub peer_secret_file: PathBuf,
    /// 证书同步模式；G1-02 实现前默认关闭。
    #[serde(default)]
    pub cert_sync: RelayCertSync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RelayRole {
    Primary,
    Secondary,
}

impl RelayRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Secondary => "secondary",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelayCertSync {
    /// 不主动同步；证书由运维或 bundle 手工分发。
    #[default]
    Off,
    /// primary 推给 secondary。
    Push,
    /// secondary 从 primary 拉取。
    Pull,
}

impl RelayCertSync {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Push => "push",
            Self::Pull => "pull",
        }
    }
}

fn default_relay_health_bind() -> String {
    "127.0.0.1:7401".to_string()
}

const fn default_relay_health_interval_s() -> u64 {
    5
}

const fn default_relay_peer_timeout_s() -> u64 {
    5
}

const fn default_relay_rotation_fail_threshold() -> u32 {
    2
}

const fn default_relay_rotation_recover_threshold() -> u32 {
    1
}

impl RelaySection {
    fn validate(&self) -> Result<(), ConfigError> {
        let id = self.id.trim();
        if id.is_empty() {
            return Err(invalid("relay.id must not be empty"));
        }
        if id.len() > 64 || id.chars().any(char::is_whitespace) {
            return Err(invalid(
                "relay.id must be <=64 characters and contain no whitespace",
            ));
        }
        let group = self.group.trim();
        if group.is_empty() {
            return Err(invalid("relay.group must not be empty"));
        }
        if group.len() > 64 || group.chars().any(char::is_whitespace) {
            return Err(invalid(
                "relay.group must be <=64 characters and contain no whitespace",
            ));
        }
        if !is_valid_host_port(&self.advertise_addr) {
            return Err(invalid(
                "relay.advertise_addr must be host:port with a non-zero port",
            ));
        }
        if self.bundle.as_os_str().is_empty() {
            return Err(invalid("relay.bundle must not be empty"));
        }
        let key = self.bundle_pubkey.trim();
        crate::relay_bundle::parse_public_key(key)
            .map_err(|err| invalid(format!("relay.bundle_pubkey: {err}")))?;
        if !is_valid_host_port(&self.health_bind) {
            return Err(invalid(
                "relay.health_bind must be host:port with a non-zero port",
            ));
        }
        if !is_private_bind_addr(&self.health_bind) {
            return Err(invalid(
                "relay.health_bind must bind loopback or a private/link-local address",
            ));
        }
        if !(1..=300).contains(&self.health_interval_s) {
            return Err(invalid("relay.health_interval_s must be 1..=300"));
        }
        if !(1..=300).contains(&self.peer_timeout_s) {
            return Err(invalid("relay.peer_timeout_s must be 1..=300"));
        }
        if !(1..=10).contains(&self.rotation_fail_threshold) {
            return Err(invalid("relay.rotation_fail_threshold must be 1..=10"));
        }
        if !(1..=10).contains(&self.rotation_recover_threshold) {
            return Err(invalid("relay.rotation_recover_threshold must be 1..=10"));
        }
        if self.peer_secret_file.as_os_str().is_empty() {
            return Err(invalid("relay.peer_secret_file must not be empty"));
        }
        Ok(())
    }
}

pub(crate) fn is_valid_host_port(addr: &str) -> bool {
    split_host_port(addr).is_some()
}

fn split_host_port(addr: &str) -> Option<(&str, u16)> {
    let addr = addr.trim();
    if addr.is_empty() || addr.contains("://") {
        return None;
    }
    let (host, port) = addr.rsplit_once(':')?;
    let host = host.trim();
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return None;
    }
    let port = port.parse::<u16>().ok()?;
    (port > 0).then_some((host, port))
}

fn is_private_bind_addr(addr: &str) -> bool {
    let Some((host, _)) = split_host_port(addr) else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let Ok(ip) = host.parse::<std::net::IpAddr>() else {
        return false;
    };
    match ip {
        std::net::IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        std::net::IpAddr::V6(ip) => {
            let segments = ip.segments();
            ip.is_loopback() || (segments[0] & 0xfe00) == 0xfc00 || (segments[0] & 0xffc0) == 0xfe80
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantSection {
    #[serde(default = "default_tenant")]
    pub default: String,
}

impl Default for TenantSection {
    fn default() -> Self {
        Self {
            default: default_tenant(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    Hmac,
    Mtls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthProviderKind {
    Local,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSection {
    #[serde(default = "default_auth_mode")]
    pub mode: AuthMode,
    #[serde(default = "default_auth_provider")]
    pub provider: AuthProviderKind,
    /// 单客户端场景的简写；多客户端请用 `clients[].token_sha256`。
    pub token_sha256: Option<String>,
    #[serde(default)]
    pub clients: Vec<ClientEntry>,
}

fn default_auth_mode() -> AuthMode {
    AuthMode::Hmac
}

fn default_auth_provider() -> AuthProviderKind {
    AuthProviderKind::Local
}

impl Default for AuthSection {
    fn default() -> Self {
        Self {
            mode: default_auth_mode(),
            provider: default_auth_provider(),
            token_sha256: None,
            clients: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientEntry {
    pub id: String,
    #[serde(default = "default_tenant")]
    pub tenant: String,
    pub token_sha256: Option<String>,
    pub allow_ports: Option<[u16; 2]>,
    /// 允许创建的 proxy 类型；空表示不限制。
    #[serde(default)]
    pub allow_types: Vec<ProxyType>,
    /// 允许的域名白名单，支持精确域名与 `*.suffix`；空表示不限制。
    #[serde(default)]
    pub allow_domains: Vec<String>,
    /// 该 client 最多同时存在的 proxy 数（跨 run 统计）；空表示不限制。
    pub max_proxies: Option<u32>,
    /// 客户端级配额与限速；proxy 级 `options` 可进一步收紧。
    #[serde(default)]
    pub limits: ClientLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuotaAction {
    #[default]
    Reject,
    Throttle,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientLimits {
    /// 带宽上限，单位 bytes/s；空或 0 表示不限速。
    pub rate_limit_bps: Option<u64>,
    /// 自然月流量上限，单位 bytes；空或 0 表示不限额。
    pub monthly_bytes: Option<u64>,
    /// 并发连接上限；空或 0 表示使用默认上限。
    pub max_conns: Option<u32>,
    /// 新建连接速率上限，单位连接/秒；空或 0 表示不限制。
    pub conn_rate_per_s: Option<u32>,
    /// 月配额耗尽后的动作；空表示 `reject`。
    pub quota_action: Option<QuotaAction>,
}

/// 认证成功后固化的客户端授权快照；热加载只影响新建立的会话。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientPolicy {
    pub allow_types: Vec<ProxyType>,
    pub allow_ports: Option<[u16; 2]>,
    pub allow_domains: Vec<String>,
    pub max_proxies: Option<u32>,
    pub limits: ClientLimits,
}

impl ClientPolicy {
    #[must_use]
    pub fn allows_type(&self, kind: ProxyType) -> bool {
        self.allow_types.is_empty() || self.allow_types.contains(&kind)
    }

    #[must_use]
    pub fn allows_port(&self, port: u16) -> bool {
        self.allow_ports
            .is_none_or(|[start, end]| port >= start && port <= end)
    }

    #[must_use]
    pub fn allows_domains(&self, domains: &[String]) -> bool {
        if self.allow_domains.is_empty() {
            return true;
        }
        let Ok(policy) = crate::net::DomainPolicy::from_allow_domains(&self.allow_domains) else {
            return false;
        };
        !policy.is_empty()
            && domains
                .iter()
                .all(|domain| policy.allows_claimed_domain(domain))
    }

    #[must_use]
    pub fn monthly_bytes(&self) -> u64 {
        self.limits.monthly_bytes.unwrap_or(0)
    }

    #[must_use]
    pub fn quota_action(&self) -> QuotaAction {
        self.limits.quota_action.unwrap_or_default()
    }

    #[must_use]
    pub fn rate_limit_bps(&self) -> u64 {
        self.limits.rate_limit_bps.unwrap_or(0)
    }

    #[must_use]
    pub fn max_conns(&self) -> u32 {
        self.limits.max_conns.unwrap_or(0)
    }

    #[must_use]
    pub fn conn_rate_per_s(&self) -> u32 {
        self.limits.conn_rate_per_s.unwrap_or(0)
    }
}

impl ClientEntry {
    #[must_use]
    pub fn policy(&self) -> ClientPolicy {
        ClientPolicy {
            allow_types: self.allow_types.clone(),
            allow_ports: self.allow_ports,
            allow_domains: self.allow_domains.clone(),
            max_proxies: self.max_proxies,
            limits: self.limits.clone(),
        }
    }
}

impl AuthSection {
    /// `(client_id, tenant_id, token_sha256)` 三元组，供本地鉴权器构建。
    #[must_use]
    pub fn credential_entries(&self) -> Vec<(String, String, String)> {
        if self.clients.is_empty() {
            return self
                .token_sha256
                .as_ref()
                .map(|hash| vec![(default_tenant(), default_tenant(), hash.clone())])
                .unwrap_or_default();
        }
        self.clients
            .iter()
            .filter_map(|client| {
                client
                    .token_sha256
                    .as_ref()
                    .map(|hash| (client.id.clone(), client.tenant.clone(), hash.clone()))
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PortReleasePolicy {
    /// 客户端断开后立即释放公网端口。
    #[default]
    Immediate,
    /// 保留端口 `hold_secs` 秒，让原 client 重连时优先取回。
    Hold,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortsSection {
    pub range: [u16; 2],
    /// 客户端离线后公网端口释放策略；默认立即释放。
    #[serde(default)]
    pub release_policy: PortReleasePolicy,
    /// `release_policy = "hold"` 时的保留秒数。
    #[serde(default = "default_port_hold_secs")]
    pub hold_secs: u64,
}

impl Default for PortsSection {
    fn default() -> Self {
        Self {
            range: [20000, 30000],
            release_policy: PortReleasePolicy::Immediate,
            hold_secs: default_port_hold_secs(),
        }
    }
}

fn default_port_hold_secs() -> u64 {
    300
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_http_bind")]
    pub bind: String,
    #[serde(default = "default_https_bind")]
    pub https_bind: String,
    #[serde(default)]
    pub domain: String,
}

fn default_http_bind() -> String {
    "0.0.0.0:80".to_string()
}

fn default_https_bind() -> String {
    "0.0.0.0:443".to_string()
}

impl Default for HttpSection {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: default_http_bind(),
            https_bind: default_https_bind(),
            domain: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum AcmeChallenge {
    #[serde(rename = "http-01")]
    Http01,
    #[serde(rename = "tls-alpn-01")]
    TlsAlpn01,
    #[serde(rename = "dns-01")]
    Dns01,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcmeSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_acme_directory")]
    pub directory: String,
    pub email: Option<String>,
    #[serde(default = "default_acme_challenge")]
    pub challenge: AcmeChallenge,
    #[serde(default = "default_acme_cert_dir")]
    pub cert_dir: PathBuf,
    #[serde(default)]
    pub staging: bool,
    pub dns_provider: Option<String>,
    pub dns_api_token: Option<String>,
}

fn default_acme_directory() -> String {
    "https://acme-v02.api.letsencrypt.org/directory".to_string()
}

fn default_acme_challenge() -> AcmeChallenge {
    AcmeChallenge::Http01
}

fn default_acme_cert_dir() -> PathBuf {
    PathBuf::from("/etc/meatus/acme")
}

impl Default for AcmeSection {
    fn default() -> Self {
        Self {
            enabled: false,
            directory: default_acme_directory(),
            email: None,
            challenge: default_acme_challenge(),
            cert_dir: default_acme_cert_dir(),
            staging: false,
            dns_provider: None,
            dns_api_token: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Socks5Section {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_socks5_bind")]
    pub bind: String,
}

fn default_socks5_bind() -> String {
    "0.0.0.0:2080".to_string()
}

impl Default for Socks5Section {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: default_socks5_bind(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminSection {
    #[serde(default = "default_admin_bind")]
    pub bind: String,
    #[serde(default = "default_panel")]
    pub panel: bool,
    pub admin_token: Option<String>,
    /// 本地用户与角色；旧 `admin_token` 仍映射为 owner。
    #[serde(default)]
    pub users: Vec<AdminUser>,
    /// 自动化 API Token；只存 SHA256，按 scope 授权。
    #[serde(default)]
    pub api_tokens: Vec<AdminApiToken>,
    #[serde(default)]
    pub tls: AdminTlsSection,
    /// 仅当管理面位于可信反向代理之后时开启；默认不信任转发头。
    #[serde(default)]
    pub trust_forwarded_proto: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdminRole {
    Owner,
    Operator,
    Viewer,
    Auditor,
}

impl AdminRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Operator => "operator",
            Self::Viewer => "viewer",
            Self::Auditor => "auditor",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminUser {
    pub name: String,
    pub role: AdminRole,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub token_sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
pub enum ApiTokenScope {
    #[serde(rename = "status:read")]
    StatusRead,
    #[serde(rename = "proxies:write")]
    ProxiesWrite,
    #[serde(rename = "certs:read")]
    CertsRead,
    #[serde(rename = "audit:read")]
    AuditRead,
}

impl ApiTokenScope {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StatusRead => "status:read",
            Self::ProxiesWrite => "proxies:write",
            Self::CertsRead => "certs:read",
            Self::AuditRead => "audit:read",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminApiToken {
    pub name: String,
    pub token_sha256: String,
    #[serde(default)]
    pub scopes: Vec<ApiTokenScope>,
    #[serde(default)]
    pub created_at_ms: Option<i64>,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminTlsSection {
    #[serde(default)]
    pub enabled: bool,
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
    #[serde(default = "default_hsts")]
    pub hsts: bool,
}

fn default_hsts() -> bool {
    true
}

impl Default for AdminTlsSection {
    fn default() -> Self {
        Self {
            enabled: false,
            cert: None,
            key: None,
            hsts: default_hsts(),
        }
    }
}

fn default_admin_bind() -> String {
    "127.0.0.1:7400".to_string()
}

fn default_panel() -> bool {
    true
}

impl Default for AdminSection {
    fn default() -> Self {
        Self {
            bind: default_admin_bind(),
            panel: default_panel(),
            admin_token: None,
            users: Vec::new(),
            api_tokens: Vec::new(),
            tls: AdminTlsSection::default(),
            trust_forwarded_proto: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CtrlSection {
    pub url: String,
    pub relay_id: Option<String>,
    pub mtls_cert: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogSection {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_log_format")]
    pub format: LogFormat,
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_format() -> LogFormat {
    LogFormat::Text
}

impl Default for LogSection {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: default_log_format(),
        }
    }
}

impl ServerConfig {
    /// 校验配置，不通过就不该启动。
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(relay) = &self.relay {
            relay.validate()?;
            if relay.role == RelayRole::Secondary && self.acme.enabled {
                return Err(invalid(
                    "relay.role = \"secondary\" must not enable acme; secondary loads synced certs",
                ));
            }
            if relay.role == RelayRole::Primary
                && relay.cert_sync != RelayCertSync::Off
                && !self.acme.enabled
            {
                return Err(invalid(
                    "relay.role = \"primary\" with cert_sync requires acme.enabled = true",
                ));
            }
        }

        if self.server.control_port < 1024 {
            return Err(invalid("server.control_port must be >= 1024"));
        }
        if self.server.drain_grace_s > 60 {
            return Err(invalid("server.drain_grace_s must be 0..=60"));
        }
        if self.server.tls_cert.is_some() != self.server.tls_key.is_some() {
            return Err(invalid(
                "server.tls_cert and server.tls_key must be set together",
            ));
        }

        let [start, end] = self.ports.range;
        if start < 1024 || start > end {
            return Err(invalid(format!(
                "ports.range must satisfy 1024 <= start <= end, got {start}..={end}"
            )));
        }
        if self.ports.release_policy == PortReleasePolicy::Hold
            && !(1..=86_400).contains(&self.ports.hold_secs)
        {
            return Err(invalid(
                "ports.hold_secs must be 1..=86400 when ports.release_policy = \"hold\"",
            ));
        }

        self.auth.validate()?;

        if self.http.enabled {
            if self.http.domain.trim().is_empty() {
                return Err(invalid("http.domain is required when http.enabled = true"));
            }
            if !self.http.bind.contains(':') || !self.http.https_bind.contains(':') {
                return Err(invalid("http.bind and http.https_bind must be host:port"));
            }
        }

        if self.acme.enabled {
            if self.acme.email.as_deref().unwrap_or("").trim().is_empty() {
                return Err(invalid("acme.email is required when acme.enabled = true"));
            }
            if self.acme.directory.trim().is_empty() {
                return Err(invalid("acme.directory must not be empty"));
            }
            if matches!(self.acme.challenge, AcmeChallenge::Dns01) {
                let provider = self
                    .acme
                    .dns_provider
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                let token = self.acme.dns_api_token.as_deref().unwrap_or("").trim();
                if provider.is_empty() || token.is_empty() {
                    return Err(invalid(
                        "acme.challenge = \"dns-01\" requires dns_provider and dns_api_token",
                    ));
                }
                if provider != "cloudflare" && provider != "dnspod" {
                    return Err(invalid(format!(
                        "acme.dns_provider = {provider:?} is not supported yet; \
                         supported providers: \"cloudflare\", \"dnspod\""
                    )));
                }
                if provider == "dnspod" && !token.contains(',') {
                    return Err(invalid(
                        "acme.dns_api_token for dnspod must use the \"ID,Token\" format",
                    ));
                }
            }
        }

        if self.socks5.enabled && !self.socks5.bind.contains(':') {
            return Err(invalid("socks5.bind must be host:port"));
        }

        if self
            .admin
            .admin_token
            .as_deref()
            .is_some_and(|token| token.trim().is_empty())
        {
            return Err(invalid("admin.admin_token must not be empty"));
        }
        if !is_loopback_bind(&self.admin.bind)
            && self.admin.admin_token.is_none()
            && self.admin.users.is_empty()
            && self.admin.api_tokens.is_empty()
        {
            return Err(invalid(
                "admin.bind is not loopback; admin_token, admin.users or admin.api_tokens is required in that case",
            ));
        }
        let mut names = std::collections::BTreeSet::new();
        for user in &self.admin.users {
            let name = user.name.trim();
            if name.is_empty() {
                return Err(invalid("admin.users name must not be empty"));
            }
            if !names.insert(name.to_string()) {
                return Err(invalid(format!("duplicate admin user name: {name}")));
            }
            let plain = user
                .token
                .as_deref()
                .filter(|value| !value.trim().is_empty());
            let hashed = user
                .token_sha256
                .as_deref()
                .filter(|value| !value.trim().is_empty());
            if plain.is_none() && hashed.is_none() {
                return Err(invalid(format!(
                    "admin user {name} requires token or token_sha256"
                )));
            }
            if let Some(hash) = hashed
                && (hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
            {
                return Err(invalid(format!(
                    "admin user {name} token_sha256 must be 64 hex characters"
                )));
            }
        }
        let mut api_names = std::collections::BTreeSet::new();
        for token in &self.admin.api_tokens {
            let name = token.name.trim();
            if name.is_empty() {
                return Err(invalid("admin.api_tokens name must not be empty"));
            }
            if !api_names.insert(name.to_string()) {
                return Err(invalid(format!("duplicate admin api token name: {name}")));
            }
            if token.token_sha256.len() != 64
                || !token
                    .token_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(invalid(format!(
                    "admin api token {name} token_sha256 must be 64 hex characters"
                )));
            }
            if token.scopes.is_empty() {
                return Err(invalid(format!(
                    "admin api token {name} requires at least one scope"
                )));
            }
            let mut scopes = std::collections::BTreeSet::new();
            for scope in &token.scopes {
                if !scopes.insert(*scope) {
                    return Err(invalid(format!(
                        "admin api token {name} has duplicate scope {}",
                        scope.as_str()
                    )));
                }
            }
            if token.expires_at_ms.is_some_and(|value| value < 0) {
                return Err(invalid(format!(
                    "admin api token {name} expires_at_ms must not be negative"
                )));
            }
        }
        if self.admin.tls.enabled {
            if self.admin.tls.cert.is_none() || self.admin.tls.key.is_none() {
                return Err(invalid(
                    "admin.tls.cert and admin.tls.key are required when admin.tls.enabled = true",
                ));
            }
        } else if self.admin.tls.cert.is_some() || self.admin.tls.key.is_some() {
            return Err(invalid(
                "admin.tls.cert/key are set but admin.tls.enabled = false",
            ));
        }
        if self.admin.tls.enabled && !self.admin.bind.contains(':') {
            return Err(invalid("admin.bind must be host:port"));
        }

        if let Some(ctrl) = &self.ctrl
            && !ctrl.url.starts_with("http://")
            && !ctrl.url.starts_with("https://")
        {
            return Err(invalid("ctrl.url must start with http:// or https://"));
        }

        Ok(())
    }

    /// 真正启动服务端前的额外校验：M1 必须启用 TLS。
    pub fn validate_runtime(&self) -> Result<(), ConfigError> {
        self.validate()?;
        if let Some(relay) = &self.relay {
            self.validate_relay_peer_secret(relay)?;
        }
        if self.acme.enabled {
            if self.acme.email.as_deref().unwrap_or("").trim().is_empty() {
                return Err(invalid("acme.email is required when acme.enabled = true"));
            }
            match self.acme.challenge {
                AcmeChallenge::Http01 => {
                    if !self.http.enabled {
                        return Err(invalid(
                            "acme.challenge = \"http-01\" requires http.enabled = true",
                        ));
                    }
                }
                AcmeChallenge::Dns01 => {
                    // 泛域名走 DNS-01，不依赖 :80；精确域名仍复用 HTTP-01，
                    // 若 http.enabled = false 则由 ensure_domain 返回 false。
                }
                AcmeChallenge::TlsAlpn01 => {
                    return Err(invalid(
                        "acme.challenge = \"tls-alpn-01\" is not wired yet; \
                         use \"http-01\" or \"dns-01\"",
                    ));
                }
            }
        }
        if self.server.tls_cert.is_none() || self.server.tls_key.is_none() {
            return Err(invalid(
                "server.tls_cert and server.tls_key are required to serve TLS in M1",
            ));
        }
        Ok(())
    }

    fn validate_relay_peer_secret(&self, relay: &RelaySection) -> Result<(), ConfigError> {
        let path = &relay.peer_secret_file;
        let meta = fs::metadata(path)
            .map_err(|err| invalid(format!("relay.peer_secret_file {}: {err}", path.display())))?;
        if !meta.is_file() {
            return Err(invalid(format!(
                "relay.peer_secret_file {} must be a regular file",
                path.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o777 != 0o600 {
                return Err(invalid(format!(
                    "relay.peer_secret_file {} must have mode 0600",
                    path.display()
                )));
            }
        }
        let secret = fs::read(path)
            .map_err(|err| invalid(format!("relay.peer_secret_file {}: {err}", path.display())))?;
        if secret.len() != 32 {
            return Err(invalid(format!(
                "relay.peer_secret_file {} must contain exactly 32 bytes, got {}",
                path.display(),
                secret.len()
            )));
        }
        for (field, value) in [
            ("admin.admin_token", self.admin.admin_token.as_deref()),
            ("acme.dns_api_token", self.acme.dns_api_token.as_deref()),
        ] {
            if value.is_some_and(|value| value.as_bytes() == secret.as_slice()) {
                return Err(invalid(format!(
                    "relay.peer_secret_file must not reuse {field}"
                )));
            }
        }
        Ok(())
    }
}

impl AuthSection {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.mode == AuthMode::Mtls {
            return Ok(());
        }

        if self.clients.is_empty() {
            if self.token_sha256.as_deref().unwrap_or("").trim().is_empty() {
                return Err(invalid(
                    "auth requires clients[].token_sha256 or auth.token_sha256",
                ));
            }
            return Ok(());
        }

        let mut seen = HashSet::new();
        for client in &self.clients {
            if client.id.trim().is_empty() {
                return Err(invalid("auth.clients[].id must not be empty"));
            }
            if !seen.insert((client.tenant.clone(), client.id.clone())) {
                return Err(invalid(format!(
                    "duplicate client id {} in tenant {}",
                    client.id, client.tenant
                )));
            }
            if client
                .token_sha256
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                return Err(invalid(format!(
                    "auth.clients[{}].token_sha256 is required",
                    client.id
                )));
            }
            if let Some([start, end]) = client.allow_ports
                && (start < 1024 || start > end)
            {
                return Err(invalid(format!(
                    "auth.clients[{}].allow_ports must satisfy 1024 <= start <= end",
                    client.id
                )));
            }
            if client
                .max_proxies
                .is_some_and(|value| value == 0 || value > 1024)
            {
                return Err(invalid(format!(
                    "auth.clients[{}].max_proxies must be 1..=1024",
                    client.id
                )));
            }
            if client.allow_types.len() > 5 {
                return Err(invalid(format!(
                    "auth.clients[{}].allow_types supports at most 5 entries",
                    client.id
                )));
            }
            let mut seen_types = HashSet::new();
            if client
                .allow_types
                .iter()
                .any(|kind| !seen_types.insert(*kind))
            {
                return Err(invalid(format!(
                    "auth.clients[{}].allow_types contains duplicates",
                    client.id
                )));
            }
            if client.allow_domains.len() > 16 {
                return Err(invalid(format!(
                    "auth.clients[{}].allow_domains supports at most 16 entries",
                    client.id
                )));
            }
            crate::net::DomainPolicy::from_allow_domains(&client.allow_domains).map_err(|err| {
                invalid(format!("auth.clients[{}].allow_domains: {err}", client.id))
            })?;
            if client.limits.max_conns.is_some_and(|value| value > 65535) {
                return Err(invalid(format!(
                    "auth.clients[{}].limits.max_conns must be <=65535",
                    client.id
                )));
            }
            if client
                .limits
                .conn_rate_per_s
                .is_some_and(|value| value > 100_000)
            {
                return Err(invalid(format!(
                    "auth.clients[{}].limits.conn_rate_per_s must be <=100000",
                    client.id
                )));
            }
        }
        Ok(())
    }
}

fn is_loopback_bind(bind: &str) -> bool {
    let host = bind.rsplit_once(':').map_or(bind, |(host, _)| host);
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

// ---------------------------------------------------------------------------
// 客户端
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub client: ClientSection,
    #[serde(rename = "proxy", default)]
    pub proxies: Vec<ProxySpec>,
    #[serde(default)]
    pub log: LogSection,
    #[serde(default)]
    pub audit: AuditSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientSection {
    /// 单入口地址；`server_addrs` 非空时被忽略。
    #[serde(default)]
    pub server_addr: String,
    /// 多入口地址列表；非空时优先于 `server_addr`。
    #[serde(default)]
    pub server_addrs: Vec<String>,
    /// 控制面传输：`tcp`（默认）、`quic`、`auto`。
    #[serde(default)]
    pub transport: TransportMode,
    pub server_name: Option<String>,
    #[serde(default = "default_client_id")]
    pub client_id: String,
    #[serde(default = "default_tenant")]
    pub tenant: String,
    /// 自签 / 私有 CA 场景下信任的根证书；不填则用系统根证书。
    pub server_ca_cert: Option<PathBuf>,
    pub token: String,
    /// SPKI SHA-256 pin；配置后仍会正常校验证书链，再额外固定公钥。
    pub server_public_key_pin: Option<String>,
    /// 数据连接池；`[client.pool]`，默认关闭。
    #[serde(default)]
    pub pool: PoolSection,
    /// relay 组多宿主；`[client.relay]`，默认关闭并保持 1.4 顺序切换。
    #[serde(default)]
    pub relay: ClientRelaySection,
}

/// 客户端 relay 组多宿主配置。[client.relay]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientRelaySection {
    /// relay 组名；启用 multi_home 时必填，服务端回显必须一致。
    #[serde(default)]
    pub group: String,
    /// true 时对 server_addrs 的每个成员维持独立控制连接。
    #[serde(default)]
    pub multi_home: bool,
    /// 成员状态刷新间隔，秒；只影响状态汇总，不改变控制面心跳。
    #[serde(default = "default_client_relay_health_interval_s")]
    pub health_interval_s: u64,
}

fn default_client_relay_health_interval_s() -> u64 {
    5
}

impl Default for ClientRelaySection {
    fn default() -> Self {
        Self {
            group: String::new(),
            multi_home: false,
            health_interval_s: default_client_relay_health_interval_s(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportMode {
    #[default]
    Tcp,
    Quic,
    Auto,
}

impl std::fmt::Display for TransportMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Tcp => "tcp",
            Self::Quic => "quic",
            Self::Auto => "auto",
        })
    }
}

/// 客户端数据连接池。[client.pool]，默认关闭。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_pool_size")]
    pub size: u32,
    #[serde(default = "default_pool_max_streams")]
    pub max_streams_per_conn: u32,
    #[serde(default = "default_pool_idle_timeout_s")]
    pub idle_timeout_s: u32,
}

impl Default for PoolSection {
    fn default() -> Self {
        Self {
            enabled: false,
            size: default_pool_size(),
            max_streams_per_conn: default_pool_max_streams(),
            idle_timeout_s: default_pool_idle_timeout_s(),
        }
    }
}

const fn default_pool_size() -> u32 {
    4
}

const fn default_pool_max_streams() -> u32 {
    64
}

const fn default_pool_idle_timeout_s() -> u32 {
    300
}

fn default_client_id() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyType {
    Tcp,
    Udp,
    Http,
    Https,
    Socks5,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyOptions {
    pub rate_limit_bps: Option<u64>,
    pub idle_timeout_s: Option<u32>,
    pub max_conns: Option<u32>,
    /// proxy 级自然月流量上限；与 client 级取较小值。
    pub monthly_bytes: Option<u64>,
    /// proxy 级新建连接速率；与 client 级取较小值。
    pub conn_rate_per_s: Option<u32>,
    /// proxy 级月配额动作，覆盖 client 级。
    pub quota_action: Option<QuotaAction>,
    #[serde(default)]
    pub allow_cidrs: Vec<String>,
    #[serde(default)]
    pub allow_domains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxySpec {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ProxyType,
    pub local_addr: String,
    pub remote_port: Option<u16>,
    #[serde(default)]
    pub custom_domains: Vec<String>,
    #[serde(default)]
    pub options: ProxyOptions,
}

/// 拼接 `host:port`，对 IPv6 字面量自动加方括号。
#[must_use]
pub fn host_port(host: &str, port: u16) -> String {
    if host.starts_with('[') {
        format!("{host}:{port}")
    } else if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// 规范化客户端拨号地址：裸 IPv6 字面量自动补方括号，已带方括号的保持原样。
#[must_use]
pub fn normalize_host_port(addr: &str) -> String {
    let addr = addr.trim();
    if addr.starts_with('[') || addr.matches(':').count() <= 1 {
        return addr.to_string();
    }
    match addr.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() && host.contains(':') => {
            format!("[{host}]:{port}")
        }
        _ => addr.to_string(),
    }
}

impl ClientConfig {
    /// 代理名到配置的映射，数据面按 `StreamHeader.proxy_name` 查找。
    #[must_use]
    pub fn proxy_map(&self) -> HashMap<String, ProxySpec> {
        self.proxies
            .iter()
            .map(|proxy| (proxy.name.clone(), proxy.clone()))
            .collect()
    }

    /// 实际拨号列表：`server_addrs` 非空时优先，否则退回 `server_addr`。
    #[must_use]
    pub fn dial_addrs(&self) -> Vec<String> {
        let raw = if self.client.server_addrs.is_empty() {
            vec![self.client.server_addr.clone()]
        } else {
            self.client.server_addrs.clone()
        };
        raw.into_iter()
            .map(|addr| normalize_host_port(&addr))
            .collect()
    }

    /// 从首个拨号地址提取主机名，用于 TLS SNI / 证书校验。
    #[must_use]
    pub fn server_host(&self) -> &str {
        let addr = self
            .client
            .server_addrs
            .first()
            .map(String::as_str)
            .unwrap_or(self.client.server_addr.as_str());
        if let Some(rest) = addr.strip_prefix('[') {
            return rest.split(']').next().unwrap_or(rest);
        }
        addr.rsplit_once(':').map_or(addr, |(host, _)| host)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let targets = self.dial_addrs();
        if targets.iter().all(|target| target.trim().is_empty()) {
            return Err(invalid(
                "client.server_addr or client.server_addrs must be set",
            ));
        }
        let mut seen = HashSet::new();
        for target in &targets {
            let target = target.trim();
            if !seen.insert(target.to_string()) {
                return Err(invalid(format!("duplicate client address: {target}")));
            }
            if !target.contains(':') {
                return Err(invalid(format!(
                    "client address must be host:port, got {target:?}"
                )));
            }
            let port = target.rsplit_once(':').map(|(_, port)| port).unwrap_or("");
            if port.parse::<u16>().is_err() {
                return Err(invalid(format!(
                    "client address must end with a valid port, got {target:?}"
                )));
            }
        }
        if self.client.client_id.trim().is_empty() {
            return Err(invalid("client.client_id must not be empty"));
        }
        if self.client.tenant.trim().is_empty() {
            return Err(invalid("client.tenant must not be empty"));
        }
        if self.client.token.trim().is_empty() {
            return Err(invalid("client.token must not be empty"));
        }
        let pool = &self.client.pool;
        if !(1..=16).contains(&pool.size) {
            return Err(invalid(format!(
                "client.pool.size must be between 1 and 16, got {}",
                pool.size
            )));
        }
        if !(1..=1024).contains(&pool.max_streams_per_conn) {
            return Err(invalid(format!(
                "client.pool.max_streams_per_conn must be between 1 and 1024, got {}",
                pool.max_streams_per_conn
            )));
        }
        if !(1..=3600).contains(&pool.idle_timeout_s) {
            return Err(invalid(format!(
                "client.pool.idle_timeout_s must be between 1 and 3600, got {}",
                pool.idle_timeout_s
            )));
        }
        let relay = &self.client.relay;
        if !(1..=300).contains(&relay.health_interval_s) {
            return Err(invalid(format!(
                "client.relay.health_interval_s must be between 1 and 300, got {}",
                relay.health_interval_s
            )));
        }
        let group = relay.group.trim();
        if !group.is_empty() {
            if group.len() > 64 || group.chars().any(char::is_whitespace) {
                return Err(invalid(
                    "client.relay.group must be <=64 characters and contain no whitespace",
                ));
            }
            if relay.multi_home && targets.len() < 2 {
                return Err(invalid(
                    "client.relay.multi_home requires at least two client.server_addrs",
                ));
            }
        } else if relay.multi_home {
            return Err(invalid(
                "client.relay.group is required when client.relay.multi_home = true",
            ));
        }
        if let Some(pin) = &self.client.server_public_key_pin {
            crate::pin::parse_spki_pin(pin)
                .map_err(|err| invalid(format!("invalid client.server_public_key_pin: {err}")))?;
        }
        if let Some(name) = &self.client.server_name
            && name.trim().is_empty()
        {
            return Err(invalid("client.server_name must not be empty when set"));
        }

        let mut names = HashSet::new();
        let mut domains = HashSet::new();
        for proxy in &self.proxies {
            if proxy.name.trim().is_empty() || proxy.name.contains('/') {
                return Err(invalid(format!(
                    "proxy.name must be non-empty without '/', got {:?}",
                    proxy.name
                )));
            }
            if !names.insert(proxy.name.clone()) {
                return Err(invalid(format!("duplicate proxy name: {}", proxy.name)));
            }
            if !proxy.local_addr.contains(':') {
                return Err(invalid(format!(
                    "proxy[{}].local_addr must be host:port",
                    proxy.name
                )));
            }
            // 0 means "let the server allocate from the port pool" and is the
            // documented value for http/https proxies.  Any explicitly chosen
            // port still has to stay outside the privileged range.
            if let Some(port) = proxy.remote_port
                && port != 0
                && port < 1024
            {
                return Err(invalid(format!(
                    "proxy[{}].remote_port must be 0 or >= 1024",
                    proxy.name
                )));
            }
            if matches!(proxy.kind, ProxyType::Http | ProxyType::Https)
                && proxy.custom_domains.is_empty()
            {
                return Err(invalid(format!(
                    "proxy[{}] of type {:?} requires custom_domains",
                    proxy.name, proxy.kind
                )));
            }
            if proxy.kind == ProxyType::Socks5 {
                if !proxy.custom_domains.is_empty() {
                    return Err(invalid(format!(
                        "proxy[{}] of type socks5 must not set custom_domains",
                        proxy.name
                    )));
                }
                crate::net::TargetPolicy::from_allow_cidrs(&proxy.options.allow_cidrs).map_err(
                    |err| invalid(format!("proxy[{}].options.allow_cidrs: {err}", proxy.name)),
                )?;
                crate::net::DomainPolicy::from_allow_domains(&proxy.options.allow_domains)
                    .map_err(|err| {
                        invalid(format!(
                            "proxy[{}].options.allow_domains: {err}",
                            proxy.name
                        ))
                    })?;
            } else if !proxy.options.allow_domains.is_empty() {
                return Err(invalid(format!(
                    "proxy[{}].options.allow_domains is only supported for socks5",
                    proxy.name
                )));
            }
            for domain in &proxy.custom_domains {
                if domain.trim().is_empty() || domain.contains(char::is_whitespace) {
                    return Err(invalid(format!(
                        "proxy[{}] has an invalid domain: {domain:?}",
                        proxy.name
                    )));
                }
                if domain != &domain.to_ascii_lowercase() {
                    return Err(invalid(format!(
                        "proxy[{}] domain must be lowercase: {domain}",
                        proxy.name
                    )));
                }
                if !domains.insert(domain.clone()) {
                    return Err(invalid(format!("duplicate domain: {domain}")));
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 加载
// ---------------------------------------------------------------------------

pub fn load_server_config(path: impl AsRef<Path>) -> Result<ServerConfig, ConfigError> {
    load_server_config_with_format(path, ConfigFormat::Auto)
}

pub fn load_server_config_with_format(
    path: impl AsRef<Path>,
    format: ConfigFormat,
) -> Result<ServerConfig, ConfigError> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let format = resolve_format(&raw, path, format);
    parse_server_config_at(&raw, format, Some(path))
}

pub fn load_client_config(path: impl AsRef<Path>) -> Result<ClientConfig, ConfigError> {
    load_client_config_with_format(path, ConfigFormat::Auto)
}

pub fn load_client_config_with_format(
    path: impl AsRef<Path>,
    format: ConfigFormat,
) -> Result<ClientConfig, ConfigError> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let format = resolve_format(&raw, path, format);
    parse_client_config_at(&raw, format, Some(path))
}

pub fn parse_server_config(raw: &str) -> Result<ServerConfig, ConfigError> {
    parse_server_config_at(raw, ConfigFormat::Toml, None)
}

pub fn parse_server_config_with_format(
    raw: &str,
    format: ConfigFormat,
) -> Result<ServerConfig, ConfigError> {
    let format = if format == ConfigFormat::Auto {
        detect_format(raw)
    } else {
        format
    };
    parse_server_config_at(raw, format, None)
}

/// 只反序列化、不跑语义校验；用于从“已改坏但仍可解析”的配置中提取备份恢复所需的秘密值。
pub fn parse_server_config_unchecked_with_format(
    raw: &str,
    format: ConfigFormat,
) -> Result<ServerConfig, ConfigError> {
    let format = if format == ConfigFormat::Auto {
        detect_format(raw)
    } else {
        format
    };
    let expanded = expand_env(raw)?;
    match format {
        ConfigFormat::Toml | ConfigFormat::Auto => Ok(toml::from_str(&expanded)?),
        ConfigFormat::Yaml => serde_yaml::from_str(&expanded).map_err(|err| yaml_error(err, None)),
    }
}

/// 只反序列化、不跑语义校验的客户端配置解析。
pub fn parse_client_config_unchecked_with_format(
    raw: &str,
    format: ConfigFormat,
) -> Result<ClientConfig, ConfigError> {
    let format = if format == ConfigFormat::Auto {
        detect_format(raw)
    } else {
        format
    };
    let expanded = expand_env(raw)?;
    match format {
        ConfigFormat::Toml | ConfigFormat::Auto => Ok(toml::from_str(&expanded)?),
        ConfigFormat::Yaml => serde_yaml::from_str(&expanded).map_err(|err| yaml_error(err, None)),
    }
}

pub fn parse_client_config(raw: &str) -> Result<ClientConfig, ConfigError> {
    parse_client_config_at(raw, ConfigFormat::Toml, None)
}

pub fn parse_client_config_with_format(
    raw: &str,
    format: ConfigFormat,
) -> Result<ClientConfig, ConfigError> {
    let format = if format == ConfigFormat::Auto {
        detect_format(raw)
    } else {
        format
    };
    parse_client_config_at(raw, format, None)
}

fn parse_server_config_at(
    raw: &str,
    format: ConfigFormat,
    path: Option<&Path>,
) -> Result<ServerConfig, ConfigError> {
    let expanded = expand_env(raw)?;
    let config: ServerConfig = match format {
        ConfigFormat::Toml | ConfigFormat::Auto => toml::from_str(&expanded)?,
        ConfigFormat::Yaml => {
            serde_yaml::from_str(&expanded).map_err(|err| yaml_error(err, path))?
        }
    };
    config.validate()?;
    Ok(config)
}

fn parse_client_config_at(
    raw: &str,
    format: ConfigFormat,
    path: Option<&Path>,
) -> Result<ClientConfig, ConfigError> {
    let expanded = expand_env(raw)?;
    let config: ClientConfig = match format {
        ConfigFormat::Toml | ConfigFormat::Auto => toml::from_str(&expanded)?,
        ConfigFormat::Yaml => {
            serde_yaml::from_str(&expanded).map_err(|err| yaml_error(err, path))?
        }
    };
    config.validate()?;
    Ok(config)
}

fn yaml_error(err: serde_yaml::Error, path: Option<&Path>) -> ConfigError {
    let location = err.location().map(|loc| (loc.line(), loc.column()));
    let source = path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<input>".to_string());
    let location = location.map_or_else(
        || source.clone(),
        |(line, column)| format!("{source}: line {line}, column {column}"),
    );
    ConfigError::Yaml(format!("{location}: {err}"))
}

fn resolve_format(raw: &str, path: &Path, requested: ConfigFormat) -> ConfigFormat {
    if requested != ConfigFormat::Auto {
        return requested;
    }
    match ConfigFormat::from_path(path) {
        ConfigFormat::Auto => detect_format(raw),
        format => format,
    }
}

/// 返回实际使用的配置格式；用于 `meatus check` 输出和错误信息。
#[must_use]
pub fn resolve_config_format(raw: &str, path: &Path, requested: ConfigFormat) -> ConfigFormat {
    resolve_format(raw, path, requested)
}

fn detect_format(raw: &str) -> ConfigFormat {
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') {
            return ConfigFormat::Toml;
        }
        if !line.starts_with(char::is_whitespace)
            && (trimmed == "server:"
                || trimmed == "client:"
                || trimmed.starts_with("server: ")
                || trimmed.starts_with("client: "))
        {
            return ConfigFormat::Yaml;
        }
        break;
    }
    ConfigFormat::Toml
}

#[must_use]
pub fn detect_kind(raw: &str) -> Option<ConfigKind> {
    detect_kind_with_format(raw, ConfigFormat::Auto)
}

/// 按指定格式识别配置顶层类型；`Auto` 时按内容猜测。
#[must_use]
pub fn detect_kind_with_format(raw: &str, format: ConfigFormat) -> Option<ConfigKind> {
    let format = if format == ConfigFormat::Auto {
        detect_format(raw)
    } else {
        format
    };
    let mut kind = None;
    for line in raw.lines() {
        let trimmed = line.trim();
        match format {
            ConfigFormat::Toml | ConfigFormat::Auto => match trimmed {
                "[server]" => kind = Some(ConfigKind::Server),
                "[client]" => kind = Some(ConfigKind::Client),
                _ => {}
            },
            ConfigFormat::Yaml => {
                if line.starts_with(char::is_whitespace) {
                    continue;
                }
                if trimmed == "server:" || trimmed.starts_with("server: ") {
                    kind = Some(ConfigKind::Server);
                } else if trimmed == "client:" || trimmed.starts_with("client: ") {
                    kind = Some(ConfigKind::Client);
                }
            }
        }
        if kind.is_some() {
            break;
        }
    }
    kind
}

/// 展开 `${VAR}`；缺失变量报错，不静默替换为空串。
pub fn expand_env(text: &str) -> Result<String, ConfigError> {
    expand_env_with(text, |name| std::env::var(name).ok())
}

fn expand_env_with<F>(text: &str, lookup: F) -> Result<String, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(invalid("unclosed ${...} placeholder"));
        };
        let name = &after[..end];
        if name.is_empty() {
            return Err(invalid("empty environment placeholder"));
        }
        let value = lookup(name).ok_or_else(|| ConfigError::MissingEnv(name.to_string()))?;
        out.push_str(&value);
        rest = &after[end + 1..];
    }

    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER_TOML: &str = r#"
[server]
bind_addr = "0.0.0.0"
control_port = 7000

[auth]
mode = "hmac"
clients = [
  { id = "home-nas", tenant = "default", token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
]

[ports]
range = [20000, 20100]

[http]
enabled = true
bogus = "x"
domain = "*.tunnel.example.com"
"#;

    #[test]
    fn parse_minimal_server() {
        let toml = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"#;
        let config = parse_server_config(toml).expect("valid");
        assert_eq!(config.server.bind_addr, "0.0.0.0");
        assert_eq!(config.ports.range, [20000, 30000]);
        assert_eq!(config.auth.credential_entries().len(), 1);
        assert!(config.relay.is_none(), "standalone must stay standalone");
    }

    fn server_with_relay(relay: &str, acme: &str) -> String {
        format!(
            r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

{relay}
{acme}
"#
        )
    }

    const RELAY_TOML: &str = r#"
[relay]
id = "relay-a"
group = "prod"
role = "primary"
advertise_addr = "relay-a.example.com:7000"
bundle = "/etc/meatus/relay-bundle.json"
bundle_pubkey = "ed25519:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
peer_secret_file = "/etc/meatus/relay-peer.key"
"#;

    #[test]
    fn relay_config_parses_with_defaults() {
        let config = parse_server_config(&server_with_relay(RELAY_TOML, ""))
            .expect("relay config should be valid");
        let relay = config.relay.expect("relay section");
        assert_eq!(relay.id, "relay-a");
        assert_eq!(relay.group, "prod");
        assert_eq!(relay.role, RelayRole::Primary);
        assert_eq!(relay.health_bind, "127.0.0.1:7401");
        assert_eq!(relay.health_interval_s, 5);
        assert_eq!(relay.peer_timeout_s, 5);
        assert_eq!(relay.rotation_fail_threshold, 2);
        assert_eq!(relay.rotation_recover_threshold, 1);
        assert_eq!(relay.cert_sync, RelayCertSync::Off);
    }

    #[test]
    fn relay_id_and_group_must_not_be_blank() {
        let bad = RELAY_TOML.replace("id = \"relay-a\"", "id = \" \"");
        let err = parse_server_config(&server_with_relay(&bad, "")).expect_err("must fail");
        assert!(err.to_string().contains("relay.id"), "{err}");

        let bad = RELAY_TOML.replace("group = \"prod\"", "group = \" \"");
        let err = parse_server_config(&server_with_relay(&bad, "")).expect_err("must fail");
        assert!(err.to_string().contains("relay.group"), "{err}");
    }

    #[test]
    fn relay_health_bind_requires_host_and_port() {
        let bad = RELAY_TOML.replace(
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"",
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"\nhealth_bind = \"7401\"",
        );
        let err = parse_server_config(&server_with_relay(&bad, "")).expect_err("must fail");
        assert!(err.to_string().contains("relay.health_bind"), "{err}");
    }

    #[test]
    fn relay_health_bind_rejects_public_interfaces() {
        let bad = RELAY_TOML.replace(
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"",
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"\nhealth_bind = \"0.0.0.0:7401\"",
        );
        let err = parse_server_config(&server_with_relay(&bad, "")).expect_err("must fail");
        assert!(err.to_string().contains("loopback"), "{err}");

        let private = RELAY_TOML.replace(
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"",
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"\nhealth_bind = \"10.0.0.5:7401\"",
        );
        parse_server_config(&server_with_relay(&private, ""))
            .expect("private health bind should be valid");
    }

    #[test]
    fn relay_yaml_matches_toml_defaults() {
        let yaml = r#"
server:
  control_port: 7000
auth:
  token_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
relay:
  id: relay-a
  group: prod
  role: primary
  advertise_addr: "relay-a.example.com:7000"
  bundle: /etc/meatus/relay-bundle.json
  bundle_pubkey: "ed25519:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
  peer_secret_file: /etc/meatus/relay-peer.key
"#;
        let config = parse_server_config_with_format(yaml, ConfigFormat::Yaml)
            .expect("yaml relay config should be valid");
        let relay = config.relay.expect("relay section");
        assert_eq!(relay.role, RelayRole::Primary);
        assert_eq!(relay.health_bind, "127.0.0.1:7401");
        assert_eq!(relay.health_interval_s, 5);
        assert_eq!(relay.peer_timeout_s, 5);
        assert_eq!(relay.rotation_fail_threshold, 2);
        assert_eq!(relay.rotation_recover_threshold, 1);
    }

    #[test]
    fn relay_advertise_addr_requires_host_and_port() {
        let bad = RELAY_TOML.replace(
            "advertise_addr = \"relay-a.example.com:7000\"",
            "advertise_addr = \"relay-a\"",
        );
        let err = parse_server_config(&server_with_relay(&bad, "")).expect_err("must fail");
        assert!(err.to_string().contains("relay.advertise_addr"), "{err}");
    }

    #[test]
    fn relay_peer_timeout_is_bounded() {
        let bad = RELAY_TOML.replace(
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"",
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"\npeer_timeout_s = 0",
        );
        let err = parse_server_config(&server_with_relay(&bad, "")).expect_err("must fail");
        assert!(err.to_string().contains("relay.peer_timeout_s"), "{err}");
    }

    #[test]
    fn relay_health_interval_is_bounded() {
        let bad = RELAY_TOML.replace(
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"",
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"\nhealth_interval_s = 0",
        );
        let err = parse_server_config(&server_with_relay(&bad, "")).expect_err("must fail");
        assert!(err.to_string().contains("relay.health_interval_s"), "{err}");

        let good = RELAY_TOML.replace(
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"",
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"\nhealth_interval_s = 1",
        );
        let config =
            parse_server_config(&server_with_relay(&good, "")).expect("interval 1 should be valid");
        assert_eq!(config.relay.expect("relay").health_interval_s, 1);
    }

    #[test]
    fn relay_rotation_thresholds_are_bounded() {
        let bad = RELAY_TOML.replace(
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"",
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"\nrotation_fail_threshold = 0",
        );
        let err = parse_server_config(&server_with_relay(&bad, "")).expect_err("must fail");
        assert!(
            err.to_string().contains("relay.rotation_fail_threshold"),
            "{err}"
        );

        let bad = RELAY_TOML.replace(
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"",
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"\nrotation_recover_threshold = 11",
        );
        let err = parse_server_config(&server_with_relay(&bad, "")).expect_err("must fail");
        assert!(
            err.to_string().contains("relay.rotation_recover_threshold"),
            "{err}"
        );

        let good = RELAY_TOML.replace(
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"",
            "peer_secret_file = \"/etc/meatus/relay-peer.key\"\nrotation_fail_threshold = 1\nrotation_recover_threshold = 1",
        );
        let config = parse_server_config(&server_with_relay(&good, "")).expect("thresholds");
        let relay = config.relay.expect("relay");
        assert_eq!(relay.rotation_fail_threshold, 1);
        assert_eq!(relay.rotation_recover_threshold, 1);
    }

    #[cfg(unix)]
    #[test]
    fn relay_peer_secret_requires_32_bytes_and_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("meatus-peer-secret-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        let secret_path = dir.join("relay-peer.key");
        let admin_token = "x".repeat(32);
        fs::write(&secret_path, b"0123456789abcdef0123456789abcdef").expect("write secret");
        fs::set_permissions(&secret_path, fs::Permissions::from_mode(0o600)).expect("set mode");

        let toml = format!(
            r#"
[server]
control_port = 7000
tls_cert = "cert.pem"
tls_key = "key.pem"

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[admin]
admin_token = "{admin_token}"

[relay]
id = "relay-a"
group = "prod"
role = "primary"
advertise_addr = "relay-a.example.com:7000"
bundle = "/etc/meatus/relay-bundle.json"
bundle_pubkey = "ed25519:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
peer_secret_file = "{}"
"#,
            secret_path.display()
        );
        let config = parse_server_config(&toml).expect("config");
        config.validate_runtime().expect("valid peer secret");

        fs::set_permissions(&secret_path, fs::Permissions::from_mode(0o644)).expect("set mode");
        let err = config.validate_runtime().expect_err("0644 must fail");
        assert!(err.to_string().contains("0600"), "{err}");

        fs::write(&secret_path, b"short").expect("write short");
        fs::set_permissions(&secret_path, fs::Permissions::from_mode(0o600)).expect("set mode");
        let err = config.validate_runtime().expect_err("short must fail");
        assert!(err.to_string().contains("32 bytes"), "{err}");

        fs::write(&secret_path, admin_token.as_bytes()).expect("write reused");
        let err = config.validate_runtime().expect_err("reused must fail");
        assert!(err.to_string().contains("admin.admin_token"), "{err}");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn relay_bundle_pubkey_requires_ed25519_prefix() {
        let bad = RELAY_TOML.replace("ed25519:", "");
        let err = parse_server_config(&server_with_relay(&bad, "")).expect_err("must fail");
        assert!(err.to_string().contains("ed25519:BASE64"), "{err}");
    }

    #[test]
    fn relay_bundle_pubkey_requires_32_bytes() {
        let bad = RELAY_TOML.replace(
            "ed25519:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "ed25519:AAAA",
        );
        let err = parse_server_config(&server_with_relay(&bad, "")).expect_err("must fail");
        assert!(err.to_string().contains("32 bytes"), "{err}");
    }

    #[test]
    fn relay_primary_cert_sync_requires_acme() {
        let with_push =
            RELAY_TOML.replace("peer_secret_file", "cert_sync = \"push\"\npeer_secret_file");
        let err = parse_server_config(&server_with_relay(&with_push, ""))
            .expect_err("primary cert_sync without acme must fail");
        assert!(err.to_string().contains("requires acme.enabled"), "{err}");

        let acme = r#"
[acme]
enabled = true
email = "ops@example.com"
"#;
        parse_server_config(&server_with_relay(&with_push, acme))
            .expect("primary cert_sync with acme");
    }

    #[test]
    fn relay_secondary_must_not_enable_acme() {
        let secondary = RELAY_TOML.replace("role = \"primary\"", "role = \"secondary\"");
        let acme = r#"
[acme]
enabled = true
email = "ops@example.com"
"#;
        let err = parse_server_config(&server_with_relay(&secondary, acme))
            .expect_err("secondary + acme must fail");
        assert!(err.to_string().contains("must not enable acme"), "{err}");
    }

    #[test]
    fn audit_directory_is_optional() {
        let config = parse_server_config(
            r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[audit]
dir = "/var/lib/meatus/audit"
"#,
        )
        .expect("valid");
        assert_eq!(
            config.audit.dir.as_deref(),
            Some(std::path::Path::new("/var/lib/meatus/audit"))
        );
        assert!(config_for_default_audit().audit.dir.is_none());
    }

    fn config_for_default_audit() -> ServerConfig {
        parse_server_config(
            r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"#,
        )
        .expect("valid")
    }

    #[test]
    fn client_policy_enforces_type_domain_and_port() {
        let toml = r#"
[server]
control_port = 7000

[auth]
clients = [
  { id = "nas", tenant = "default", token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", allow_ports = [21000, 21010], allow_types = ["http"], allow_domains = ["*.example.com"], max_proxies = 2, limits = { monthly_bytes = 104857600, quota_action = "throttle", conn_rate_per_s = 5 } },
]
"#;
        let config = parse_server_config(toml).expect("valid");
        let policy = config.auth.clients[0].policy();
        assert!(policy.allows_type(ProxyType::Http));
        assert!(!policy.allows_type(ProxyType::Tcp));
        assert!(policy.allows_port(21005));
        assert!(!policy.allows_port(21011));
        assert!(policy.allows_domains(&["a.example.com".to_string()]));
        assert!(!policy.allows_domains(&["a.other.com".to_string()]));
        assert_eq!(policy.monthly_bytes(), 104857600);
        assert_eq!(policy.quota_action(), QuotaAction::Throttle);
        assert_eq!(policy.conn_rate_per_s(), 5);
    }

    #[test]
    fn client_policy_allows_equivalent_wildcard_custom_domains_only() {
        let toml = r#"
[server]
control_port = 7000

[auth]
clients = [
  { id = "nas", tenant = "default", token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", allow_domains = ["*.binsail.cloud"], allow_types = ["http"] },
]
"#;
        let config = parse_server_config(toml).expect("valid");
        let policy = config.auth.clients[0].policy();

        assert!(policy.allows_domains(&["*.binsail.cloud".to_string()]));
        assert!(policy.allows_domains(&["*.BINSAIL.CLOUD.".to_string()]));
        assert!(policy.allows_domains(&["app.binsail.cloud".to_string()]));
        assert!(!policy.allows_domains(&["*.other.binsail.cloud".to_string()]));
        assert!(!policy.allows_domains(&["*.binsail.cloud.evil".to_string()]));
        assert!(!policy.allows_domains(&["binsail.cloud".to_string()]));
    }

    #[test]
    fn admin_users_roles_and_tokens_are_validated() {
        let toml = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[admin]
bind = "0.0.0.0:7400"
users = [
  { name = "alice", role = "operator", token = "alice-secret" },
  { name = "audit", role = "auditor", token_sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
]
"#;
        let config = parse_server_config(toml).expect("valid");
        assert_eq!(config.admin.users.len(), 2);
        assert_eq!(config.admin.users[0].role, AdminRole::Operator);
        assert_eq!(config.admin.users[1].role, AdminRole::Auditor);

        let duplicate = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[admin]
users = [
  { name = "dup", role = "viewer", token = "one" },
  { name = "dup", role = "auditor", token = "two" },
]
"#;
        assert!(
            parse_server_config(duplicate)
                .expect_err("duplicate")
                .to_string()
                .contains("duplicate")
        );

        let no_token = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[admin]
users = [{ name = "empty", role = "viewer" }]
"#;
        assert!(
            parse_server_config(no_token)
                .expect_err("missing token")
                .to_string()
                .contains("requires token")
        );
    }

    #[test]
    fn parse_ports_release_policy() {
        let toml = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[ports]
range = [20000, 20100]
release_policy = "hold"
hold_secs = 120
"#;
        let config = parse_server_config(toml).expect("valid");
        assert_eq!(config.ports.release_policy, PortReleasePolicy::Hold);
        assert_eq!(config.ports.hold_secs, 120);
    }

    #[test]
    fn reject_zero_hold_secs_when_holding() {
        let toml = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[ports]
range = [20000, 20100]
release_policy = "hold"
hold_secs = 0
"#;
        assert!(parse_server_config(toml).is_err());
    }

    #[test]
    fn drain_grace_defaults_to_five_seconds_and_rejects_over_sixty() {
        let toml = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[ports]
range = [20000, 20100]
"#;
        let config = parse_server_config(toml).expect("valid");
        assert_eq!(config.server.drain_grace_s, 5);

        let invalid = toml.replace(
            "control_port = 7000",
            "control_port = 7000\ndrain_grace_s = 61",
        );
        let err = parse_server_config(&invalid).expect_err("drain grace too large");
        assert!(err.to_string().contains("drain_grace_s must be 0..=60"));
    }

    #[test]
    fn reject_unknown_section_field() {
        assert!(parse_server_config(SERVER_TOML).is_err());
    }

    #[test]
    fn empty_admin_token_is_rejected() {
        let toml = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[admin]
bind = "127.0.0.1:7400"
admin_token = "  "
"#;
        let err = parse_server_config(toml).expect_err("empty admin token");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn admin_on_all_interfaces_requires_token() {
        let toml = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[admin]
bind = "0.0.0.0:7400"
"#;
        let err = parse_server_config(toml).expect_err("should reject");
        assert!(err.to_string().contains("admin_token"));
    }

    #[test]
    fn acme_dns01_requires_provider() {
        let toml = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[acme]
enabled = true
email = "ops@example.com"
challenge = "dns-01"
"#;
        let err = parse_server_config(toml).expect_err("should reject");
        assert!(err.to_string().contains("dns_provider"));
    }

    #[test]
    fn acme_dns01_accepts_cloudflare_without_http() {
        let toml = r#"
[server]
control_port = 7000
tls_cert = "/tmp/meatus-test.crt"
tls_key = "/tmp/meatus-test.key"

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[acme]
enabled = true
email = "ops@example.com"
challenge = "dns-01"
dns_provider = "cloudflare"
dns_api_token = "token"
"#;
        let config = parse_server_config(toml).expect("dns-01 config");
        config.validate_runtime().expect("dns-01 runtime");
    }

    #[test]
    fn acme_dns01_accepts_dnspod_without_http() {
        let toml = r#"
[server]
control_port = 7000
tls_cert = "/tmp/meatus-test.crt"
tls_key = "/tmp/meatus-test.key"

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[acme]
enabled = true
email = "ops@example.com"
challenge = "dns-01"
dns_provider = "dnspod"
dns_api_token = "12345,abcdef"
"#;
        let config = parse_server_config(toml).expect("dnspod dns-01 config");
        config.validate_runtime().expect("dnspod dns-01 runtime");
    }

    #[test]
    fn acme_dns01_rejects_dnspod_token_without_comma() {
        let toml = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[acme]
enabled = true
email = "ops@example.com"
challenge = "dns-01"
dns_provider = "dnspod"
dns_api_token = "not-an-id-token"
"#;
        let err = parse_server_config(toml).expect_err("should reject");
        assert!(err.to_string().contains("ID,Token"), "{err}");
    }

    #[test]
    fn acme_dns01_rejects_unknown_provider() {
        let toml = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[acme]
enabled = true
email = "ops@example.com"
challenge = "dns-01"
dns_provider = "route53"
dns_api_token = "token"
"#;
        let err = parse_server_config(toml).expect_err("should reject");
        assert!(err.to_string().contains("cloudflare"));
    }

    #[test]
    fn parse_client_with_proxies() {
        let toml = r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"

[[proxy]]
name = "ssh"
type = "tcp"
local_addr = "127.0.0.1:22"
remote_port = 20022

[[proxy]]
name = "nas-web"
type = "http"
local_addr = "127.0.0.1:8080"
custom_domains = ["nas.tunnel.example.com"]
"#;
        let config = parse_client_config(toml).expect("valid");
        assert_eq!(config.proxies.len(), 2);
        assert_eq!(config.proxies[1].kind, ProxyType::Http);
    }

    #[test]
    fn client_identity_defaults_and_server_host() {
        let config = parse_client_config(
            r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"
"#,
        )
        .expect("valid");
        assert_eq!(config.client.client_id, "default");
        assert_eq!(config.client.tenant, "default");
        assert_eq!(config.server_host(), "tunnel.example.com");
        assert!(config.proxy_map().is_empty());

        let ipv6 = parse_client_config(
            r#"
[client]
server_addr = "[::1]:7000"
token = "secret-token"
"#,
        )
        .expect("valid");
        assert_eq!(ipv6.server_host(), "::1");
    }

    #[test]
    fn host_port_brackets_ipv6_literals() {
        assert_eq!(host_port("::", 7000), "[::]:7000");
        assert_eq!(host_port("::1", 7000), "[::1]:7000");
        assert_eq!(host_port("[::1]", 7000), "[::1]:7000");
        assert_eq!(host_port("0.0.0.0", 7000), "0.0.0.0:7000");
        assert_eq!(normalize_host_port("::1:7000"), "[::1]:7000");
        assert_eq!(normalize_host_port("[::1]:7000"), "[::1]:7000");
        assert_eq!(normalize_host_port("example.com:7000"), "example.com:7000");
    }

    #[test]
    fn client_server_addrs_take_priority_and_validate() {
        let config = parse_client_config(
            r#"
[client]
server_addr = "old.example.com:7000"
server_addrs = ["one.example.com:7000", "two.example.com:7001"]
token = "secret-token"
"#,
        )
        .expect("valid");
        assert_eq!(
            config.dial_addrs(),
            vec![
                "one.example.com:7000".to_string(),
                "two.example.com:7001".to_string()
            ]
        );
        assert_eq!(config.server_host(), "one.example.com");

        let err = parse_client_config(
            r#"
[client]
server_addrs = ["one.example.com:7000", "one.example.com:7000"]
token = "secret-token"
"#,
        )
        .expect_err("duplicate should fail");
        assert!(err.to_string().contains("duplicate"), "{err}");

        let err = parse_client_config(
            r#"
[client]
server_addrs = ["not-a-host-port"]
token = "secret-token"
"#,
        )
        .expect_err("invalid should fail");
        assert!(err.to_string().contains("host:port"), "{err}");
    }

    #[test]
    fn client_multi_home_requires_group_and_two_members() {
        let config = parse_client_config(
            r#"
[client]
server_addrs = ["one.example.com:7000", "two.example.com:7001"]
token = "secret-token"

[client.relay]
group = "prod"
multi_home = true
health_interval_s = 5
"#,
        )
        .expect("valid multi-home");
        assert!(config.client.relay.multi_home);
        assert_eq!(config.client.relay.group, "prod");
        assert_eq!(config.client.relay.health_interval_s, 5);

        let err = parse_client_config(
            r#"
[client]
server_addrs = ["one.example.com:7000"]
token = "secret-token"

[client.relay]
group = "prod"
multi_home = true
"#,
        )
        .expect_err("one member should fail");
        assert!(err.to_string().contains("at least two"), "{err}");

        let err = parse_client_config(
            r#"
[client]
server_addrs = ["one.example.com:7000", "two.example.com:7001"]
token = "secret-token"

[client.relay]
multi_home = true
"#,
        )
        .expect_err("missing group should fail");
        assert!(err.to_string().contains("group is required"), "{err}");
    }

    #[test]
    fn client_relay_defaults_to_legacy_sequential() {
        let config = parse_client_config(
            r#"
[client]
server_addr = "one.example.com:7000"
token = "secret-token"
"#,
        )
        .expect("legacy config");
        assert!(!config.client.relay.multi_home);
        assert!(config.client.relay.group.is_empty());
        assert_eq!(config.client.relay.health_interval_s, 5);
    }

    #[test]
    fn transport_defaults_to_tcp_and_parses_quic() {
        let config = parse_client_config(
            r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"
"#,
        )
        .expect("valid");
        assert_eq!(config.client.transport, TransportMode::Tcp);

        let config = parse_client_config(
            r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"
transport = "quic"
"#,
        )
        .expect("valid");
        assert_eq!(config.client.transport, TransportMode::Quic);
        assert_eq!(TransportMode::Auto.to_string(), "auto");

        let config = parse_client_config(
            r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"
transport = "auto"
"#,
        )
        .expect("valid");
        assert_eq!(config.client.transport, TransportMode::Auto);

        assert!(
            parse_client_config(
                r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"
transport = "udp"
"#,
            )
            .is_err(),
            "unknown transport must be rejected"
        );
    }

    #[test]
    fn client_pool_defaults_to_disabled() {
        let config = parse_client_config(
            r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"
"#,
        )
        .expect("valid");
        assert!(!config.client.pool.enabled);
        assert_eq!(config.client.pool.size, 4);
        assert_eq!(config.client.pool.max_streams_per_conn, 64);
        assert_eq!(config.client.pool.idle_timeout_s, 300);
    }

    #[test]
    fn client_pool_reads_nested_toml_table() {
        let config = parse_client_config(
            r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"

[client.pool]
enabled = true
size = 8
max_streams_per_conn = 128
idle_timeout_s = 600
"#,
        )
        .expect("valid");
        assert!(config.client.pool.enabled);
        assert_eq!(config.client.pool.size, 8);
        assert_eq!(config.client.pool.max_streams_per_conn, 128);
        assert_eq!(config.client.pool.idle_timeout_s, 600);
    }

    #[test]
    fn client_pool_yaml_matches_toml() {
        let yaml = r#"
client:
  server_addr: "tunnel.example.com:7000"
  token: "secret-token"
  pool:
    enabled: true
    size: 2
    max_streams_per_conn: 16
    idle_timeout_s: 60
"#;
        let config = parse_client_config_with_format(yaml, ConfigFormat::Yaml).expect("valid yaml");
        assert!(config.client.pool.enabled);
        assert_eq!(config.client.pool.size, 2);
        assert_eq!(config.client.pool.max_streams_per_conn, 16);
        assert_eq!(config.client.pool.idle_timeout_s, 60);
    }

    #[test]
    fn client_pool_rejects_out_of_range_values() {
        for (field, value, expected) in [
            ("size", "0", "client.pool.size"),
            (
                "max_streams_per_conn",
                "0",
                "client.pool.max_streams_per_conn",
            ),
            ("idle_timeout_s", "0", "client.pool.idle_timeout_s"),
        ] {
            let toml = format!(
                r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"

[client.pool]
{field} = {value}
"#
            );
            let err = parse_client_config(&toml).expect_err("out of range");
            assert!(err.to_string().contains(expected), "{err}");
        }
    }

    #[test]
    fn client_pool_rejects_unknown_field() {
        let err = parse_client_config(
            r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"

[client.pool]
enabled = true
max_conns = 10
"#,
        )
        .expect_err("unknown field");
        assert!(err.to_string().contains("max_conns"), "{err}");
    }

    #[test]
    fn spki_pin_is_validated() {
        let good = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let config = parse_client_config(&format!(
            r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"
server_public_key_pin = "{good}"
"#
        ))
        .expect("valid pin");
        assert_eq!(config.client.server_public_key_pin.as_deref(), Some(good));

        let bad = r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"
server_public_key_pin = "sha256:abc"
"#;
        let err = parse_client_config(bad).expect_err("bad pin");
        assert!(err.to_string().contains("server_public_key_pin"));
    }

    #[test]
    fn runtime_requires_tls_cert_and_key() {
        let without_tls = parse_server_config(
            r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"#,
        )
        .expect("parse");
        let err = without_tls.validate_runtime().expect_err("missing TLS");
        assert!(err.to_string().contains("tls_cert"));

        let with_tls = parse_server_config(
            r#"
[server]
control_port = 7000
tls_cert = "certs/server.crt"
tls_key = "certs/server.key"

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"#,
        )
        .expect("parse");
        with_tls.validate_runtime().expect("TLS configured");
    }

    #[test]
    fn duplicate_proxy_names_are_rejected() {
        let toml = r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"

[[proxy]]
name = "ssh"
type = "tcp"
local_addr = "127.0.0.1:22"

[[proxy]]
name = "ssh"
type = "tcp"
local_addr = "127.0.0.1:23"
"#;
        assert!(parse_client_config(toml).is_err());
    }

    #[test]
    fn http_proxy_accepts_remote_port_zero() {
        let toml = r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"

[[proxy]]
name = "nas-web"
type = "http"
local_addr = "127.0.0.1:8080"
remote_port = 0
custom_domains = ["nas.tunnel.example.com"]
"#;
        let config = parse_client_config(toml).expect("valid zero remote_port");
        assert_eq!(config.proxies[0].remote_port, Some(0));
    }

    #[test]
    fn explicit_privileged_remote_port_is_rejected() {
        let toml = r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"

[[proxy]]
name = "ssh"
type = "tcp"
local_addr = "127.0.0.1:22"
remote_port = 80
"#;
        let err = parse_client_config(toml).expect_err("privileged port");
        assert!(err.to_string().contains("must be 0 or >= 1024"));
    }

    #[test]
    fn socks5_proxy_validates_target_policy() {
        let valid = r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"

[[proxy]]
name = "socks"
type = "socks5"
local_addr = "127.0.0.1:0"

[[proxy]]
name = "socks-bad"
type = "socks5"
local_addr = "127.0.0.1:0"
options.allow_cidrs = ["not-a-cidr"]
"#;
        let err = parse_client_config(valid).expect_err("invalid CIDR");
        assert!(err.to_string().contains("allow_cidrs"));

        let with_domains = r#"
[client]
server_addr = "tunnel.example.com:7000"
token = "secret-token"

[[proxy]]
name = "socks"
type = "socks5"
local_addr = "127.0.0.1:0"
custom_domains = ["socks.example.com"]
"#;
        assert!(parse_client_config(with_domains).is_err());
    }

    #[test]
    fn env_placeholder_is_expanded() {
        let text = "a=${FOO}/b";
        let out = expand_env_with(text, |name| (name == "FOO").then(|| "bar".to_string()))
            .expect("expanded");
        assert_eq!(out, "a=bar/b");
    }

    #[test]
    fn missing_env_is_an_error() {
        let err = expand_env_with("${NOPE}", |_| None).expect_err("missing");
        assert!(matches!(err, ConfigError::MissingEnv(ref name) if name == "NOPE"));
    }

    #[test]
    fn unclosed_placeholder_is_an_error() {
        assert!(expand_env_with("${NOPE", |_| None).is_err());
    }

    #[test]
    fn detect_config_kind() {
        assert_eq!(detect_kind("[server]\n"), Some(ConfigKind::Server));
        assert_eq!(detect_kind("[client]\n"), Some(ConfigKind::Client));
        assert_eq!(detect_kind("[log]\n"), None);
        assert_eq!(
            detect_kind("server:\n  control_port: 7000\n"),
            Some(ConfigKind::Server)
        );
        assert_eq!(
            detect_kind("client:\n  server_addr: x:7000\n"),
            Some(ConfigKind::Client)
        );
    }

    #[test]
    fn parse_yaml_server_is_semantically_equal() {
        let yaml = r#"
server:
  control_port: 7000
auth:
  token_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ports:
  range: [20000, 20100]
admin:
  bind: "127.0.0.1:7400"
  tls:
    enabled: true
    cert: "/tmp/admin.crt"
    key: "/tmp/admin.key"
"#;
        let config =
            parse_server_config_with_format(yaml, ConfigFormat::Yaml).expect("valid YAML server");
        assert_eq!(config.server.control_port, 7000);
        assert_eq!(config.ports.range, [20000, 20100]);
        assert!(config.admin.tls.enabled);
        assert_eq!(
            config.admin.tls.cert.as_deref(),
            Some(Path::new("/tmp/admin.crt"))
        );
    }

    #[test]
    fn yaml_error_reports_line_and_column() {
        let yaml = "server:\n  control_port: 7000\n   broken: [\n";
        let err =
            parse_server_config_with_format(yaml, ConfigFormat::Yaml).expect_err("invalid YAML");
        let message = err.to_string();
        assert!(message.contains("line "), "{message}");
        assert!(message.contains("column "), "{message}");
    }

    #[test]
    fn admin_tls_requires_cert_and_key() {
        let toml = r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[admin]
bind = "0.0.0.0:7400"
admin_token = "admin-secret"

[admin.tls]
enabled = true
"#;
        let err = parse_server_config(toml).expect_err("missing admin TLS files");
        assert!(err.to_string().contains("admin.tls.cert"));
    }
}
