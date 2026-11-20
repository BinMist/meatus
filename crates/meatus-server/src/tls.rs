#![forbid(unsafe_code)]
//! 服务端 TLS 1.3 终结。

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, bail};
use meatus_core::ServerConfig;
use rustls::ServerConfig as RustlsServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls_pemfile::{certs, private_key};

pub fn load(config: &ServerConfig) -> anyhow::Result<Arc<RustlsServerConfig>> {
    let cert_path = config
        .server
        .tls_cert
        .as_ref()
        .context("server.tls_cert is required in M1")?;
    let key_path = config
        .server
        .tls_key
        .as_ref()
        .context("server.tls_key is required in M1")?;
    load_pair(cert_path, key_path)
}

pub fn load_admin(config: &ServerConfig) -> anyhow::Result<Arc<RustlsServerConfig>> {
    let cert_path = config
        .admin
        .tls
        .cert
        .as_ref()
        .context("admin.tls.cert is required when admin.tls.enabled = true")?;
    let key_path = config
        .admin
        .tls
        .key
        .as_ref()
        .context("admin.tls.key is required when admin.tls.enabled = true")?;
    load_pair(cert_path, key_path)
}

/// 读取同一份 PEM 证书链与私钥，供 QUIC 使用。
pub fn load_der(
    cert_path: &Path,
    key_path: &Path,
) -> anyhow::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_file =
        File::open(cert_path).with_context(|| format!("open tls cert {}", cert_path.display()))?;
    let certs = certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("parse tls cert {}", cert_path.display()))?;
    if certs.is_empty() {
        bail!("no certificate found in {}", cert_path.display());
    }
    let key_file =
        File::open(key_path).with_context(|| format!("open tls key {}", key_path.display()))?;
    let key = private_key(&mut BufReader::new(key_file))
        .with_context(|| format!("parse tls key {}", key_path.display()))?
        .context("no private key found in tls_key")?;
    Ok((certs, key))
}

fn load_pair(cert_path: &Path, key_path: &Path) -> anyhow::Result<Arc<RustlsServerConfig>> {
    let cert_file =
        File::open(cert_path).with_context(|| format!("open tls cert {}", cert_path.display()))?;
    let certs = certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("parse tls cert {}", cert_path.display()))?;
    if certs.is_empty() {
        bail!("no certificate found in {}", cert_path.display());
    }

    let key_file =
        File::open(key_path).with_context(|| format!("open tls key {}", key_path.display()))?;
    let key = private_key(&mut BufReader::new(key_file))
        .with_context(|| format!("parse tls key {}", key_path.display()))?
        .context("no private key found in tls_key")?;

    let tls = RustlsServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("build rustls server config")?;

    Ok(Arc::new(tls))
}
