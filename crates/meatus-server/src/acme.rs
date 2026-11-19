#![forbid(unsafe_code)]
//! ACME（Let's Encrypt / Pebble）证书签发与热加载。
//!
//! 精确域名走 HTTP-01；泛域名走 DNS-01（支持 Cloudflare 与 DNSPod）。两类证书共用
//! `SecureDirCache`，HTTPS 入口按 SNI 选择域名 resolver，命中后做 TLS 终止；
//! 未命中仍走 SNI 透传，保证自带证书场景不被破坏。

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::StreamExt;
use instant_acme::{
    Account, AccountCredentials, ChallengeType, Identifier, NewAccount, NewOrder, RetryPolicy,
};
use rustls::ServerConfig as RustlsServerConfig;
use rustls::pki_types::CertificateDer;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls_acme::{AccountCache, AcmeConfig, CertCache, UseChallenge};
use rustls_pemfile::{certs as pem_certs, private_key as pem_private_key};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;
use tracing::{info, warn};

const LETS_ENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";
const CLOUDFLARE_API_BASE: &str = "https://api.cloudflare.com/client/v4";
const DNSPOD_API_BASE: &str = "https://dnsapi.cn";
const DNS_PROPAGATION_DELAY: Duration = Duration::from_secs(10);
const DNS_RENEW_BEFORE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const DNS_RENEW_CHECK: Duration = Duration::from_secs(6 * 60 * 60);
const DNS_RETRY_DELAY: Duration = Duration::from_secs(60 * 60);

/// 0600 文件缓存：账户密钥与证书都只允许当前用户读写。
#[derive(Clone)]
pub struct SecureDirCache {
    dir: PathBuf,
}

impl SecureDirCache {
    fn new(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    fn path(&self, prefix: &str, key: &str) -> PathBuf {
        self.dir.join(format!("{prefix}-{key}.pem"))
    }

    async fn load(&self, prefix: &str, key: &str) -> Result<Option<Vec<u8>>, io::Error> {
        match tokio::fs::read(self.path(prefix, key)).await {
            Ok(contents) => Ok(Some(contents)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    async fn store(&self, prefix: &str, key: &str, contents: &[u8]) -> Result<(), io::Error> {
        let path = self.path(prefix, key);
        tokio::fs::write(&path, contents).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
        }
        Ok(())
    }
}

fn cache_key(parts: &[&str], directory_url: &str) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    hasher.update(directory_url.as_bytes());
    hex::encode(hasher.finalize())
}

#[async_trait]
impl CertCache for SecureDirCache {
    type EC = io::Error;

    async fn load_cert(
        &self,
        domains: &[String],
        directory_url: &str,
    ) -> Result<Option<Vec<u8>>, io::Error> {
        let domain_refs: Vec<&str> = domains.iter().map(String::as_str).collect();
        self.load("cert", &cache_key(&domain_refs, directory_url))
            .await
    }

    async fn store_cert(
        &self,
        domains: &[String],
        directory_url: &str,
        cert: &[u8],
    ) -> Result<(), io::Error> {
        let domain_refs: Vec<&str> = domains.iter().map(String::as_str).collect();
        self.store("cert", &cache_key(&domain_refs, directory_url), cert)
            .await
    }
}

#[async_trait]
impl AccountCache for SecureDirCache {
    type EA = io::Error;

    async fn load_account(
        &self,
        contact: &[String],
        directory_url: &str,
    ) -> Result<Option<Vec<u8>>, io::Error> {
        let contact_refs: Vec<&str> = contact.iter().map(String::as_str).collect();
        self.load("account", &cache_key(&contact_refs, directory_url))
            .await
    }

    async fn store_account(
        &self,
        contact: &[String],
        directory_url: &str,
        account: &[u8],
    ) -> Result<(), io::Error> {
        let contact_refs: Vec<&str> = contact.iter().map(String::as_str).collect();
        self.store("account", &cache_key(&contact_refs, directory_url), account)
            .await
    }
}

impl SecureDirCache {
    async fn load_dns_account(
        &self,
        contact: &str,
        directory_url: &str,
    ) -> Result<Option<Vec<u8>>, io::Error> {
        self.load(
            "dns-account",
            &cache_key(&["dns-account", contact], directory_url),
        )
        .await
    }

    async fn store_dns_account(
        &self,
        contact: &str,
        directory_url: &str,
        credentials: &[u8],
    ) -> Result<(), io::Error> {
        self.store(
            "dns-account",
            &cache_key(&["dns-account", contact], directory_url),
            credentials,
        )
        .await
    }
}

#[derive(Debug, Clone)]
struct DnsError(String);

impl DnsError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for DnsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<instant_acme::Error> for DnsError {
    fn from(err: instant_acme::Error) -> Self {
        Self::new(format!("acme: {err}"))
    }
}

impl From<io::Error> for DnsError {
    fn from(err: io::Error) -> Self {
        Self::new(format!("io: {err}"))
    }
}

impl From<serde_json::Error> for DnsError {
    fn from(err: serde_json::Error) -> Self {
        Self::new(format!("json: {err}"))
    }
}

#[derive(Clone)]
struct CloudflareClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl fmt::Debug for CloudflareClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudflareClient")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct CloudflareEnvelope {
    success: bool,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    errors: Vec<CloudflareError>,
}

#[derive(Deserialize)]
struct CloudflareError {
    message: Option<String>,
}

impl CloudflareClient {
    fn new(token: String) -> Result<Self, DnsError> {
        Self::with_base(token, CLOUDFLARE_API_BASE)
    }

    fn with_base(token: String, base: impl Into<String>) -> Result<Self, DnsError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("meatus-acme/0.8")
            .build()
            .map_err(|err| DnsError::new(format!("cloudflare http client: {err}")))?;
        Ok(Self {
            http,
            base: base.into(),
            token,
        })
    }

    async fn zone(&self, domain: &str) -> Result<DnsZone, DnsError> {
        for candidate in zone_candidates(domain) {
            let response = self
                .http
                .get(format!("{}/zones", self.base))
                .bearer_auth(&self.token)
                .query(&[("name", candidate.as_str()), ("per_page", "1")])
                .send()
                .await
                .map_err(|err| map_reqwest_error("cloudflare", err))?;
            let value = self.check_response(response).await?;
            if let Some(id) = value.pointer("/0/id").and_then(|value| value.as_str()) {
                return Ok(DnsZone {
                    id: id.to_string(),
                    name: candidate,
                });
            }
        }
        Err(DnsError::new(format!(
            "cloudflare zone not found for {domain}"
        )))
    }

    async fn create_txt(
        &self,
        zone_id: &str,
        record_name: &str,
        value: &str,
    ) -> Result<String, DnsError> {
        let response = self
            .http
            .post(format!("{}/zones/{zone_id}/dns_records", self.base))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "type": "TXT",
                "name": record_name,
                "content": value,
                "ttl": 120,
            }))
            .send()
            .await
            .map_err(|err| map_reqwest_error("cloudflare", err))?;
        let value = self.check_response(response).await?;
        value
            .pointer("/id")
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
            .ok_or_else(|| DnsError::new("cloudflare TXT record id missing"))
    }

    async fn delete_txt(&self, zone_id: &str, record_id: &str) -> Result<(), DnsError> {
        let response = self
            .http
            .delete(format!(
                "{}/zones/{zone_id}/dns_records/{record_id}",
                self.base
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|err| map_reqwest_error("cloudflare", err))?;
        self.check_response(response).await?;
        Ok(())
    }

    async fn check_response(
        &self,
        response: reqwest::Response,
    ) -> Result<serde_json::Value, DnsError> {
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| DnsError::new(format!("cloudflare response body: {err}")))?;
        let envelope = serde_json::from_str::<CloudflareEnvelope>(&body).ok();
        if status.is_success() && envelope.as_ref().is_some_and(|value| value.success) {
            return Ok(envelope
                .and_then(|value| value.result)
                .unwrap_or(serde_json::Value::Null));
        }
        let message = envelope
            .as_ref()
            .and_then(|value| value.errors.first())
            .and_then(|error| error.message.as_deref())
            .unwrap_or("request failed");
        Err(DnsError::new(format!(
            "cloudflare API {}: {message}",
            status.as_u16()
        )))
    }
}

fn map_reqwest_error(provider: &str, err: reqwest::Error) -> DnsError {
    if err.is_timeout() {
        DnsError::new(format!("{provider} request timed out"))
    } else if err.is_connect() {
        DnsError::new(format!("{provider} connect failed: {err}"))
    } else {
        DnsError::new(format!("{provider} request failed: {err}"))
    }
}

#[derive(Clone)]
struct DnspodClient {
    http: reqwest::Client,
    base: String,
    login_token: String,
}

impl fmt::Debug for DnspodClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DnspodClient")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct DnspodResponse {
    status: DnspodStatus,
    #[serde(default)]
    domains: Vec<DnspodDomain>,
    #[serde(default)]
    record: Option<DnspodRecord>,
}

#[derive(Deserialize)]
struct DnspodStatus {
    code: String,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct DnspodDomain {
    #[serde(default)]
    id: Option<serde_json::Value>,
    name: String,
}

#[derive(Deserialize)]
struct DnspodRecord {
    #[serde(default)]
    id: Option<serde_json::Value>,
}

impl DnspodClient {
    fn new(token: String) -> Result<Self, DnsError> {
        Self::with_base(token, DNSPOD_API_BASE)
    }

    fn with_base(token: String, base: impl Into<String>) -> Result<Self, DnsError> {
        let (login_id, login_secret) = token
            .split_once(',')
            .ok_or_else(|| DnsError::new("dnspod token must use the \"ID,Token\" format"))?;
        if login_id.trim().is_empty() || login_secret.trim().is_empty() {
            return Err(DnsError::new("dnspod token must contain both ID and Token"));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("meatus-acme/0.8")
            .build()
            .map_err(|err| DnsError::new(format!("dnspod http client: {err}")))?;
        Ok(Self {
            http,
            base: base.into(),
            login_token: format!("{},{}", login_id.trim(), login_secret.trim()),
        })
    }

    async fn zone(&self, domain: &str) -> Result<DnsZone, DnsError> {
        for candidate in zone_candidates(domain) {
            let response = self
                .post(
                    "Domain.List",
                    &[("keyword", candidate.as_str()), ("length", "100")],
                    true,
                )
                .await?;
            if let Some(zone) = response.domains.iter().find(|zone| {
                zone.name
                    .trim_end_matches('.')
                    .eq_ignore_ascii_case(&candidate)
            }) {
                return Ok(DnsZone {
                    id: zone
                        .id
                        .clone()
                        .and_then(value_to_id)
                        .unwrap_or_else(|| zone.name.clone()),
                    name: zone.name.trim_end_matches('.').to_string(),
                });
            }
        }
        Err(DnsError::new(format!(
            "dnspod domain not found for {domain}"
        )))
    }

    async fn create_txt(
        &self,
        zone: &DnsZone,
        record_name: &str,
        value: &str,
    ) -> Result<String, DnsError> {
        let sub_domain = relative_record_name(record_name, &zone.name);
        let response = self
            .post(
                "Record.Create",
                &[
                    ("domain", zone.name.as_str()),
                    ("sub_domain", sub_domain.as_str()),
                    ("record_type", "TXT"),
                    ("record_line", "默认"),
                    ("value", value),
                    ("ttl", "600"),
                ],
                false,
            )
            .await?;
        response
            .record
            .and_then(|record| record.id)
            .and_then(value_to_id)
            .ok_or_else(|| DnsError::new("dnspod TXT record id missing"))
    }

    async fn delete_txt(&self, zone: &DnsZone, record_id: &str) -> Result<(), DnsError> {
        self.post(
            "Record.Remove",
            &[("domain", zone.name.as_str()), ("record_id", record_id)],
            false,
        )
        .await?;
        Ok(())
    }

    async fn post(
        &self,
        action: &str,
        params: &[(&str, &str)],
        allow_empty: bool,
    ) -> Result<DnspodResponse, DnsError> {
        let mut form = std::collections::BTreeMap::new();
        form.insert("login_token", self.login_token.as_str());
        form.insert("format", "json");
        for (key, value) in params {
            form.insert(*key, *value);
        }
        let response = self
            .http
            .post(format!("{}/{action}", self.base))
            .form(&form)
            .send()
            .await
            .map_err(|err| map_reqwest_error("dnspod", err))?;
        self.check_response(response, allow_empty).await
    }

    async fn check_response(
        &self,
        response: reqwest::Response,
        allow_empty: bool,
    ) -> Result<DnspodResponse, DnsError> {
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| DnsError::new(format!("dnspod response body: {err}")))?;
        let parsed = serde_json::from_str::<DnspodResponse>(&body).map_err(|err| {
            DnsError::new(format!(
                "dnspod API {} returned invalid JSON: {err}",
                status.as_u16()
            ))
        })?;
        if status.is_success() && parsed.status.code == "1" {
            return Ok(parsed);
        }
        if status.is_success() && allow_empty && parsed.status.code == "9" {
            return Ok(parsed);
        }
        let message = if parsed.status.message.trim().is_empty() {
            "request failed"
        } else {
            parsed.status.message.trim()
        };
        Err(DnsError::new(format!(
            "dnspod API {} (code {}): {message}",
            status.as_u16(),
            parsed.status.code
        )))
    }
}

fn value_to_id(value: serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value),
        serde_json::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[derive(Clone)]
struct DnsZone {
    id: String,
    name: String,
}

#[derive(Clone)]
enum DnsProvider {
    Cloudflare(CloudflareClient),
    Dnspod(DnspodClient),
}

impl DnsProvider {
    fn name(&self) -> &'static str {
        match self {
            Self::Cloudflare(_) => "cloudflare",
            Self::Dnspod(_) => "dnspod",
        }
    }

    async fn zone(&self, domain: &str) -> Result<DnsZone, DnsError> {
        match self {
            Self::Cloudflare(client) => client.zone(domain).await,
            Self::Dnspod(client) => client.zone(domain).await,
        }
    }

    async fn create_txt(
        &self,
        zone: &DnsZone,
        record_name: &str,
        value: &str,
    ) -> Result<String, DnsError> {
        match self {
            Self::Cloudflare(client) => client.create_txt(&zone.id, record_name, value).await,
            Self::Dnspod(client) => client.create_txt(zone, record_name, value).await,
        }
    }

    async fn delete_txt(&self, zone: &DnsZone, record_id: &str) -> Result<(), DnsError> {
        match self {
            Self::Cloudflare(client) => client.delete_txt(&zone.id, record_id).await,
            Self::Dnspod(client) => client.delete_txt(zone, record_id).await,
        }
    }
}

#[derive(Clone)]
struct Dns01Context {
    provider: DnsProvider,
    propagation: Duration,
}

fn zone_candidates(domain: &str) -> Vec<String> {
    let base = domain
        .strip_prefix("*.")
        .unwrap_or(domain)
        .trim_end_matches('.');
    let labels: Vec<&str> = base.split('.').filter(|label| !label.is_empty()).collect();
    let mut candidates = Vec::new();
    for start in 0..labels.len().saturating_sub(1) {
        candidates.push(labels[start..].join("."));
    }
    if candidates.is_empty() && !base.is_empty() {
        candidates.push(base.to_string());
    }
    candidates
}

fn txt_record_name(domain: &str) -> String {
    let base = domain.strip_prefix("*.").unwrap_or(domain);
    format!("_acme-challenge.{base}")
}

fn relative_record_name(record_name: &str, zone_name: &str) -> String {
    let record_name = record_name.trim_end_matches('.');
    let zone_name = zone_name.trim_end_matches('.');
    if record_name.eq_ignore_ascii_case(zone_name) {
        return "@".to_string();
    }
    let split_at = record_name.len().saturating_sub(zone_name.len() + 1);
    if split_at > 0 && record_name.is_char_boundary(split_at) {
        let (prefix, suffix) = record_name.split_at(split_at);
        let expected = format!(".{zone_name}");
        if suffix.eq_ignore_ascii_case(&expected) {
            return prefix.to_string();
        }
    }
    record_name.to_string()
}

fn wildcard_matches(pattern: &str, host: &str) -> bool {
    let Some(suffix) = pattern.strip_prefix("*.") else {
        return false;
    };
    if !host.ends_with(&format!(".{suffix}")) {
        return false;
    }
    let prefix = &host[..host.len() - suffix.len() - 1];
    !prefix.is_empty() && !prefix.contains('.')
}

fn certified_key_from_pem(pem: &[u8]) -> Result<Arc<CertifiedKey>, DnsError> {
    let mut reader = Cursor::new(pem);
    let certs = pem_certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| DnsError::new(format!("parse certificate PEM: {err}")))?;
    if certs.is_empty() {
        return Err(DnsError::new("no certificate found in PEM cache"));
    }
    let mut reader = Cursor::new(pem);
    let key = pem_private_key(&mut reader)
        .map_err(|err| DnsError::new(format!("parse private key PEM: {err}")))?
        .ok_or_else(|| DnsError::new("no private key found in PEM cache"))?;
    let signing_key = rustls::crypto::ring::sign::any_supported_type(&key)
        .map_err(|err| DnsError::new(format!("unsupported private key: {err}")))?;
    Ok(Arc::new(CertifiedKey::new(certs, signing_key)))
}

fn certified_key_not_after(key: &CertifiedKey) -> Option<i64> {
    let der: &CertificateDer<'_> = key.cert.first()?;
    let (_, cert) = x509_parser::parse_x509_certificate(der.as_ref()).ok()?;
    Some(cert.validity().not_after.timestamp())
}

fn certificate_needs_renewal(key: &CertifiedKey) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| {
            i64::try_from(value.as_secs()).unwrap_or(i64::MAX)
        });
    certified_key_not_after(key)
        .is_none_or(|not_after| not_after - now <= DNS_RENEW_BEFORE.as_secs() as i64)
}

#[derive(Debug, Default)]
struct MultiAcmeResolver {
    domains: RwLock<HashMap<String, Arc<rustls_acme::ResolvesServerCertAcme>>>,
    dns_certs: RwLock<HashMap<String, Arc<CertifiedKey>>>,
}

impl MultiAcmeResolver {
    fn insert(&self, domain: String, resolver: Arc<rustls_acme::ResolvesServerCertAcme>) {
        self.domains
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(domain, resolver);
    }

    fn insert_dns(&self, domain: String, cert: Arc<CertifiedKey>) {
        self.dns_certs
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(domain, cert);
    }

    fn has_domain(&self, domain: &str) -> bool {
        if self
            .domains
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(domain)
        {
            return true;
        }
        let certs = self
            .dns_certs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        certs.contains_key(domain)
            || certs
                .keys()
                .any(|pattern| wildcard_matches(pattern, domain))
    }

    fn remove(&self, domain: &str) {
        self.domains
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(domain);
        self.dns_certs
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(domain);
    }

    fn http01_key_auth(&self, token: &str) -> Option<String> {
        let domains = self
            .domains
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        domains
            .values()
            .find_map(|resolver| resolver.get_http_01_key_auth(token))
    }
}

impl ResolvesServerCert for MultiAcmeResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let domain = client_hello.server_name()?.to_ascii_lowercase();
        let exact_dns = {
            let certs = self
                .dns_certs
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            certs.get(&domain).cloned()
        };
        let wildcard_dns = {
            let certs = self
                .dns_certs
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            certs
                .iter()
                .filter(|(pattern, _)| wildcard_matches(pattern, &domain))
                .max_by_key(|(pattern, _)| pattern.len())
                .map(|(_, cert)| Arc::clone(cert))
        };
        let http_resolver = self
            .domains
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&domain)
            .cloned();
        if let Some(resolver) = http_resolver {
            if let Some(cert) = resolver.resolve(client_hello) {
                return Some(cert);
            }
            return exact_dns.or(wildcard_dns);
        }
        exact_dns.or(wildcard_dns)
    }
}

/// 某个域名的 DNS-01 签发状态，供管理面板展示失败原因。
#[derive(Debug, Clone)]
pub struct AcmeDomainStatus {
    pub state: &'static str,
    pub challenge: &'static str,
    pub error: Option<String>,
    pub updated_at: i64,
}

/// 一个进程内的 ACME 管理器：按域名懒创建状态机，共享缓存与 TLS resolver。
pub struct AcmeManager {
    resolver: Arc<MultiAcmeResolver>,
    tls_config: Arc<RustlsServerConfig>,
    directory: String,
    contacts: Vec<String>,
    cache: SecureDirCache,
    http_enabled: bool,
    challenge: meatus_core::AcmeChallenge,
    dns: Option<Dns01Context>,
    tasks: Mutex<HashMap<String, JoinHandle<()>>>,
    domains: Mutex<HashMap<String, Arc<rustls_acme::ResolvesServerCertAcme>>>,
    dns_status: Arc<Mutex<HashMap<String, AcmeDomainStatus>>>,
}

impl fmt::Debug for AcmeManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcmeManager")
            .field("directory", &self.directory)
            .field("challenge", &self.challenge)
            .field("domains", &self.domains.lock().map(|domains| domains.len()))
            .field("dns", &self.dns.is_some())
            .finish_non_exhaustive()
    }
}

impl AcmeManager {
    pub fn new(config: &meatus_core::ServerConfig) -> Result<Self, io::Error> {
        let section = &config.acme;
        let contacts = section
            .email
            .as_deref()
            .filter(|email| !email.trim().is_empty())
            .map(|email| vec![contact_uri(email)])
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "acme.email is required when acme.enabled = true",
                )
            })?;
        let directory = if section.staging {
            LETS_ENCRYPT_STAGING.to_string()
        } else {
            section.directory.clone()
        };
        let dns = match section.challenge {
            meatus_core::AcmeChallenge::Dns01 => {
                let provider = section
                    .dns_provider
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                if provider != "cloudflare" && provider != "dnspod" {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "acme.dns_provider = {provider:?} is not supported yet; \
                             supported providers: \"cloudflare\", \"dnspod\""
                        ),
                    ));
                }
                let token = section
                    .dns_api_token
                    .as_deref()
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "acme.dns_api_token is required for dns-01",
                        )
                    })?;
                let provider = match provider.as_str() {
                    "cloudflare" => {
                        DnsProvider::Cloudflare(CloudflareClient::new(token.to_string()).map_err(
                            |err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()),
                        )?)
                    }
                    "dnspod" => DnsProvider::Dnspod(DnspodClient::new(token.to_string()).map_err(
                        |err| io::Error::new(io::ErrorKind::InvalidInput, err.to_string()),
                    )?),
                    _ => unreachable!("provider already validated"),
                };
                Some(Dns01Context {
                    provider,
                    propagation: DNS_PROPAGATION_DELAY,
                })
            }
            _ => None,
        };
        let cache = SecureDirCache::new(&section.cert_dir)?;
        let resolver = Arc::new(MultiAcmeResolver::default());
        let tls_config = Arc::new(
            RustlsServerConfig::builder()
                .with_no_client_auth()
                .with_cert_resolver(Arc::clone(&resolver) as Arc<dyn ResolvesServerCert>),
        );
        Ok(Self {
            resolver,
            tls_config,
            directory,
            contacts,
            cache,
            http_enabled: config.http.enabled,
            challenge: section.challenge,
            dns,
            tasks: Mutex::new(HashMap::new()),
            domains: Mutex::new(HashMap::new()),
            dns_status: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    #[must_use]
    pub fn tls_config(&self) -> Arc<RustlsServerConfig> {
        Arc::clone(&self.tls_config)
    }

    #[must_use]
    pub fn has_domain(&self, domain: &str) -> bool {
        self.resolver.has_domain(&normalize_domain(domain))
    }

    /// 代理下线后释放该域名的 ACME 状态机与 resolver，避免继续续期或
    /// 在新 HTTPS 路由上误终止 TLS。
    pub fn remove_domain(&self, domain: &str) {
        let domain = normalize_domain(domain);
        if let Some(task) = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&domain)
        {
            task.abort();
        }
        self.domains
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&domain);
        self.resolver.remove(&domain);
        self.dns_status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&domain);
    }

    /// 为一个域名启动 ACME 状态机。精确域名走 HTTP-01，泛域名走 DNS-01。
    pub fn ensure_domain(&self, domain: &str) -> bool {
        let domain = normalize_domain(domain);
        if domain.is_empty() {
            return false;
        }
        let wildcard = domain.starts_with("*.");
        if wildcard {
            if self.challenge != meatus_core::AcmeChallenge::Dns01 || self.dns.is_none() {
                warn!(
                    %domain,
                    "wildcard requires acme.challenge = \"dns-01\"; falls back to sni"
                );
                return false;
            }
            return self.ensure_dns_domain(domain);
        }
        // 精确域名优先 HTTP-01；未开启 HTTP 时，DNS-01 也可签发精确域名。
        if !self.http_enabled {
            if self.challenge == meatus_core::AcmeChallenge::Dns01 && self.dns.is_some() {
                return self.ensure_dns_domain(domain);
            }
            warn!(%domain, "exact domain ACME requires http.enabled = true; falls back to sni");
            return false;
        }
        let mut domains = self
            .domains
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if domains.contains_key(&domain) {
            return true;
        }

        let config = AcmeConfig::new([domain.clone()])
            .directory(&self.directory)
            .contact(self.contacts.iter().cloned())
            .cache(self.cache.clone())
            .challenge_type(UseChallenge::Http01)
            .state();
        let resolver = config.resolver();
        self.resolver.insert(domain.clone(), Arc::clone(&resolver));
        domains.insert(domain.clone(), resolver);

        let task_domain = domain.clone();
        let mut state = config;
        let task = tokio::spawn(async move {
            while let Some(event) = state.next().await {
                match event {
                    Ok(event) => info!(%domain, event = ?event, "acme state event"),
                    Err(err) => warn!(%domain, error = %err, "acme state error"),
                }
            }
        });
        drop(domains);
        self.tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(task_domain, task);
        true
    }

    fn ensure_dns_domain(&self, domain: String) -> bool {
        let mut tasks = self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tasks.contains_key(&domain) {
            return true;
        }
        let Some(context) = self.dns.clone() else {
            return false;
        };
        update_dns_status(&self.dns_status, &domain, "issuing", None);

        let task_domain = domain.clone();
        let cache = self.cache.clone();
        let resolver = Arc::clone(&self.resolver);
        let directory = self.directory.clone();
        let contacts = self.contacts.clone();
        let status = Arc::clone(&self.dns_status);
        let task = tokio::spawn(async move {
            run_dns_task(
                task_domain,
                directory,
                contacts,
                cache,
                context,
                resolver,
                status,
            )
            .await;
        });
        tasks.insert(domain, task);
        true
    }

    #[must_use]
    pub fn http01_key_auth(&self, token: &str) -> Option<String> {
        self.resolver.http01_key_auth(token)
    }

    /// 查询某个域名的 DNS-01 状态；精确域名会回退到覆盖它的泛域名。
    #[must_use]
    pub fn domain_status(&self, domain: &str) -> Option<AcmeDomainStatus> {
        let domain = normalize_domain(domain);
        let statuses = self
            .dns_status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(status) = statuses.get(&domain) {
            return Some(status.clone());
        }
        statuses
            .iter()
            .filter(|(pattern, _)| wildcard_matches(pattern, &domain))
            .max_by_key(|(pattern, _)| pattern.len())
            .map(|(_, status)| status.clone())
    }

    /// 该域名实际使用的 challenge 标签。
    #[must_use]
    pub fn challenge_label(&self, domain: &str) -> &'static str {
        let domain = normalize_domain(domain);
        if domain.starts_with("*.") {
            return if self.challenge == meatus_core::AcmeChallenge::Dns01 && self.dns.is_some() {
                "dns-01"
            } else {
                "http-01"
            };
        }
        if !self.http_enabled
            && self.challenge == meatus_core::AcmeChallenge::Dns01
            && self.dns.is_some()
        {
            return "dns-01";
        }
        "http-01"
    }
}

impl Drop for AcmeManager {
    fn drop(&mut self) {
        for task in self
            .tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .map(|(_, task)| task)
        {
            task.abort();
        }
    }
}

fn update_dns_status(
    statuses: &Mutex<HashMap<String, AcmeDomainStatus>>,
    domain: &str,
    state: &'static str,
    error: Option<String>,
) {
    let updated_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| {
            i64::try_from(value.as_secs()).unwrap_or(i64::MAX)
        });
    statuses
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            domain.to_string(),
            AcmeDomainStatus {
                state,
                challenge: "dns-01",
                error,
                updated_at,
            },
        );
}

fn acme_retry_policy() -> RetryPolicy {
    RetryPolicy::new()
        .initial_delay(Duration::from_secs(2))
        .backoff(1.5)
        .timeout(Duration::from_secs(180))
}

async fn run_dns_task(
    domain: String,
    directory: String,
    contacts: Vec<String>,
    cache: SecureDirCache,
    context: Dns01Context,
    resolver: Arc<MultiAcmeResolver>,
    status: Arc<Mutex<HashMap<String, AcmeDomainStatus>>>,
) {
    let domains = [domain.clone()];
    let mut current = match cache.load_cert(&domains, &directory).await {
        Ok(Some(pem)) => match certified_key_from_pem(&pem) {
            Ok(key) => {
                resolver.insert_dns(domain.clone(), Arc::clone(&key));
                update_dns_status(&status, &domain, "managed", None);
                info!(%domain, "loaded cached dns-01 certificate");
                Some(key)
            }
            Err(err) => {
                warn!(%domain, error = %err, "cached dns-01 certificate is invalid");
                None
            }
        },
        Ok(None) => None,
        Err(err) => {
            warn!(%domain, error = %err, "read dns-01 certificate cache failed");
            None
        }
    };

    loop {
        if current
            .as_ref()
            .is_none_or(|key| certificate_needs_renewal(key))
        {
            update_dns_status(&status, &domain, "issuing", None);
            match issue_dns_cert(&domain, &directory, &contacts, &cache, &context).await {
                Ok(pem) => match certified_key_from_pem(pem.as_bytes()) {
                    Ok(key) => {
                        if let Err(err) =
                            cache.store_cert(&domains, &directory, pem.as_bytes()).await
                        {
                            warn!(%domain, error = %err, "store dns-01 certificate failed");
                        }
                        resolver.insert_dns(domain.clone(), Arc::clone(&key));
                        current = Some(key);
                        update_dns_status(&status, &domain, "managed", None);
                        info!(%domain, "dns-01 certificate issued");
                    }
                    Err(err) => {
                        update_dns_status(&status, &domain, "failed", Some(err.to_string()));
                        warn!(%domain, error = %err, "dns-01 certificate parse failed");
                    }
                },
                Err(err) => {
                    update_dns_status(&status, &domain, "failed", Some(err.to_string()));
                    warn!(%domain, error = %err, "dns-01 certificate issuance failed; keeping old certificate");
                }
            }
        }

        let failed = status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&domain)
            .is_some_and(|status| status.state == "failed");
        let delay = if failed {
            DNS_RETRY_DELAY
        } else {
            DNS_RENEW_CHECK
        };
        tokio::time::sleep(delay).await;
    }
}

async fn issue_dns_cert(
    domain: &str,
    directory: &str,
    contacts: &[String],
    cache: &SecureDirCache,
    context: &Dns01Context,
) -> Result<String, DnsError> {
    let account = load_dns_account(cache, directory, contacts).await?;
    let identifiers = [Identifier::Dns(domain.to_string())];
    let mut order = account
        .new_order(&NewOrder::new(&identifiers))
        .await
        .map_err(DnsError::from)?;
    let mut records = Vec::new();
    let result = async {
        {
            let mut authorizations = order.authorizations();
            while let Some(authorization) = authorizations.next().await {
                let mut authorization = authorization.map_err(DnsError::from)?;
                let identifier = authorization.identifier().to_string();
                let mut challenge = authorization
                    .challenge(ChallengeType::Dns01)
                    .ok_or_else(|| DnsError::new("dns-01 challenge not offered by ACME server"))?;
                let value = challenge.key_authorization().dns_value();
                let record_name = txt_record_name(&identifier);
                let zone = context.provider.zone(&identifier).await?;
                let record_id = context
                    .provider
                    .create_txt(&zone, &record_name, &value)
                    .await?;
                records.push((zone, record_id));
                info!(
                    %domain,
                    provider = context.provider.name(),
                    %record_name,
                    "DNS TXT record created; waiting for propagation"
                );
                tokio::time::sleep(context.propagation).await;
                challenge.set_ready().await.map_err(DnsError::from)?;
            }
        }

        let status = order
            .poll_ready(&acme_retry_policy())
            .await
            .map_err(DnsError::from)?;
        if status != instant_acme::OrderStatus::Ready {
            return Err(DnsError::new(format!(
                "dns-01 order did not become ready: {status:?}"
            )));
        }
        let private_key = order.finalize().await.map_err(DnsError::from)?;
        let certificate = order
            .poll_certificate(&acme_retry_policy())
            .await
            .map_err(DnsError::from)?;
        Ok(format!("{private_key}{certificate}"))
    }
    .await;

    for (zone, record_id) in records {
        if let Err(err) = context.provider.delete_txt(&zone, &record_id).await {
            warn!(%domain, provider = context.provider.name(), error = %err, "cleanup DNS TXT record failed");
        }
    }
    result
}

async fn load_dns_account(
    cache: &SecureDirCache,
    directory: &str,
    contacts: &[String],
) -> Result<Account, DnsError> {
    let contact_key = contacts.join(",");
    if let Some(credentials) = cache.load_dns_account(&contact_key, directory).await? {
        match serde_json::from_slice::<AccountCredentials>(&credentials) {
            Ok(credentials) => match Account::builder()?.from_credentials(credentials).await {
                Ok(account) => return Ok(account),
                Err(err) => warn!(error = %err, "restore dns-01 account failed; recreating"),
            },
            Err(err) => warn!(error = %err, "parse cached dns-01 account failed; recreating"),
        }
    }

    let contact_refs: Vec<&str> = contacts.iter().map(String::as_str).collect();
    let (account, credentials) = Account::builder()?
        .create(
            &NewAccount {
                contact: &contact_refs,
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            directory.to_string(),
            None,
        )
        .await
        .map_err(DnsError::from)?;
    let encoded = serde_json::to_vec(&credentials)?;
    if let Err(err) = cache
        .store_dns_account(&contact_key, directory, &encoded)
        .await
    {
        warn!(error = %err, "store dns-01 account failed");
    }
    Ok(account)
}

fn normalize_domain(domain: &str) -> String {
    domain.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn contact_uri(email: &str) -> String {
    let email = email.trim();
    if email.starts_with("mailto:") {
        email.to_string()
    } else {
        format!("mailto:{email}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cache_roundtrip_uses_0600_files() {
        let dir = std::env::temp_dir().join(format!(
            "meatus-acme-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let cache = SecureDirCache::new(&dir).expect("cache dir");
        let domains = vec!["Example.com".to_string()];
        cache
            .store_cert(&domains, "https://acme.test/directory", b"cert-pem")
            .await
            .expect("store cert");
        let loaded = cache
            .load_cert(&domains, "https://acme.test/directory")
            .await
            .expect("load cert");
        assert_eq!(loaded.as_deref(), Some(b"cert-pem".as_slice()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = cache.path(
                "cert",
                &cache_key(&["Example.com"], "https://acme.test/directory"),
            );
            let mode = std::fs::metadata(path)
                .expect("cert metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
            let dir_mode = std::fs::metadata(&dir)
                .expect("cache dir metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cloudflare_debug_does_not_leak_token() {
        let client = CloudflareClient::with_base(
            "super-secret-token".to_string(),
            "https://cloudflare.test",
        )
        .expect("client");
        let debug = format!("{client:?}");
        assert!(!debug.contains("super-secret-token"));
        assert!(debug.contains("https://cloudflare.test"));
    }

    #[test]
    fn dnspod_debug_does_not_leak_token() {
        let client = DnspodClient::with_base(
            "12345,super-secret-token".to_string(),
            "https://dnspod.test",
        )
        .expect("client");
        let debug = format!("{client:?}");
        assert!(!debug.contains("super-secret-token"));
        assert!(debug.contains("https://dnspod.test"));
    }

    #[test]
    fn dnspod_token_requires_id_and_secret() {
        assert!(DnspodClient::new("12345".to_string()).is_err());
        assert!(DnspodClient::new(",secret".to_string()).is_err());
        assert!(DnspodClient::new("12345,secret".to_string()).is_ok());
    }

    #[test]
    fn contact_uri_adds_mailto_prefix() {
        assert_eq!(contact_uri("ops@example.com"), "mailto:ops@example.com");
        assert_eq!(
            contact_uri("  mailto:ops@example.com  "),
            "mailto:ops@example.com"
        );
    }

    #[test]
    fn dns_record_name_and_zone_candidates() {
        assert_eq!(
            txt_record_name("*.tunnel.example.com"),
            "_acme-challenge.tunnel.example.com"
        );
        assert_eq!(
            zone_candidates("*.tunnel.example.com"),
            vec!["tunnel.example.com".to_string(), "example.com".to_string()]
        );
        assert_eq!(
            zone_candidates("example.com"),
            vec!["example.com".to_string()]
        );
        assert_eq!(
            relative_record_name("_acme-challenge.tunnel.example.com", "example.com"),
            "_acme-challenge.tunnel"
        );
        assert_eq!(
            relative_record_name("_acme-challenge.tunnel.example.com", "tunnel.example.com"),
            "_acme-challenge"
        );
        assert_eq!(relative_record_name("example.com", "example.com"), "@");
        assert_eq!(
            relative_record_name("_acme-challenge.other.com", "example.com"),
            "_acme-challenge.other.com"
        );
    }

    #[test]
    fn wildcard_matching_is_suffix_bounded() {
        assert!(wildcard_matches("*.example.com", "app.example.com"));
        assert!(!wildcard_matches("*.example.com", "a.b.example.com"));
        assert!(!wildcard_matches("*.example.com", "example.com"));
        assert!(!wildcard_matches("*.example.com", "notexample.com"));
    }

    #[test]
    fn parses_cached_dns_certificate_and_reads_expiry() {
        let cert = rcgen::generate_simple_self_signed(vec!["*.example.com".to_string()])
            .expect("generate cert");
        let pem = format!("{}{}", cert.signing_key.serialize_pem(), cert.cert.pem());
        let key = certified_key_from_pem(pem.as_bytes()).expect("parse cached cert");
        assert!(certified_key_not_after(&key).is_some());
        assert!(!certificate_needs_renewal(&key));
    }

    #[tokio::test]
    async fn cloudflare_zone_fallback_and_txt_lifecycle() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock cloudflare listener");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match socket.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let request = String::from_utf8_lossy(&buf);
                let first = request.lines().next().unwrap_or_default();
                let body = if first.starts_with("GET /zones?") && first.contains("name=example.com")
                {
                    r#"{"success":true,"result":[{"id":"zone1"}]}"#.to_string()
                } else if first.starts_with("GET /zones?") {
                    r#"{"success":true,"result":[]}"#.to_string()
                } else if first.starts_with("POST /zones/zone1/dns_records") {
                    r#"{"success":true,"result":{"id":"rec1"}}"#.to_string()
                } else if first.starts_with("DELETE /zones/zone1/dns_records/rec1") {
                    r#"{"success":true,"result":{}}"#.to_string()
                } else {
                    r#"{"success":false,"errors":[{"message":"not found"}]}"#.to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let client =
            CloudflareClient::with_base("test-token".to_string(), format!("http://{addr}"))
                .expect("cloudflare client");
        let zone = client
            .zone("*.tunnel.example.com")
            .await
            .expect("zone fallback");
        assert_eq!(zone.id, "zone1");
        let record = client
            .create_txt(&zone.id, "_acme-challenge.tunnel.example.com", "value")
            .await
            .expect("create TXT");
        assert_eq!(record, "rec1");
        client
            .delete_txt(&zone.id, &record)
            .await
            .expect("delete TXT");
        assert_eq!(client.zone("other.example.com").await.unwrap().id, "zone1");
    }

    #[tokio::test]
    async fn dnspod_domain_fallback_and_txt_lifecycle() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock dnspod listener");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match socket.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if let Some(header_end) = buf
                                .windows(4)
                                .position(|window| window == b"\r\n\r\n")
                                .map(|index| index + 4)
                            {
                                let content_length = String::from_utf8_lossy(&buf[..header_end])
                                    .lines()
                                    .find_map(|line| {
                                        let (name, value) = line.split_once(':')?;
                                        name.eq_ignore_ascii_case("content-length")
                                            .then(|| value.trim().parse::<usize>().ok())
                                            .flatten()
                                    })
                                    .unwrap_or(0);
                                if buf.len() >= header_end + content_length {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let request = String::from_utf8_lossy(&buf);
                let first = request.lines().next().unwrap_or_default();
                let body = if first.starts_with("POST /Domain.List") {
                    r#"{"status":{"code":"1","message":"ok"},"domains":[{"id":123,"name":"example.com"}]}"#.to_string()
                } else if first.starts_with("POST /Record.Create") {
                    assert!(
                        request.contains("sub_domain=_acme-challenge.tunnel"),
                        "unexpected Record.Create body: {request}"
                    );
                    r#"{"status":{"code":"1","message":"ok"},"record":{"id":"rec1"}}"#.to_string()
                } else if first.starts_with("POST /Record.Remove") {
                    r#"{"status":{"code":"1","message":"ok"}}"#.to_string()
                } else {
                    r#"{"status":{"code":"6","message":"not found"}}"#.to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let client =
            DnspodClient::with_base("12345,test-token".to_string(), format!("http://{addr}"))
                .expect("dnspod client");
        let zone = client
            .zone("*.tunnel.example.com")
            .await
            .expect("domain fallback");
        assert_eq!(zone.name, "example.com");
        assert_eq!(zone.id, "123");
        let record = client
            .create_txt(&zone, "_acme-challenge.tunnel.example.com", "value")
            .await
            .expect("create TXT");
        assert_eq!(record, "rec1");
        client.delete_txt(&zone, &record).await.expect("delete TXT");
    }
}

#[cfg(test)]
mod cached_cert_tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn wildcard_cert_terminates_child_domain() {
        let cert = rcgen::generate_simple_self_signed(vec!["*.example.com".to_string()])
            .expect("generate wildcard cert");
        let pem = format!("{}{}", cert.signing_key.serialize_pem(), cert.cert.pem());
        let key = certified_key_from_pem(pem.as_bytes()).expect("parse wildcard cert");
        let resolver = Arc::new(MultiAcmeResolver::default());
        resolver.insert_dns("*.example.com".to_string(), key);
        assert!(resolver.has_domain("app.example.com"));

        let tls = Arc::new(
            RustlsServerConfig::builder()
                .with_no_client_auth()
                .with_cert_resolver(Arc::clone(&resolver) as Arc<dyn ResolvesServerCert>),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let acceptor = tokio_rustls::TlsAcceptor::from(tls);
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let Ok((hello, host)) = crate::http::read_client_hello(&mut socket).await else {
                return;
            };
            assert_eq!(host, "app.example.com");
            let stream = crate::http::PrefixedStream::new(socket, hello);
            let Ok(mut tls) = acceptor.accept(stream).await else {
                return;
            };
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 1024];
            let _ = tls.read(&mut buf).await;
            let _ = tls
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await;
        });

        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.cert.der().clone()).expect("trust cert");
        let client = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
        let server_name = rustls::pki_types::ServerName::try_from("app.example.com")
            .expect("server name")
            .to_owned();
        let mut ok = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let Ok(stream) = tokio::net::TcpStream::connect(addr).await else {
                continue;
            };
            let Ok(mut tls) = connector.connect(server_name.clone(), stream).await else {
                continue;
            };
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if tls
                .write_all(b"GET / HTTP/1.1\r\nHost: app.example.com\r\n\r\n")
                .await
                .is_err()
            {
                continue;
            }
            let mut response = Vec::new();
            let _ = tls.read_to_end(&mut response).await;
            if String::from_utf8_lossy(&response).contains("200 OK") {
                ok = true;
                break;
            }
        }
        assert!(ok, "wildcard cert should terminate app.example.com");
    }

    #[tokio::test]
    async fn cached_cert_drives_tls_termination_with_client_hello_replay() {
        let dir = std::env::temp_dir().join(format!(
            "meatus-acme-tls-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let cert =
            rcgen::generate_simple_self_signed(vec!["acme.test.local".to_string()]).expect("cert");
        let cert_pem = cert.cert.pem();
        let cached = format!("{}{}", cert.signing_key.serialize_pem(), cert_pem);

        let directory_url = "https://acme.test/directory";
        let cache = SecureDirCache::new(&dir).expect("cache");
        cache
            .store_cert(
                &["acme.test.local".to_string()],
                directory_url,
                cached.as_bytes(),
            )
            .await
            .expect("seed cert");

        let config = meatus_core::parse_server_config(&format!(
            r#"
[server]
bind_addr = "127.0.0.1"
control_port = 7000
[auth]
token_sha256 = "{token}"
[http]
enabled = true
bind = "127.0.0.1:0"
https_bind = "127.0.0.1:0"
domain = "acme.test.local"
[acme]
enabled = true
email = "ops@example.com"
directory = "{directory_url}"
cert_dir = "{dir}"
staging = false
"#,
            token = "a".repeat(64),
            dir = dir.display(),
        ))
        .expect("server config");

        let manager = AcmeManager::new(&config).expect("acme manager");
        assert!(manager.ensure_domain("acme.test.local"));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let acceptor = tokio_rustls::TlsAcceptor::from(manager.tls_config());

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok((hello, host)) = crate::http::read_client_hello(&mut socket).await
                    else {
                        return;
                    };
                    assert_eq!(host, "acme.test.local");
                    let stream = crate::http::PrefixedStream::new(socket, hello);
                    let Ok(mut tls) = acceptor.accept(stream).await else {
                        return;
                    };
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = tls.read(&mut buf).await;
                    let _ = tls
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .await;
                });
            }
        });

        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.cert.der().clone()).expect("trust cert");
        let client = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(client));
        let server_name = rustls::pki_types::ServerName::try_from("acme.test.local")
            .expect("server name")
            .to_owned();

        let mut ok = false;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let Ok(stream) = tokio::net::TcpStream::connect(addr).await else {
                continue;
            };
            let Ok(mut tls) = connector.connect(server_name.clone(), stream).await else {
                continue;
            };
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if tls
                .write_all(b"GET / HTTP/1.1\r\nHost: acme.test.local\r\n\r\n")
                .await
                .is_err()
            {
                continue;
            }
            let mut response = Vec::new();
            let _ = tls.read_to_end(&mut response).await;
            if String::from_utf8_lossy(&response).contains("200 OK") {
                ok = true;
                break;
            }
        }
        let _ = std::fs::remove_dir_all(dir);
        assert!(ok, "cached ACME cert should complete TLS termination");
    }
}
