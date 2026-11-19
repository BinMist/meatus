#![forbid(unsafe_code)]
//! HTTP Host 路由与 HTTPS SNI 透传入口。
//!
//! 两个入口都只解析握手所需的最少字节，然后按原始字节透传；请求体、
//! chunked、gzip 与 WebSocket 升级因此天然保持流式。

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, SystemTime};

use anyhow::{Context, bail};
use meatus_core::{UsageRecord, copy_bidirectional_limited, write_frame};
use meatus_proto::v1::{StreamHeader, StreamKind};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::ServerState;
use crate::routes::{DomainRoute, RouteKind};

const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HTTP_HEAD: usize = 8 * 1024;
const MAX_TLS_RECORD: usize = 64 * 1024;

pub async fn serve_http(listener: TcpListener, state: Arc<ServerState>, cancel: CancellationToken) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let (socket, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(err) => {
                        warn!(error = %err, "http accept failed");
                        continue;
                    }
                };
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Err(err) = handle_http(socket, peer, state).await {
                        debug!(%peer, error = %err, "http connection ended");
                    }
                });
            }
        }
    }
    debug!("http listener stopped");
}

pub async fn serve_https(
    listener: TcpListener,
    state: Arc<ServerState>,
    cancel: CancellationToken,
) {
    let tls_acceptor = state
        .acme
        .as_ref()
        .map(|acme| TlsAcceptor::from(acme.tls_config()));
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let (socket, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(err) => {
                        warn!(error = %err, "https accept failed");
                        continue;
                    }
                };
                let state = Arc::clone(&state);
                let tls_acceptor = tls_acceptor.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_https(socket, peer, state, tls_acceptor).await {
                        debug!(%peer, error = format!("{err:#}"), "https connection ended");
                    }
                });
            }
        }
    }
    debug!("https listener stopped");
}

async fn handle_http(
    mut client: TcpStream,
    peer: SocketAddr,
    state: Arc<ServerState>,
) -> anyhow::Result<()> {
    let (head, host) = match read_http_head(&mut client).await {
        Ok(value) => value,
        Err(err) => {
            let _ = write_http_error(&mut client, 400, "Bad Request").await;
            return Err(err);
        }
    };
    if let Some(acme) = &state.acme
        && let Some(token) = acme_challenge_token(&head)
        && let Some(key_auth) = acme.http01_key_auth(token)
    {
        write_http_text(&mut client, 200, "OK", &key_auth).await?;
        debug!(%peer, %host, %token, "acme http-01 challenge served");
        return Ok(());
    }
    let Some(route) = state.domains.resolve(&host, RouteKind::Http) else {
        if state.domains.is_offline(&host, RouteKind::Http) {
            write_http_error(&mut client, 503, "Service Unavailable").await?;
            debug!(%peer, %host, "http route offline");
        } else {
            write_http_error(&mut client, 404, "Not Found").await?;
            debug!(%peer, %host, "http route not found");
        }
        return Ok(());
    };
    let permit = match Arc::clone(&route.connections).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            write_http_error(&mut client, 503, "Service Unavailable").await?;
            return Ok(());
        }
    };
    pipe_route(client, peer, route, state, head, &host, permit).await
}

async fn handle_https(
    mut client: TcpStream,
    peer: SocketAddr,
    state: Arc<ServerState>,
    tls_acceptor: Option<TlsAcceptor>,
) -> anyhow::Result<()> {
    let (hello, host) = read_client_hello(&mut client).await?;
    let terminate = state
        .acme
        .as_ref()
        .is_some_and(|acme| acme.has_domain(&host))
        && (state.domains.resolve(&host, RouteKind::Http).is_some()
            || state.domains.is_offline(&host, RouteKind::Http));
    if terminate {
        let acceptor = tls_acceptor.context("acme enabled without tls acceptor")?;
        let stream = PrefixedStream::new(client, hello);
        let mut tls = timeout(OPEN_TIMEOUT, acceptor.accept(stream))
            .await
            .context("acme tls handshake timed out")?
            .context("acme tls handshake")?;
        debug!(%peer, %host, "https terminated for acme domain");
        return handle_terminated_http(&mut tls, peer, state).await;
    }

    let Some(route) = state.domains.resolve(&host, RouteKind::Https) else {
        debug!(%peer, %host, "https sni route not found");
        return Ok(());
    };
    let permit = match Arc::clone(&route.connections).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            debug!(%peer, %host, "https route max_conns reached");
            return Ok(());
        }
    };
    pipe_route(client, peer, route, state, hello, &host, permit).await
}

/// ACME 终止 TLS 后，把明文 HTTP 交给 `type = "http"` 的本地服务。
async fn handle_terminated_http<S>(
    client: &mut S,
    peer: SocketAddr,
    state: Arc<ServerState>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (head, host) = match read_http_head(client).await {
        Ok(value) => value,
        Err(err) => {
            let _ = write_http_error(client, 400, "Bad Request").await;
            return Err(err);
        }
    };
    let Some(route) = state.domains.resolve(&host, RouteKind::Http) else {
        if state.domains.is_offline(&host, RouteKind::Http) {
            write_http_error(client, 503, "Service Unavailable").await?;
            debug!(%peer, %host, "https terminated route offline");
        } else {
            write_http_error(client, 404, "Not Found").await?;
            debug!(%peer, %host, "https terminated route not found");
        }
        return Ok(());
    };
    let permit = match Arc::clone(&route.connections).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            write_http_error(client, 503, "Service Unavailable").await?;
            return Ok(());
        }
    };
    pipe_route(client, peer, route, state, head, &host, permit).await
}

async fn pipe_route<S>(
    mut client: S,
    peer: SocketAddr,
    route: Arc<DomainRoute>,
    state: Arc<ServerState>,
    prefix: Vec<u8>,
    dst_hint: &str,
    _permit: tokio::sync::OwnedSemaphorePermit,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    route.metrics.connection_started();
    let result = pipe_route_inner(&mut client, peer, &route, prefix, dst_hint).await;
    route.metrics.connection_finished();
    if let Ok((bytes_in, bytes_out)) = result {
        route.metrics.add_bytes(bytes_in, bytes_out);
        state.usage.record(&UsageRecord {
            tenant_id: route.tenant_id.clone(),
            client_id: route.client_id.clone(),
            proxy_name: route.proxy_name.clone(),
            bytes_in,
            bytes_out,
            connections: 1,
            at: SystemTime::now(),
        });
    }
    result.map(|_| ())
}

async fn pipe_route_inner<S>(
    client: &mut S,
    peer: SocketAddr,
    route: &Arc<DomainRoute>,
    prefix: Vec<u8>,
    dst_hint: &str,
) -> anyhow::Result<(u64, u64)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut tunnel = timeout(OPEN_TIMEOUT, route.open.open())
        .await
        .context("timed out opening http data stream")?
        .context("open http data stream")?;

    let header = StreamHeader {
        proxy_name: route.proxy_name.clone(),
        kind: StreamKind::HttpReq as i32,
        src_addr: peer.to_string(),
        dst_hint: dst_hint.to_string(),
        created_ms: now_ms(),
        trace_id: format!("{:016x}", route.next_trace.fetch_add(1, Ordering::Relaxed)),
        udp_session_id: 0,
    };
    write_frame(&mut tunnel, &header).await?;
    tunnel.write_all(&prefix).await?;
    copy_bidirectional_limited(client, &mut tunnel, route.limiter.as_ref())
        .await
        .context("http bidirectional copy")
}

async fn read_http_head<S>(client: &mut S) -> anyhow::Result<(Vec<u8>, String)>
where
    S: AsyncRead + Unpin,
{
    let mut head = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];
    loop {
        let read = client
            .read(&mut chunk)
            .await
            .context("read http request head")?;
        if read == 0 {
            bail!("connection closed before http headers completed");
        }
        head.extend_from_slice(&chunk[..read]);
        if let Some(end) = head.windows(4).position(|window| window == b"\r\n\r\n") {
            let header_len = end + 4;
            if header_len > MAX_HTTP_HEAD {
                bail!("http request head exceeds {MAX_HTTP_HEAD} bytes");
            }
            let host = parse_host(&head[..header_len])?;
            return Ok((head, host));
        }
        if head.len() > MAX_HTTP_HEAD {
            bail!("http request head exceeds {MAX_HTTP_HEAD} bytes");
        }
    }
}

fn parse_host(head: &[u8]) -> anyhow::Result<String> {
    let text = std::str::from_utf8(head).context("http head is not utf-8")?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    if method.is_empty() || path.is_empty() || !version.starts_with("HTTP/") {
        bail!("invalid http request line");
    }
    for line in lines {
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("host") {
            let host = value.trim().to_ascii_lowercase();
            if host.is_empty() {
                bail!("empty Host header");
            }
            return Ok(host);
        }
    }
    bail!("missing Host header")
}

async fn write_http_error<S>(client: &mut S, code: u16, reason: &str) -> anyhow::Result<()>
where
    S: AsyncWrite + Unpin,
{
    write_http_text(client, code, reason, reason).await
}

async fn write_http_text<S>(
    client: &mut S,
    code: u16,
    reason: &str,
    body: &str,
) -> anyhow::Result<()>
where
    S: AsyncWrite + Unpin,
{
    let response = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    client.write_all(response.as_bytes()).await?;
    client.shutdown().await?;
    Ok(())
}

fn acme_challenge_token(head: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(head).ok()?;
    let path = text.lines().next()?.split_whitespace().nth(1)?;
    let token = path.strip_prefix("/.well-known/acme-challenge/")?;
    if token.is_empty() || token.contains('/') || token.contains('?') {
        return None;
    }
    Some(token)
}

/// 把已读取的 ClientHello 字节回放进 TLS 握手，剩余字节走原始 socket。
pub(crate) struct PrefixedStream {
    prefix: Vec<u8>,
    offset: usize,
    inner: TcpStream,
}

impl PrefixedStream {
    pub(crate) fn new(inner: TcpStream, prefix: Vec<u8>) -> Self {
        Self {
            prefix,
            offset: 0,
            inner,
        }
    }
}

impl AsyncRead for PrefixedStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.offset < this.prefix.len() {
            let remaining = &this.prefix[this.offset..];
            let len = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..len]);
            this.offset += len;
            if len > 0 {
                return Poll::Ready(Ok(()));
            }
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for PrefixedStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

pub(crate) async fn read_client_hello(client: &mut TcpStream) -> anyhow::Result<(Vec<u8>, String)> {
    let mut header = [0u8; 5];
    client
        .read_exact(&mut header)
        .await
        .context("read tls record header")?;
    if header[0] != 0x16 || header[1] != 0x03 {
        bail!("not a TLS handshake record");
    }
    let len = usize::from(u16::from_be_bytes([header[3], header[4]]));
    if len == 0 || len > MAX_TLS_RECORD {
        bail!("invalid TLS record length {len}");
    }
    let mut record = header.to_vec();
    record.resize(5 + len, 0);
    client
        .read_exact(&mut record[5..])
        .await
        .context("read tls client hello")?;
    let host = parse_client_hello_sni(&record).context("parse SNI")?;
    Ok((record, host))
}

fn parse_client_hello_sni(record: &[u8]) -> anyhow::Result<String> {
    if record.len() < 9 || record[0] != 0x16 {
        bail!("short TLS handshake record");
    }
    if record[5] != 0x01 {
        bail!("first TLS handshake message is not ClientHello");
    }
    let hello_len = u32::from_be_bytes([0, record[6], record[7], record[8]]) as usize;
    let hello = record
        .get(9..9 + hello_len)
        .context("truncated ClientHello")?;

    let mut cursor = Cursor::new(hello);
    cursor.take(2).context("ClientHello version")?;
    cursor.take(32).context("ClientHello random")?;
    let session_len = usize::from(cursor.u8().context("session id length")?);
    cursor.take(session_len).context("session id")?;
    let cipher_len = usize::from(cursor.u16().context("cipher length")?);
    cursor.take(cipher_len).context("cipher suites")?;
    let compression_len = usize::from(cursor.u8().context("compression length")?);
    cursor.take(compression_len).context("compression")?;
    if cursor.remaining() == 0 {
        bail!("ClientHello has no extensions");
    }
    let extensions_len = usize::from(cursor.u16().context("extensions length")?);
    let extensions = cursor.take(extensions_len).context("extensions")?;
    let mut extensions = Cursor::new(extensions);

    while extensions.remaining() >= 4 {
        let kind = extensions.u16().context("extension type")?;
        let len = usize::from(extensions.u16().context("extension length")?);
        let data = extensions.take(len).context("extension data")?;
        if kind == 0x0000 {
            return parse_sni(data);
        }
    }
    bail!("ClientHello has no SNI extension")
}

fn parse_sni(data: &[u8]) -> anyhow::Result<String> {
    let mut cursor = Cursor::new(data);
    let list_len = usize::from(cursor.u16().context("server name list length")?);
    let mut list = Cursor::new(cursor.take(list_len).context("server name list")?);
    while list.remaining() >= 3 {
        let name_type = list.u8().context("server name type")?;
        let len = usize::from(list.u16().context("server name length")?);
        let name = list.take(len).context("server name")?;
        if name_type == 0 {
            let host = std::str::from_utf8(name)
                .context("SNI is not utf-8")?
                .trim()
                .to_ascii_lowercase();
            if host.is_empty() || host.contains('/') || host.contains(char::is_whitespace) {
                bail!("invalid SNI hostname");
            }
            return Ok(host);
        }
    }
    bail!("SNI list has no host_name entry")
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn take(&mut self, len: usize) -> anyhow::Result<&'a [u8]> {
        let end = self.pos.checked_add(len).context("length overflow")?;
        let value = self.data.get(self.pos..end).context("truncated input")?;
        self.pos = end;
        Ok(value)
    }

    fn u8(&mut self) -> anyhow::Result<u8> {
        let value = *self.take(1)?.first().context("empty u8")?;
        Ok(value)
    }

    fn u16(&mut self) -> anyhow::Result<u16> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}
