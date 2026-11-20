//! QUIC 端点构造：复用 rustls 0.23 的证书与配置。

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

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
    let mut config = config;
    config.transport_config(server_transport());
    Endpoint::server(config, bind)
}

/// QUIC 资源上限：单连接只允许一条双向流（应用层再用 yamux 复用），
/// 禁止单向流，30 秒握手 / 空闲超时，15 秒 keep-alive。
/// 服务端与客户端共用，窗口与 BBR 调整同时作用于两侧，避免单向瓶颈。
#[must_use]
pub fn server_transport() -> std::sync::Arc<quinn::TransportConfig> {
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(1u32.into());
    transport.max_concurrent_uni_streams(0u32.into());
    transport.max_idle_timeout(Some(
        std::time::Duration::from_secs(30)
            .try_into()
            .expect("valid idle timeout"),
    ));
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(15)));
    // 本地与跨机房大窗口：避免默认 1.25 MiB 流窗口成为吞吐瓶颈。
    transport.stream_receive_window((8u32 * 1024 * 1024).into());
    transport.receive_window((16u32 * 1024 * 1024).into());
    transport.send_window(16 * 1024 * 1024);
    transport.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
    std::sync::Arc::new(transport)
}

/// 建立客户端端点；绑定 `0.0.0.0:0`。
pub fn client_endpoint() -> io::Result<Endpoint> {
    Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 0)))
}

/// 一条 QUIC 双向流，实现 tokio 的 `AsyncRead` / `AsyncWrite`。
pub struct QuicStream {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
}

impl QuicStream {
    #[must_use]
    pub fn new(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self { send, recv }
    }
}

impl tokio::io::AsyncRead for QuicStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.recv
            .poll_read_buf(cx, buf)
            .map(|result| result.map_err(|err| std::io::Error::other(err.to_string())))
    }
}

impl tokio::io::AsyncWrite for QuicStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.send)
            .poll_write(cx, buf)
            .map(|result| result.map_err(|err| std::io::Error::other(err.to_string())))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let _ = self.send.finish();
        std::task::Poll::Ready(Ok(()))
    }
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
