#![forbid(unsafe_code)]
//! V1.5 Relay 组签名配置包：JSON schema、canonical hash、Ed25519 签名/验签。
//!
//! 签名对象是 `config_hash` 字符串本身（`sha256:<hex>`），而 `config_hash`
//! 是除 `config_hash` / `signature` 外整个 body 的 canonical JSON 的 SHA-256。
//! 这样 bundle 可以重新格式化、字段顺序变化，验签结果不变。

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::config::{RelayRole, ServerConfig};

pub const BUNDLE_VERSION: u8 = 1;
pub const BUNDLE_PREFIX: &str = "sha256:";
pub const SIGNATURE_PREFIX: &str = "ed25519:";
pub const MAX_BUNDLE_BYTES: u64 = 1 << 20;

#[derive(Debug, Error)]
pub enum BundleError {
    #[error("cannot {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("bundle JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid relay bundle: {0}")]
    Invalid(String),
}

fn invalid(msg: impl Into<String>) -> BundleError {
    BundleError::Invalid(msg.into())
}

fn io_error(action: &'static str, path: &Path, source: std::io::Error) -> BundleError {
    BundleError::Io {
        action,
        path: path.display().to_string(),
        source,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayMember {
    pub id: String,
    pub role: RelayRole,
    pub control_addr: String,
    #[serde(default)]
    pub http_domains: Vec<String>,
    pub ports: [u16; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayBundleBody {
    pub version: u8,
    pub group: String,
    pub epoch: u64,
    #[serde(default)]
    pub generated_at: String,
    pub relays: Vec<RelayMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayBundle {
    pub version: u8,
    pub group: String,
    pub epoch: u64,
    pub generated_at: String,
    pub relays: Vec<RelayMember>,
    pub config_hash: String,
    pub signature: String,
}

impl RelayBundle {
    #[must_use]
    pub fn body(&self) -> RelayBundleBody {
        RelayBundleBody {
            version: self.version,
            group: self.group.clone(),
            epoch: self.epoch,
            generated_at: self.generated_at.clone(),
            relays: self.relays.clone(),
        }
    }

    pub fn validate_structure(&self) -> Result<(), BundleError> {
        self.body().validate()?;
        validate_config_hash(&self.config_hash)?;
        decode_signature(&self.signature)?;
        Ok(())
    }
}

impl RelayBundleBody {
    pub fn validate(&self) -> Result<(), BundleError> {
        if self.version != BUNDLE_VERSION {
            return Err(invalid(format!(
                "unsupported bundle version {}, expected {BUNDLE_VERSION}",
                self.version
            )));
        }
        validate_name("group", &self.group)?;
        if self.epoch == 0 {
            return Err(invalid("epoch must be >= 1"));
        }
        validate_timestamp(&self.generated_at)?;
        if self.relays.len() != 2 {
            return Err(invalid(format!(
                "relay group must contain exactly 2 members, got {}",
                self.relays.len()
            )));
        }

        let mut ids = HashSet::with_capacity(self.relays.len());
        let mut primary = 0usize;
        for (index, member) in self.relays.iter().enumerate() {
            member.validate(index)?;
            if !ids.insert(member.id.as_str()) {
                return Err(invalid(format!("duplicate relay id {:?}", member.id)));
            }
            if member.role == RelayRole::Primary {
                primary += 1;
            }
        }
        if primary != 1 {
            return Err(invalid(format!(
                "relay group must contain exactly one primary, got {primary}"
            )));
        }

        let mut ranges: Vec<[u16; 2]> = self.relays.iter().map(|relay| relay.ports).collect();
        ranges.sort_by_key(|range| range[0]);
        for window in ranges.windows(2) {
            if window[1][0] <= window[0][1] {
                return Err(invalid(format!(
                    "relay port ranges overlap: {:?} and {:?}",
                    window[0], window[1]
                )));
            }
        }
        Ok(())
    }
}

impl RelayMember {
    fn validate(&self, index: usize) -> Result<(), BundleError> {
        validate_name(&format!("relays[{index}].id"), &self.id)?;
        if !crate::config::is_valid_host_port(&self.control_addr) {
            return Err(invalid(format!(
                "relays[{index}].control_addr must be host:port with a non-zero port"
            )));
        }
        if self.http_domains.len() > 16 {
            return Err(invalid(format!(
                "relays[{index}].http_domains supports at most 16 entries"
            )));
        }
        let mut domains = HashSet::with_capacity(self.http_domains.len());
        for domain in &self.http_domains {
            let normalized = normalize_bundle_domain(domain).map_err(|err| {
                invalid(format!(
                    "relays[{index}].http_domains entry {domain:?}: {err}"
                ))
            })?;
            if !domains.insert(normalized) {
                return Err(invalid(format!(
                    "relays[{index}].http_domains contains duplicates"
                )));
            }
        }
        let [start, end] = self.ports;
        if start < 1024 || start > end {
            return Err(invalid(format!(
                "relays[{index}].ports must satisfy 1024 <= start <= end, got {start}..={end}"
            )));
        }
        Ok(())
    }
}

fn validate_name(field: &str, value: &str) -> Result<(), BundleError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid(format!("{field} must not be empty")));
    }
    if value.len() > 64 || value.chars().any(char::is_whitespace) {
        return Err(invalid(format!(
            "{field} must be <=64 characters and contain no whitespace"
        )));
    }
    Ok(())
}

fn validate_timestamp(value: &str) -> Result<(), BundleError> {
    let value = value.trim();
    if value.len() < 20 || value.len() > 64 {
        return Err(invalid(
            "generated_at must be an RFC3339-like timestamp of length 20..=64",
        ));
    }
    if !value.contains('T')
        || value.contains(char::is_whitespace)
        || !(value.ends_with('Z') || value.contains('+'))
    {
        return Err(invalid(
            "generated_at must be an RFC3339-like timestamp ending in Z or with a timezone offset",
        ));
    }
    Ok(())
}

fn validate_config_hash(value: &str) -> Result<(), BundleError> {
    let Some(hex) = value.trim().strip_prefix(BUNDLE_PREFIX) else {
        return Err(invalid("config_hash must use the \"sha256:HEX\" format"));
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid(
            "config_hash must contain exactly 64 hexadecimal characters",
        ));
    }
    Ok(())
}

fn normalize_bundle_domain(value: &str) -> Result<String, String> {
    let value = value.trim();
    let (wildcard, raw) = match value.strip_prefix("*.") {
        Some(raw) => (true, raw),
        None => (false, value),
    };
    let suffix = crate::net::normalize_domain(raw)?;
    if wildcard {
        Ok(format!("*.{suffix}"))
    } else {
        Ok(suffix)
    }
}

/// canonical body bytes：字段顺序由结构体固定，不包含 `config_hash` / `signature`。
pub fn canonical_body_bytes(body: &RelayBundleBody) -> Result<Vec<u8>, BundleError> {
    Ok(serde_json::to_vec(body)?)
}

pub fn compute_config_hash(body: &RelayBundleBody) -> Result<String, BundleError> {
    let bytes = canonical_body_bytes(body)?;
    let digest = Sha256::digest(&bytes);
    Ok(format!("{BUNDLE_PREFIX}{}", hex::encode(digest)))
}

pub struct RelaySigningKey {
    inner: Ed25519KeyPair,
}

impl RelaySigningKey {
    pub fn from_pkcs8(der: &[u8]) -> Result<Self, BundleError> {
        Ed25519KeyPair::from_pkcs8(der)
            .map(|inner| Self { inner })
            .map_err(|err| invalid(format!("invalid Ed25519 PKCS#8 signing key: {err:?}")))
    }

    /// 生成临时测试/运维密钥；私钥 PKCS#8 由调用方自行保存。
    pub fn generate() -> Result<(Self, Vec<u8>), BundleError> {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|err| invalid(format!("cannot generate Ed25519 key: {err:?}")))?;
        let der = pkcs8.as_ref().to_vec();
        Ok((Self::from_pkcs8(&der)?, der))
    }

    #[must_use]
    pub fn public_key(&self) -> String {
        format!(
            "{SIGNATURE_PREFIX}{}",
            STANDARD.encode(self.inner.public_key().as_ref())
        )
    }

    pub fn sign(&self, body: &RelayBundleBody) -> Result<RelayBundle, BundleError> {
        body.validate()?;
        let config_hash = compute_config_hash(body)?;
        let signature = self.inner.sign(config_hash.as_bytes());
        Ok(RelayBundle {
            version: body.version,
            group: body.group.clone(),
            epoch: body.epoch,
            generated_at: body.generated_at.clone(),
            relays: body.relays.clone(),
            config_hash,
            signature: format!("{SIGNATURE_PREFIX}{}", STANDARD.encode(signature.as_ref())),
        })
    }
}

pub fn parse_public_key(value: &str) -> Result<Vec<u8>, BundleError> {
    let Some(body) = value.trim().strip_prefix(SIGNATURE_PREFIX) else {
        return Err(invalid(
            "bundle_pubkey must use the \"ed25519:BASE64\" format",
        ));
    };
    let bytes = decode_base64(body)?;
    if bytes.len() != 32 {
        return Err(invalid(format!(
            "Ed25519 public key must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    Ok(bytes)
}

fn decode_signature(value: &str) -> Result<Vec<u8>, BundleError> {
    let Some(body) = value.trim().strip_prefix(SIGNATURE_PREFIX) else {
        return Err(invalid("signature must use the \"ed25519:BASE64\" format"));
    };
    let bytes = decode_base64(body)?;
    if bytes.len() != 64 {
        return Err(invalid(format!(
            "Ed25519 signature must be 64 bytes, got {}",
            bytes.len()
        )));
    }
    Ok(bytes)
}

fn decode_base64(value: &str) -> Result<Vec<u8>, BundleError> {
    let value = value.trim();
    STANDARD
        .decode(value)
        .or_else(|_| STANDARD_NO_PAD.decode(value))
        .or_else(|_| URL_SAFE_NO_PAD.decode(value))
        .map_err(|_| invalid("invalid base64 encoding"))
}

pub fn parse_bundle(raw: &str) -> Result<RelayBundle, BundleError> {
    let bundle: RelayBundle = serde_json::from_str(raw)?;
    bundle.validate_structure()?;
    Ok(bundle)
}

pub fn load_bundle(path: &Path) -> Result<RelayBundle, BundleError> {
    let meta = fs::metadata(path).map_err(|err| io_error("stat", path, err))?;
    if meta.len() > MAX_BUNDLE_BYTES {
        return Err(invalid(format!(
            "bundle file {} is larger than {MAX_BUNDLE_BYTES} bytes",
            path.display()
        )));
    }
    let raw = fs::read_to_string(path).map_err(|err| io_error("read", path, err))?;
    parse_bundle(&raw)
}

pub fn verify_bundle(bundle: &RelayBundle, public_key: &str) -> Result<(), BundleError> {
    bundle.validate_structure()?;
    let expected = compute_config_hash(&bundle.body())?;
    if expected != bundle.config_hash {
        return Err(invalid(format!(
            "config_hash mismatch: expected {expected}, got {}",
            bundle.config_hash
        )));
    }
    let key = parse_public_key(public_key)?;
    let signature = decode_signature(&bundle.signature)?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(bundle.config_hash.as_bytes(), &signature)
        .map_err(|_| invalid("Ed25519 signature verification failed"))
}

pub fn verify_against_config(
    config: &ServerConfig,
    bundle: &RelayBundle,
) -> Result<(), BundleError> {
    let Some(relay) = &config.relay else {
        return Err(invalid("server config has no [relay] section"));
    };
    if relay.group != bundle.group {
        return Err(invalid(format!(
            "bundle group {:?} does not match relay.group {:?}",
            bundle.group, relay.group
        )));
    }
    let Some(member) = bundle.relays.iter().find(|member| member.id == relay.id) else {
        return Err(invalid(format!(
            "relay.id {:?} is not present in bundle",
            relay.id
        )));
    };
    if member.role != relay.role {
        return Err(invalid(format!(
            "bundle member {:?} role {:?} does not match relay.role {:?}",
            relay.id,
            member.role.as_str(),
            relay.role.as_str()
        )));
    }
    if member.control_addr != relay.advertise_addr {
        return Err(invalid(format!(
            "bundle member {:?} control_addr {:?} does not match relay.advertise_addr {:?}",
            relay.id, member.control_addr, relay.advertise_addr
        )));
    }
    if member.ports != config.ports.range {
        return Err(invalid(format!(
            "bundle member {:?} ports {:?} do not match ports.range {:?}",
            relay.id, member.ports, config.ports.range
        )));
    }
    if relay.role == RelayRole::Secondary && config.acme.enabled {
        return Err(invalid(
            "secondary relay must not enable acme; only primary issues certificates",
        ));
    }
    Ok(())
}

/// 服务端启动时调用；`[relay]` 缺省返回 `None`。
pub fn verify_for_config(config: &ServerConfig) -> Result<Option<RelayBundle>, BundleError> {
    let Some(relay) = &config.relay else {
        return Ok(None);
    };
    let bundle = load_bundle(&relay.bundle)?;
    verify_bundle(&bundle, &relay.bundle_pubkey)?;
    verify_against_config(config, &bundle)?;
    check_epoch_rollback(&relay.bundle, bundle.epoch)?;
    Ok(Some(bundle))
}

pub fn write_bundle(path: &Path, bundle: &RelayBundle) -> Result<(), BundleError> {
    let mut raw = serde_json::to_string_pretty(bundle)?;
    raw.push('\n');
    write_atomic(path, raw.as_bytes(), 0o644)
}

pub fn write_epoch_state(bundle_path: &Path, epoch: u64) -> Result<(), BundleError> {
    write_atomic(
        &epoch_state_path(bundle_path),
        format!("{epoch}\n").as_bytes(),
        0o600,
    )
}

#[must_use]
pub fn epoch_state_path(bundle_path: &Path) -> PathBuf {
    let mut path = bundle_path.as_os_str().to_os_string();
    path.push(".epoch");
    PathBuf::from(path)
}

fn read_epoch_state(bundle_path: &Path) -> Result<Option<u64>, BundleError> {
    let path = epoch_state_path(bundle_path);
    match fs::read_to_string(&path) {
        Ok(raw) => {
            let epoch = raw.trim().parse::<u64>().map_err(|_| {
                invalid(format!(
                    "invalid relay epoch state {}: expected an integer",
                    path.display()
                ))
            })?;
            Ok(Some(epoch))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io_error("read", &path, err)),
    }
}

pub fn check_epoch_rollback(bundle_path: &Path, epoch: u64) -> Result<(), BundleError> {
    if let Some(last) = read_epoch_state(bundle_path)?
        && epoch < last
    {
        return Err(invalid(format!(
            "relay bundle epoch rollback: {epoch} < last accepted {last}"
        )));
    }
    Ok(())
}

pub fn current_epoch(bundle_path: &Path) -> Result<u64, BundleError> {
    let mut epoch = read_epoch_state(bundle_path)?.unwrap_or(0);
    if let Ok(bundle) = load_bundle(bundle_path) {
        epoch = epoch.max(bundle.epoch);
    }
    Ok(epoch)
}

fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<(), BundleError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|err| io_error("create directory", parent, err))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid(format!("path {} has no file name", path.display())))?;
    let tmp = parent.join(format!(
        ".{}.tmp.{}.{}",
        name.to_string_lossy(),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.subsec_nanos())
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|err| io_error("create", &tmp, err))?;
    if let Err(err) = file.write_all(bytes).and_then(|()| file.flush()) {
        let _ = fs::remove_file(&tmp);
        return Err(io_error("write", &tmp, err));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))
            .map_err(|err| io_error("chmod", &tmp, err))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    fs::rename(&tmp, path).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        io_error("rename", path, err)
    })
}

/// 当前 UTC 的 RFC3339 秒级时间戳；不引入额外时间库。
#[must_use]
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    format_rfc3339(secs)
}

fn format_rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let remainder = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = remainder / 3600;
    let minute = (remainder % 3600) / 60;
    let second = remainder % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    (
        year + if month <= 2 { 1 } else { 0 },
        month as u32,
        day as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_server_config;

    fn body() -> RelayBundleBody {
        RelayBundleBody {
            version: BUNDLE_VERSION,
            group: "prod".to_string(),
            epoch: 3,
            generated_at: "2026-11-23T00:00:00Z".to_string(),
            relays: vec![
                RelayMember {
                    id: "relay-a".to_string(),
                    role: RelayRole::Primary,
                    control_addr: "relay-a.example.com:7000".to_string(),
                    http_domains: vec!["*.example.com".to_string()],
                    ports: [20_000, 20_999],
                },
                RelayMember {
                    id: "relay-b".to_string(),
                    role: RelayRole::Secondary,
                    control_addr: "relay-b.example.com:7000".to_string(),
                    http_domains: vec!["*.example.com".to_string()],
                    ports: [21_000, 22_000],
                },
            ],
        }
    }

    fn key() -> (RelaySigningKey, String) {
        let (key, _pkcs8) = RelaySigningKey::generate().expect("generate key");
        let public = key.public_key();
        (key, public)
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let (signing, public) = key();
        let bundle = signing.sign(&body()).expect("sign");
        assert!(bundle.config_hash.starts_with("sha256:"));
        assert!(bundle.signature.starts_with("ed25519:"));
        verify_bundle(&bundle, &public).expect("verify");
    }

    #[test]
    fn tampered_body_fails_hash_check() {
        let (signing, public) = key();
        let mut bundle = signing.sign(&body()).expect("sign");
        bundle.epoch = 4;
        let err = verify_bundle(&bundle, &public).expect_err("must fail");
        assert!(err.to_string().contains("config_hash mismatch"), "{err}");
    }

    #[test]
    fn tampered_signature_fails() {
        let (signing, public) = key();
        let mut bundle = signing.sign(&body()).expect("sign");
        bundle.signature = format!("{SIGNATURE_PREFIX}{}", STANDARD.encode([0u8; 64]));
        let err = verify_bundle(&bundle, &public).expect_err("must fail");
        assert!(
            err.to_string().contains("signature verification failed"),
            "{err}"
        );
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let mut body = body();
        body.relays[1].id = body.relays[0].id.clone();
        let err = body.validate().expect_err("must fail");
        assert!(err.to_string().contains("duplicate relay id"), "{err}");
    }

    #[test]
    fn overlapping_ports_are_rejected() {
        let mut body = body();
        body.relays[1].ports = [20_999, 22_000];
        let err = body.validate().expect_err("must fail");
        assert!(err.to_string().contains("port ranges overlap"), "{err}");
    }

    #[test]
    fn bundle_rejects_unknown_secret_fields() {
        let (signing, _public) = key();
        let bundle = signing.sign(&body()).expect("sign");
        let mut value = serde_json::to_value(&bundle).expect("json");
        value
            .as_object_mut()
            .expect("object")
            .insert("token".to_string(), serde_json::json!("secret"));
        let raw = serde_json::to_string(&value).expect("serialize");
        let err = parse_bundle(&raw).expect_err("must fail");
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn config_binding_checks_group_member_and_ports() {
        let (signing, public) = key();
        let bundle = signing.sign(&body()).expect("sign");
        let dir = std::env::temp_dir().join(format!("meatus-bundle-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("relay-bundle.json");
        write_bundle(&path, &bundle).expect("write bundle");

        let toml = format!(
            r#"
[server]
control_port = 7000

[auth]
token_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[ports]
range = [20000, 20999]

[relay]
id = "relay-a"
group = "prod"
role = "primary"
advertise_addr = "relay-a.example.com:7000"
bundle = "{}"
bundle_pubkey = "{}"
peer_secret_file = "/etc/meatus/relay-peer.key"
"#,
            path.display(),
            public
        );
        let config = parse_server_config(&toml).expect("config");
        let loaded = verify_for_config(&config).expect("verify").expect("relay");
        assert_eq!(loaded.epoch, 3);

        let mut wrong = config.clone();
        wrong.relay.as_mut().expect("relay").group = "stage".to_string();
        let err = verify_against_config(&wrong, &bundle).expect_err("must fail");
        assert!(err.to_string().contains("does not match"), "{err}");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn epoch_rollback_is_rejected() {
        let dir = std::env::temp_dir().join(format!("meatus-epoch-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("relay-bundle.json");
        write_epoch_state(&path, 5).expect("epoch state");
        let err = check_epoch_rollback(&path, 4).expect_err("must fail");
        assert!(err.to_string().contains("epoch rollback"), "{err}");
        check_epoch_rollback(&path, 5).expect("equal epoch is allowed");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rfc3339_epoch_zero_is_known() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
    }
}
