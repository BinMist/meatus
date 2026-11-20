//! QUIC 端点构造：复用 rustls 0.23 的证书与配置。

use std::io;
use std::net::SocketAddr;

use quinn::{Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// 用现有证书链与私钥建立 QUIC 服务端端点。
pub fn server_endpoint(
    bind: SocketAddr,
    cert: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> io::Result<Endpoint> {
    let config = ServerConfig::with_single_cert(cert, key)
        .map_err(|err| io::Error::other(format!("quic server config: {err}")))?;
    Endpoint::server(config, bind)
}

/// 建立客户端端点；绑定 `0.0.0.0:0`。
pub fn client_endpoint() -> io::Result<Endpoint> {
    Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 0)))
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::Arc;

    use quinn::crypto::rustls::QuicClientConfig;
    use quinn::{ClientConfig, Endpoint, ServerConfig};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    fn self_signed() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let cert_der = cert.cert.der().clone();
        let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
        (cert_der, PrivateKeyDer::Pkcs8(key))
    }

    fn server_config(cert: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> ServerConfig {
        ServerConfig::with_single_cert(vec![cert], key).expect("quic server config")
    }

    fn client_config(cert: CertificateDer<'static>) -> ClientConfig {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).expect("trust cert");
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let tls = QuicClientConfig::try_from(tls).expect("quic client config");
        ClientConfig::new(Arc::new(tls))
    }

    async fn bind_server(config: ServerConfig) -> (quinn::Endpoint, SocketAddr) {
        let endpoint =
            Endpoint::server(config, SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).expect("bind");
        let addr = endpoint.local_addr().expect("local addr");
        (endpoint, addr)
    }

    #[tokio::test]
    async fn quic_handshake_and_stream_roundtrip() {
        let (cert, key) = self_signed();
        let (server, addr) = bind_server(server_config(cert.clone(), key)).await;
        let client = Endpoint::client(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).expect("client");
        let connecting = client
            .connect_with(client_config(cert), addr, "localhost")
            .expect("connect");
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let accepted = tokio::spawn(async move {
            let incoming = server.accept().await.expect("incoming");
            let conn = incoming.await.expect("server handshake");
            let (mut send, mut recv) = conn.accept_bi().await.expect("accept stream");
            let request = recv.read_to_end(64).await.expect("read request");
            assert_eq!(request, b"ping");
            send.write_all(b"pong").await.expect("write response");
            send.finish().expect("finish");
            let _ = done_rx.await;
        });
        let conn = connecting.await.expect("client handshake");
        let (mut send, mut recv) = conn.open_bi().await.expect("open stream");
        send.write_all(b"ping").await.expect("write ping");
        send.finish().expect("finish");
        let response = recv.read_to_end(64).await.expect("read response");
        assert_eq!(response, b"pong");
        let _ = done_tx.send(());
        accepted.await.expect("server task");
        conn.close(0u32.into(), b"done");
    }
}
