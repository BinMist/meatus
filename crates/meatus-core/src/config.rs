//! TOML 配置模型、环境变量展开与校验。
//!
//! 约定：配置文件里允许写 `${VAR}`，启动时展开；变量缺失直接报错，不做空值兜底。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub server: ServerSection,
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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,
    #[serde(default = "default_control_port")]
    pub control_port: u16,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
}

fn default_bind_addr() -> String {
    "0.0.0.0".to_string()
}

fn default_control_port() -> u16 {
    7000
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            bind_addr: default_bind_addr(),
            control_port: default_control_port(),
            tls_cert: None,
            tls_key: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientEntry {
    pub id: String,
    #[serde(default = "default_tenant")]
    pub tenant: String,
    pub token_sha256: Option<String>,
    pub allow_ports: Option<[u16; 2]>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortsSection {
    pub range: [u16; 2],
}

impl Default for PortsSection {
    fn default() -> Self {
        Self {
            range: [20000, 30000],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminSection {
    #[serde(default = "default_admin_bind")]
    pub bind: String,
    #[serde(default = "default_panel")]
    pub panel: bool,
    pub admin_token: Option<String>,
    #[serde(default)]
    pub tls: AdminTlsSection,
    /// 仅当管理面位于可信反向代理之后时开启；默认不信任转发头。
    #[serde(default)]
    pub trust_forwarded_proto: bool,
}

#[derive(Debug, Clone, Deserialize)]
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
            tls: AdminTlsSection::default(),
            trust_forwarded_proto: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
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
        if self.server.control_port < 1024 {
            return Err(invalid("server.control_port must be >= 1024"));
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
            if matches!(self.acme.challenge, AcmeChallenge::Dns01)
                && (self.acme.dns_provider.is_none() || self.acme.dns_api_token.is_none())
            {
                return Err(invalid(
                    "acme.challenge = \"dns-01\" requires dns_provider and dns_api_token",
                ));
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
        if !is_loopback_bind(&self.admin.bind) && self.admin.admin_token.is_none() {
            return Err(invalid(
                "admin.bind is not loopback; admin_token is required in that case",
            ));
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
        if self.acme.enabled {
            if !self.http.enabled {
                return Err(invalid(
                    "acme.enabled requires http.enabled = true (HTTP-01 uses :80)",
                ));
            }
            if self.acme.email.as_deref().unwrap_or("").trim().is_empty() {
                return Err(invalid("acme.email is required when acme.enabled = true"));
            }
            if self.acme.challenge != AcmeChallenge::Http01 {
                return Err(invalid(
                    "ACME M3 only supports challenge = \"http-01\" for exact domains; \
                     tls-alpn-01 / dns-01 are not wired yet",
                ));
            }
        }
        if self.server.tls_cert.is_none() || self.server.tls_key.is_none() {
            return Err(invalid(
                "server.tls_cert and server.tls_key are required to serve TLS in M1",
            ));
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
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientSection {
    pub server_addr: String,
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
}

fn default_client_id() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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
    #[serde(default)]
    pub allow_cidrs: Vec<String>,
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

impl ClientConfig {
    /// 代理名到配置的映射，数据面按 `StreamHeader.proxy_name` 查找。
    #[must_use]
    pub fn proxy_map(&self) -> HashMap<String, ProxySpec> {
        self.proxies
            .iter()
            .map(|proxy| (proxy.name.clone(), proxy.clone()))
            .collect()
    }

    /// 从 `server_addr` 提取主机名，用于 TLS SNI / 证书校验。
    #[must_use]
    pub fn server_host(&self) -> &str {
        let addr = self.client.server_addr.as_str();
        if let Some(rest) = addr.strip_prefix('[') {
            return rest.split(']').next().unwrap_or(rest);
        }
        addr.rsplit_once(':').map_or(addr, |(host, _)| host)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.client.server_addr.contains(':') {
            return Err(invalid("client.server_addr must be host:port"));
        }
        let port = self
            .client
            .server_addr
            .rsplit_once(':')
            .map(|(_, port)| port)
            .unwrap_or("");
        if port.parse::<u16>().is_err() {
            return Err(invalid("client.server_addr must end with a valid port"));
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
