#![forbid(unsafe_code)]
//! 客户端 TLS 1.3 拨号，支持 CA / 系统根与 SPKI pin。

use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use meatus_core::{ClientConfig, parse_spki_pin};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{
    ClientConfig as RustlsClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn connect(config: &ClientConfig) -> anyhow::Result<TlsStream<TcpStream>> {
    let mut roots = RootCertStore::empty();
    if let Some(ca) = &config.client.server_ca_cert {
        let file =
            File::open(ca).with_context(|| format!("open server_ca_cert {}", ca.display()))?;
        let certs = rustls_pemfile::certs(&mut BufReader::new(file))
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("parse server_ca_cert {}", ca.display()))?;
        if certs.is_empty() {
            bail!("no certificate found in {}", ca.display());
        }
        for cert in certs {
            roots.add(cert).context("add server CA certificate")?;
        }
    } else {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }

    let name = config
        .client
        .server_name
        .clone()
        .unwrap_or_else(|| config.server_host().to_string());
    let server_name = ServerName::try_from(name.as_str())
        .map_err(|err| anyhow!("invalid server_name {name}: {err}"))?
        .to_owned();

    let roots = Arc::new(roots);
    let builder = RustlsClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13]);
    let tls = if let Some(raw_pin) = &config.client.server_public_key_pin {
        let expected = parse_spki_pin(raw_pin)
            .map_err(|err| anyhow!("invalid server_public_key_pin: {err}"))?;
        let inner = WebPkiServerVerifier::builder(Arc::clone(&roots))
            .build()
            .context("build certificate verifier")?;
        let verifier = Arc::new(PinnedVerifier { inner, expected });
        builder
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth()
    } else {
        builder.with_root_certificates(roots).with_no_client_auth()
    };

    let tcp = timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect(&config.client.server_addr),
    )
    .await
    .with_context(|| format!("connect {} timed out", config.client.server_addr))?
    .with_context(|| format!("connect {}", config.client.server_addr))?;
    tcp.set_nodelay(true).context("set TCP_NODELAY")?;

    TlsConnector::from(Arc::new(tls))
        .connect(server_name, tcp)
        .await
        .context("TLS handshake")
}

/// 先走正常链校验，再固定叶子证书 SPKI 的 SHA-256。
#[derive(Debug)]
struct PinnedVerifier {
    inner: Arc<WebPkiServerVerifier>,
    expected: [u8; 32],
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let (_, certificate) = x509_parser::parse_x509_certificate(end_entity.as_ref())
            .map_err(|_| rustls::Error::General("cannot parse server certificate".to_string()))?;
        let digest: [u8; 32] = Sha256::digest(certificate.tbs_certificate.subject_pki.raw).into();
        if digest.ct_eq(&self.expected).unwrap_u8() != 1 {
            return Err(rustls::Error::General(
                "server certificate SPKI pin mismatch".to_string(),
            ));
        }

        self.inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}
