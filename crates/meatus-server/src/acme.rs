#![forbid(unsafe_code)]
//! ACME（Let's Encrypt / Pebble）HTTP-01 证书签发与热加载。
//!
//! 每个域名独立一个 `AcmeState`，共享同一份账户 / 证书缓存；HTTPS 入口在
//! ClientHello 后按 SNI 选择域名 resolver，命中后做 TLS 终止；未命中仍走
//! SNI 透传，保证自带证书场景不被破坏。

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use futures_util::StreamExt;
use rustls::ServerConfig as RustlsServerConfig;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls_acme::{AccountCache, AcmeConfig, CertCache, UseChallenge};
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;
use tracing::{info, warn};

const LETS_ENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

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

#[derive(Debug, Default)]
struct MultiAcmeResolver {
    domains: RwLock<HashMap<String, Arc<rustls_acme::ResolvesServerCertAcme>>>,
}

impl MultiAcmeResolver {
    fn insert(&self, domain: String, resolver: Arc<rustls_acme::ResolvesServerCertAcme>) {
        self.domains
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(domain, resolver);
    }

    fn has_domain(&self, domain: &str) -> bool {
        self.domains
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(domain)
    }

    fn remove(&self, domain: &str) {
        self.domains
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
        let resolver = self
            .domains
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&domain)
            .cloned()?;
        resolver.resolve(client_hello)
    }
}

/// 一个进程内的 ACME 管理器：按域名懒创建状态机，共享缓存与 TLS resolver。
pub struct AcmeManager {
    resolver: Arc<MultiAcmeResolver>,
    tls_config: Arc<RustlsServerConfig>,
    directory: String,
    contacts: Vec<String>,
    cache: SecureDirCache,
    tasks: Mutex<HashMap<String, JoinHandle<()>>>,
    domains: Mutex<HashMap<String, Arc<rustls_acme::ResolvesServerCertAcme>>>,
}

impl fmt::Debug for AcmeManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcmeManager")
            .field("directory", &self.directory)
            .field("domains", &self.domains.lock().map(|domains| domains.len()))
            .finish_non_exhaustive()
    }
}

impl AcmeManager {
    pub fn new(config: &meatus_core::ServerConfig) -> Result<Self, io::Error> {
        let section = &config.acme;
        if !config.http.enabled {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "acme requires http.enabled = true for http-01",
            ));
        }
        if section.challenge != meatus_core::AcmeChallenge::Http01 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "acme m3 only supports challenge = http-01",
            ));
        }
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
            tasks: Mutex::new(HashMap::new()),
            domains: Mutex::new(HashMap::new()),
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
    }

    /// 为一个精确域名启动 ACME 状态机；泛域名需要 DNS-01，当前返回 false。
    pub fn ensure_domain(&self, domain: &str) -> bool {
        let domain = normalize_domain(domain);
        if domain.is_empty() || domain.contains('*') {
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
        self.tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(task_domain, task);
        true
    }

    #[must_use]
    pub fn http01_key_auth(&self, token: &str) -> Option<String> {
        self.resolver.http01_key_auth(token)
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
    fn contact_uri_adds_mailto_prefix() {
        assert_eq!(contact_uri("ops@example.com"), "mailto:ops@example.com");
        assert_eq!(
            contact_uri("  mailto:ops@example.com  "),
            "mailto:ops@example.com"
        );
    }
}

#[cfg(test)]
mod cached_cert_tests {
    use super::*;
    use tokio::net::TcpListener;

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
