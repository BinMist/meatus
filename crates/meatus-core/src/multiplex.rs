#![forbid(unsafe_code)]
//! yamux 多路复用驱动。
//!
//! yamux 的 `Connection` 是单 owner 模型：必须持续轮询才能推进 I/O，同时打开
//! 新流也要 `&mut`。这里用一个事件循环统一驱动，向调用方暴露可克隆的
//! [`OpenHandle`] 和只读的 [`Inbound`]。

use std::future::poll_fn;
use std::task::Poll;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt};

pub type MultiplexedStream = Compat<yamux::Stream>;

#[derive(Debug, thiserror::Error)]
pub enum MultiplexError {
    #[error("multiplexed connection closed")]
    Closed,
    #[error("yamux error: {0}")]
    Yamux(String),
}

#[derive(Clone)]
pub struct OpenHandle {
    tx: mpsc::Sender<oneshot::Sender<Result<MultiplexedStream, MultiplexError>>>,
}

impl OpenHandle {
    pub async fn open(&self) -> Result<MultiplexedStream, MultiplexError> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(tx).await.map_err(|_| MultiplexError::Closed)?;
        rx.await.map_err(|_| MultiplexError::Closed)?
    }
}

pub struct Inbound {
    rx: mpsc::Receiver<MultiplexedStream>,
    task: JoinHandle<()>,
}

impl Inbound {
    pub async fn recv(&mut self) -> Option<MultiplexedStream> {
        self.rx.recv().await
    }
}

impl Drop for Inbound {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum DriverEvent {
    Inbound(Result<yamux::Stream, yamux::ConnectionError>),
    Open(oneshot::Sender<Result<MultiplexedStream, MultiplexError>>),
    Closed,
}

#[must_use]
pub fn spawn<T>(io: T, mode: yamux::Mode) -> (OpenHandle, Inbound)
where
    T: futures_io::AsyncRead + futures_io::AsyncWrite + Unpin + Send + 'static,
{
    let (open_tx, mut open_rx) =
        mpsc::channel::<oneshot::Sender<Result<MultiplexedStream, MultiplexError>>>(64);
    let (in_tx, in_rx) = mpsc::channel::<MultiplexedStream>(64);

    let task = tokio::spawn(async move {
        let mut connection = yamux::Connection::new(io, yamux::Config::default(), mode);

        loop {
            let event = poll_fn(|cx| {
                if let Poll::Ready(Some(respond)) = open_rx.poll_recv(cx) {
                    return Poll::Ready(DriverEvent::Open(respond));
                }
                match connection.poll_next_inbound(cx) {
                    Poll::Ready(Some(Ok(stream))) => Poll::Ready(DriverEvent::Inbound(Ok(stream))),
                    Poll::Ready(Some(Err(err))) => Poll::Ready(DriverEvent::Inbound(Err(err))),
                    Poll::Ready(None) => Poll::Ready(DriverEvent::Closed),
                    Poll::Pending => Poll::Pending,
                }
            })
            .await;

            match event {
                DriverEvent::Inbound(Ok(stream)) => {
                    if in_tx.send(stream.compat()).await.is_err() {
                        break;
                    }
                }
                DriverEvent::Inbound(Err(err)) => {
                    tracing::warn!(error = %err, "yamux inbound stream failed");
                    break;
                }
                DriverEvent::Open(respond) => {
                    let result = poll_fn(|cx| connection.poll_new_outbound(cx)).await;
                    let mapped = result
                        .map(|stream| stream.compat())
                        .map_err(|err| MultiplexError::Yamux(err.to_string()));
                    let _ = respond.send(mapped);
                }
                DriverEvent::Closed => break,
            }
        }
    });

    (OpenHandle { tx: open_tx }, Inbound { rx: in_rx, task })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::{Duration, timeout};
    use tokio_util::compat::TokioAsyncReadCompatExt;

    #[tokio::test]
    async fn streams_cross_the_connection() {
        let (a, b) = tokio::io::duplex(1024);
        let (server_open, _server_inbound) = spawn(a.compat(), yamux::Mode::Client);
        let (_client_open, mut client_inbound) = spawn(b.compat(), yamux::Mode::Server);

        let mut outbound = timeout(Duration::from_secs(5), server_open.open())
            .await
            .expect("open timeout")
            .expect("open stream");

        // yamux 的 SYN 附带在首个数据帧上：先写再等对端 accept。
        outbound.write_all(b"ping").await.expect("write");

        let mut inbound = timeout(Duration::from_secs(5), client_inbound.recv())
            .await
            .expect("accept timeout")
            .expect("inbound stream");
        let mut buf = [0u8; 4];
        inbound.read_exact(&mut buf).await.expect("read");
        assert_eq!(&buf, b"ping");

        inbound.write_all(b"pong").await.expect("write back");
        let mut back = [0u8; 4];
        outbound.read_exact(&mut back).await.expect("read back");
        assert_eq!(&back, b"pong");
    }
}
