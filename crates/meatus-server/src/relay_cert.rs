#![forbid(unsafe_code)]
//! Relay 组证书快照与 secondary 只读证书库。
//!
//! 只同步 `cert-*.pem`：证书链与私钥。`account-*.pem` 明确排除，避免 secondary
//! 获得 ACME 账户私钥并具备下单能力；证书指纹用于主备一致性校验。

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use rustls::ServerConfig as RustlsServerConfig;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls_pemfile::{certs as pem_certs, private_key as pem_private_key};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;
use x509_parser::extensions::GeneralName;

const SNAPSHOT_VERSION: u8 = 1;
const MAX_FILES: usize = 16;
const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
const CERT_PREFIX: &str = "cert-";

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelayCertFile {
    pub name: String,
    pub pem: String,
}

impl fmt::Debug for RelayCertFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayCertFile")
            .field("name", &self.name)
            .field("pem", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelayCertEntry {
    pub file: String,
    pub domains: Vec<String>,
    pub fingerprint: String,
    pub not_after: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RelayCertSnapshot {
    pub version: u8,
    pub group: String,
    pub epoch: u64,
    pub generated_at: String,
    pub files: Vec<RelayCertFile>,
    pub entries: Vec<RelayCertEntry>,
}

fn scan_cache(dir: &Path) -> Result<(Vec<RelayCertFile>, Vec<RelayCertEntry>), RelayCertError> {
    let mut files = Vec::new();
    let mut entries = Vec::new();
    let dir = fs::read_dir(dir).map_err(|err| {
        RelayCertError::io(format!("read acme cert dir {}: {err}", dir.display()))
    })?;
    for entry in dir {
        let entry =
            entry.map_err(|err| RelayCertError::io(format!("read acme dir entry: {err}")))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !valid_cert_file_name(name) {
            continue;
        }
        let bytes = fs::read(&path)
            .map_err(|err| RelayCertError::io(format!("read {}: {err}", path.display())))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(RelayCertError::invalid(format!(
                "{} exceeds {MAX_FILE_BYTES} bytes",
                path.display()
            )));
        }
        let pem = String::from_utf8(bytes)
            .map_err(|_| RelayCertError::invalid(format!("{} is not UTF-8 PEM", path.display())))?;
        let parsed = parse_cert_pem(&pem)
            .map_err(|err| RelayCertError::invalid(format!("{}: {err}", path.display())))?;
        files.push(RelayCertFile {
            name: name.to_string(),
            pem,
        });
        entries.push(RelayCertEntry {
            file: name.to_string(),
            domains: parsed.domains,
            fingerprint: parsed.fingerprint,
            not_after: parsed.not_after,
        });
    }
    if files.len() > MAX_FILES {
        return Err(RelayCertError::invalid(format!(
            "cert cache contains {} files; max {MAX_FILES}",
            files.len()
        )));
    }
    files.sort_by(|left, right| left.name.cmp(&right.name));
    entries.sort_by(|left, right| left.file.cmp(&right.file));
    Ok((files, entries))
}

impl RelayCertSnapshot {
    pub fn from_cache(group: &str, epoch: u64, dir: &Path) -> Result<Self, RelayCertError> {
        let (files, entries) = scan_cache(dir)?;
        let snapshot = Self {
            version: SNAPSHOT_VERSION,
            group: group.to_string(),
            epoch,
            generated_at: now_unix().to_string(),
            files,
            entries,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<(), RelayCertError> {
        if self.version != SNAPSHOT_VERSION {
            return Err(RelayCertError::invalid(format!(
                "snapshot.version must be {SNAPSHOT_VERSION}"
            )));
        }
        if self.group.trim().is_empty() {
            return Err(RelayCertError::invalid("snapshot.group must not be empty"));
        }
        if self.files.len() > MAX_FILES {
            return Err(RelayCertError::invalid(format!(
                "snapshot.files exceeds {MAX_FILES}"
            )));
        }
        if self.files.len() != self.entries.len() {
            return Err(RelayCertError::invalid(
                "snapshot.files and snapshot.entries must have the same length",
            ));
        }
        let total: usize = self.files.iter().map(|file| file.pem.len()).sum();
        if total > MAX_SNAPSHOT_BYTES {
            return Err(RelayCertError::invalid(format!(
                "snapshot body exceeds {MAX_SNAPSHOT_BYTES} bytes"
            )));
        }
        let mut names = std::collections::HashSet::with_capacity(self.files.len());
        for file in &self.files {
            if !valid_cert_file_name(&file.name) {
                return Err(RelayCertError::invalid(format!(
                    "snapshot file {:?} is not a cert-<64 hex>.pem name",
                    file.name
                )));
            }
            if !names.insert(file.name.as_str()) {
                return Err(RelayCertError::invalid(format!(
                    "snapshot contains duplicate file {:?}",
                    file.name
                )));
            }
            if file.pem.len() > MAX_FILE_BYTES {
                return Err(RelayCertError::invalid(format!(
                    "snapshot file {:?} exceeds {MAX_FILE_BYTES} bytes",
                    file.name
                )));
            }
            let parsed = parse_cert_pem(&file.pem).map_err(|err| {
                RelayCertError::invalid(format!("snapshot file {:?}: {err}", file.name))
            })?;
            let entry = self
                .entries
                .iter()
                .find(|entry| entry.file == file.name)
                .ok_or_else(|| {
                    RelayCertError::invalid(format!(
                        "snapshot entry for file {:?} is missing",
                        file.name
                    ))
                })?;
            if entry.fingerprint != parsed.fingerprint
                || entry.not_after != parsed.not_after
                || entry.domains != parsed.domains
            {
                return Err(RelayCertError::invalid(format!(
                    "snapshot entry for file {:?} does not match cert",
                    file.name
                )));
            }
        }
        for entry in &self.entries {
            if !names.contains(entry.file.as_str()) {
                return Err(RelayCertError::invalid(format!(
                    "snapshot entry {:?} has no file",
                    entry.file
                )));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn fingerprints(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.fingerprint.clone())
            .collect()
    }
}

#[derive(Debug)]
pub struct RelayCertError(String);

impl RelayCertError {
    fn invalid(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    fn io(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for RelayCertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RelayCertError {}

#[derive(Debug, Clone)]
struct ParsedCert {
    domains: Vec<String>,
    fingerprint: String,
    not_after: i64,
}

fn parse_cert_pem(pem: &str) -> Result<ParsedCert, RelayCertError> {
    let mut reader = Cursor::new(pem.as_bytes());
    let certs = pem_certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| RelayCertError::invalid(format!("parse certificate PEM: {err}")))?;
    let leaf = certs
        .first()
        .ok_or_else(|| RelayCertError::invalid("certificate PEM contains no certificate"))?;
    let mut reader = Cursor::new(pem.as_bytes());
    let key = pem_private_key(&mut reader)
        .map_err(|err| RelayCertError::invalid(format!("parse private key PEM: {err}")))?
        .ok_or_else(|| RelayCertError::invalid("certificate PEM contains no private key"))?;
    let _signing_key = rustls::crypto::ring::sign::any_supported_type(&key)
        .map_err(|err| RelayCertError::invalid(format!("unsupported private key: {err}")))?;
    let (_, cert) = x509_parser::parse_x509_certificate(leaf.as_ref())
        .map_err(|err| RelayCertError::invalid(format!("parse x509 certificate: {err}")))?;
    let mut domains = Vec::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for name in &san.value.general_names {
            if let GeneralName::DNSName(domain) = name {
                let domain = normalize_domain(domain);
                if !domain.is_empty() && !domains.contains(&domain) {
                    domains.push(domain);
                }
            }
        }
    }
    domains.sort();
    if domains.is_empty() {
        return Err(RelayCertError::invalid(
            "certificate has no DNS subject alternative name",
        ));
    }
    let fingerprint = format!("sha256:{}", hex::encode(Sha256::digest(leaf.as_ref())));
    let not_after = cert.validity().not_after.timestamp();
    Ok(ParsedCert {
        domains,
        fingerprint,
        not_after,
    })
}

fn normalize_domain(value: &str) -> String {
    value.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn valid_cert_file_name(name: &str) -> bool {
    let Some(hash) = name
        .strip_prefix(CERT_PREFIX)
        .and_then(|rest| rest.strip_suffix(".pem"))
    else {
        return false;
    };
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| {
            i64::try_from(value.as_secs()).unwrap_or(i64::MAX)
        })
}

fn certified_key_from_pem(pem: &str) -> Result<Arc<CertifiedKey>, RelayCertError> {
    let mut reader = Cursor::new(pem.as_bytes());
    let certs = pem_certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| RelayCertError::invalid(format!("parse certificate PEM: {err}")))?;
    if certs.is_empty() {
        return Err(RelayCertError::invalid(
            "certificate PEM contains no certificate",
        ));
    }
    let mut reader = Cursor::new(pem.as_bytes());
    let key = pem_private_key(&mut reader)
        .map_err(|err| RelayCertError::invalid(format!("parse private key PEM: {err}")))?
        .ok_or_else(|| RelayCertError::invalid("certificate PEM contains no private key"))?;
    let signing_key = rustls::crypto::ring::sign::any_supported_type(&key)
        .map_err(|err| RelayCertError::invalid(format!("unsupported private key: {err}")))?;
    Ok(Arc::new(CertifiedKey::new(certs, signing_key)))
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

#[derive(Default)]
struct RelayCertResolver {
    certs: RwLock<HashMap<String, Arc<CertifiedKey>>>,
}

impl fmt::Debug for RelayCertResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayCertResolver")
            .field(
                "domains",
                &self
                    .certs
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len(),
            )
            .finish()
    }
}

impl RelayCertResolver {
    fn replace(&self, certs: HashMap<String, Arc<CertifiedKey>>) {
        *self
            .certs
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = certs;
    }

    fn has_domain(&self, domain: &str) -> bool {
        let domain = normalize_domain(domain);
        let certs = self
            .certs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        certs.contains_key(&domain)
            || certs
                .keys()
                .any(|pattern| wildcard_matches(pattern, &domain))
    }
}

impl ResolvesServerCert for RelayCertResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let domain = normalize_domain(client_hello.server_name()?);
        let certs = self
            .certs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cert) = certs.get(&domain) {
            return Some(Arc::clone(cert));
        }
        certs
            .iter()
            .filter(|(pattern, _)| wildcard_matches(pattern, &domain))
            .max_by_key(|(pattern, _)| pattern.len())
            .map(|(_, cert)| Arc::clone(cert))
    }
}

pub struct RelayCertStore {
    dir: PathBuf,
    resolver: Arc<RelayCertResolver>,
    tls_config: Arc<RustlsServerConfig>,
}

impl fmt::Debug for RelayCertStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayCertStore")
            .field("dir", &self.dir)
            .field("resolver", &self.resolver)
            .finish()
    }
}

impl RelayCertStore {
    pub fn new(dir: &Path) -> Result<Self, RelayCertError> {
        fs::create_dir_all(dir).map_err(|err| {
            RelayCertError::io(format!("create cert dir {}: {err}", dir.display()))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|err| {
                RelayCertError::io(format!("chmod cert dir {}: {err}", dir.display()))
            })?;
        }
        let resolver = Arc::new(RelayCertResolver::default());
        let tls_config = Arc::new(
            RustlsServerConfig::builder()
                .with_no_client_auth()
                .with_cert_resolver(Arc::clone(&resolver) as Arc<dyn ResolvesServerCert>),
        );
        let store = Self {
            dir: dir.to_path_buf(),
            resolver,
            tls_config,
        };
        store.refresh()?;
        Ok(store)
    }

    #[must_use]
    pub fn tls_config(&self) -> Arc<RustlsServerConfig> {
        Arc::clone(&self.tls_config)
    }

    #[must_use]
    pub fn has_domain(&self, domain: &str) -> bool {
        self.resolver.has_domain(domain)
    }

    pub fn refresh(&self) -> Result<(), RelayCertError> {
        let (files, entries) = scan_cache(&self.dir)?;
        let mut certs = HashMap::new();
        for file in &files {
            let key = certified_key_from_pem(&file.pem)?;
            let entry = entries
                .iter()
                .find(|entry| entry.file == file.name)
                .expect("scan_cache emits matching entries");
            for domain in &entry.domains {
                certs.insert(domain.clone(), Arc::clone(&key));
            }
        }
        self.resolver.replace(certs);
        Ok(())
    }

    pub fn apply(&self, snapshot: &RelayCertSnapshot) -> Result<(), RelayCertError> {
        snapshot.validate()?;
        let mut written = std::collections::HashSet::with_capacity(snapshot.files.len());
        for file in &snapshot.files {
            if !valid_cert_file_name(&file.name) {
                return Err(RelayCertError::invalid(format!(
                    "refuse to write unsafe cert file {:?}",
                    file.name
                )));
            }
            let path = self.dir.join(&file.name);
            let temp = self
                .dir
                .join(format!(".{}.tmp-{}", file.name, std::process::id()));
            fs::write(&temp, file.pem.as_bytes())
                .map_err(|err| RelayCertError::io(format!("write {}: {err}", temp.display())))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&temp, fs::Permissions::from_mode(0o600)).map_err(|err| {
                    RelayCertError::io(format!("chmod {}: {err}", temp.display()))
                })?;
            }
            fs::rename(&temp, &path).map_err(|err| {
                let _ = fs::remove_file(&temp);
                RelayCertError::io(format!("rename {}: {err}", path.display()))
            })?;
            written.insert(file.name.as_str());
        }
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if valid_cert_file_name(&name)
                    && !written.contains(name.as_str())
                    && let Err(err) = fs::remove_file(entry.path())
                {
                    warn!(file = %name, error = %err, "remove stale relay cert failed");
                }
            }
        }
        self.refresh()
    }

    #[cfg(test)]
    pub(crate) fn cert_for_test(&self, domain: &str) -> Option<Arc<CertifiedKey>> {
        let resolver = &self.resolver;
        let certs = resolver
            .certs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        certs.get(&normalize_domain(domain)).cloned().or_else(|| {
            certs
                .iter()
                .filter(|(pattern, _)| wildcard_matches(pattern, &normalize_domain(domain)))
                .max_by_key(|(pattern, _)| pattern.len())
                .map(|(_, cert)| Arc::clone(cert))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_cert(dir: &Path, name: &str, domains: &[&str]) -> String {
        let certified = rcgen::generate_simple_self_signed(
            domains.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
        )
        .expect("cert");
        let pem = format!(
            "{}{}",
            certified.cert.pem(),
            certified.signing_key.serialize_pem()
        );
        let path = dir.join(name);
        fs::write(&path, &pem).expect("write cert");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
        }
        pem
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("meatus-relay-cert-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn snapshot_excludes_account_and_matches_fingerprint() {
        let dir = temp_dir("snapshot");
        let hash = "a".repeat(64);
        let pem = write_cert(&dir, &format!("cert-{hash}.pem"), &["app.test.local"]);
        fs::write(dir.join("account-secret.pem"), "account-key").expect("account");

        let snapshot = RelayCertSnapshot::from_cache("prod", 3, &dir).expect("snapshot");
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].domains, vec!["app.test.local"]);
        assert!(snapshot.entries[0].fingerprint.starts_with("sha256:"));
        assert!(snapshot.files[0].pem.contains(&pem[..32]));
        let debug = format!("{:?}", snapshot.files[0]);
        assert!(!debug.contains("PRIVATE KEY"), "{debug}");
        snapshot.validate().expect("valid snapshot");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn store_applies_snapshot_and_resolves_wildcard() {
        let source = temp_dir("source");
        let target = temp_dir("target");
        let hash = "b".repeat(64);
        write_cert(&source, &format!("cert-{hash}.pem"), &["*.test.local"]);
        let snapshot = RelayCertSnapshot::from_cache("prod", 3, &source).expect("snapshot");
        let store = RelayCertStore::new(&target).expect("store");
        store.apply(&snapshot).expect("apply");
        assert!(store.has_domain("app.test.local"));
        assert!(!store.has_domain("test.local"));
        assert!(store.cert_for_test("app.test.local").is_some());
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(target);
    }

    #[test]
    fn snapshot_rejects_path_traversal_and_bad_fingerprint() {
        let dir = temp_dir("reject");
        let hash = "c".repeat(64);
        write_cert(&dir, &format!("cert-{hash}.pem"), &["app.test.local"]);
        let mut snapshot = RelayCertSnapshot::from_cache("prod", 3, &dir).expect("snapshot");
        snapshot.files[0].name = "../evil.pem".to_string();
        let err = snapshot.validate().expect_err("path traversal");
        assert!(err.to_string().contains("cert-"), "{err}");

        snapshot.files[0].name = format!("cert-{hash}.pem");
        snapshot.entries[0].fingerprint = format!("sha256:{}", "0".repeat(64));
        let err = snapshot.validate().expect_err("fingerprint");
        assert!(err.to_string().contains("does not match"), "{err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_cert_files_are_removed_but_account_files_remain() {
        let source = temp_dir("stale-source");
        let target = temp_dir("stale-target");
        let hash = "d".repeat(64);
        write_cert(&source, &format!("cert-{hash}.pem"), &["app.test.local"]);
        let stale = format!("cert-{}.pem", "e".repeat(64));
        write_cert(&target, &stale, &["old.test.local"]);
        fs::write(target.join("account-keep.pem"), "account").expect("account");
        let snapshot = RelayCertSnapshot::from_cache("prod", 3, &source).expect("snapshot");
        let store = RelayCertStore::new(&target).expect("store");
        store.apply(&snapshot).expect("apply");
        assert!(!target.join(&stale).exists());
        assert!(target.join("account-keep.pem").exists());
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(target);
    }

    #[test]
    fn snapshot_is_bounded() {
        let snapshot = RelayCertSnapshot {
            version: 1,
            group: "prod".to_string(),
            epoch: 3,
            generated_at: now_unix().to_string(),
            files: vec![RelayCertFile {
                name: "cert-not-a-hash.pem".to_string(),
                pem: "pem".to_string(),
            }],
            entries: vec![RelayCertEntry {
                file: "cert-not-a-hash.pem".to_string(),
                domains: vec!["app.test.local".to_string()],
                fingerprint: format!("sha256:{}", "0".repeat(64)),
                not_after: 1,
            }],
        };
        let err = snapshot.validate().expect_err("bad name");
        assert!(err.to_string().contains("cert-"), "{err}");
    }

    #[test]
    fn apply_rejects_unsafe_names() {
        let dir = temp_dir("unsafe");
        let store = RelayCertStore::new(&dir).expect("store");
        let snapshot = RelayCertSnapshot {
            version: 1,
            group: "prod".to_string(),
            epoch: 3,
            generated_at: now_unix().to_string(),
            files: vec![RelayCertFile {
                name: "../evil.pem".to_string(),
                pem: "pem".to_string(),
            }],
            entries: vec![RelayCertEntry {
                file: "../evil.pem".to_string(),
                domains: vec!["app.test.local".to_string()],
                fingerprint: format!("sha256:{}", "0".repeat(64)),
                not_after: 1,
            }],
        };
        assert!(store.apply(&snapshot).is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
