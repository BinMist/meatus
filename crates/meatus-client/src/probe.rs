#![forbid(unsafe_code)]
//! 客户端本地服务探测：只验证 proxy 指向的 local_addr 是否可达。

use std::io;
use std::time::{Duration, Instant};

use meatus_core::{ProxySpec, ProxyType};
use meatus_proto::v1::ProbeResult;
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::timeout;

const MAX_TIMEOUT_MS: u32 = 5_000;

pub(crate) async fn run(
    request_id: u64,
    proxy_name: &str,
    spec: Option<&ProxySpec>,
    timeout_ms: u32,
) -> ProbeResult {
    let Some(spec) = spec else {
        return result(
            request_id,
            proxy_name,
            false,
            0,
            "not_registered",
            "proxy 不在当前客户端运行表中",
        );
    };

    if spec.kind == ProxyType::Socks5 {
        return result(
            request_id,
            proxy_name,
            false,
            0,
            "requires_request",
            "SOCKS5 需实际请求验证",
        );
    }

    let budget = Duration::from_millis(u64::from(timeout_ms.clamp(500, MAX_TIMEOUT_MS)));
    let started = Instant::now();
    let outcome = match spec.kind {
        ProxyType::Tcp | ProxyType::Http | ProxyType::Https => {
            probe_tcp(&spec.local_addr, budget).await
        }
        ProxyType::Udp => probe_udp(&spec.local_addr, budget).await,
        ProxyType::Socks5 => unreachable!("SOCKS5 handled above"),
    };
    let latency = elapsed_ms(started);

    match outcome {
        Ok(()) => result(request_id, proxy_name, true, latency, "", "本地服务可达"),
        Err(error) => {
            let (code, message) = describe(&error);
            result(request_id, proxy_name, false, latency, code, &message)
        }
    }
}

async fn probe_tcp(local_addr: &str, budget: Duration) -> io::Result<()> {
    let stream = timeout(budget, TcpStream::connect(local_addr))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "连接超时"))??;
    drop(stream);
    Ok(())
}

async fn probe_udp(local_addr: &str, budget: Duration) -> io::Result<()> {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    timeout(budget, socket.connect(local_addr))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "UDP connect 超时"))??;
    Ok(())
}

fn describe(error: &io::Error) -> (&'static str, String) {
    let code = match error.kind() {
        io::ErrorKind::ConnectionRefused => "connection_refused",
        io::ErrorKind::TimedOut => "timeout",
        io::ErrorKind::NotFound => "dns_not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        _ => "io_error",
    };
    (code, error.to_string())
}

fn result(
    request_id: u64,
    proxy_name: &str,
    ok: bool,
    latency_ms: u32,
    error_code: &str,
    message: &str,
) -> ProbeResult {
    ProbeResult {
        request_id,
        proxy_name: proxy_name.to_string(),
        ok,
        latency_ms,
        error_code: error_code.to_string(),
        message: message.to_string(),
    }
}

fn elapsed_ms(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use meatus_core::ProxyOptions;
    use tokio::net::TcpListener;

    fn spec(kind: ProxyType, local_addr: String) -> ProxySpec {
        ProxySpec {
            name: "local".to_string(),
            kind,
            local_addr,
            remote_port: None,
            custom_domains: Vec::new(),
            options: ProxyOptions::default(),
        }
    }

    #[tokio::test]
    async fn tcp_probe_reports_reachable_service() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let target = spec(ProxyType::Tcp, addr);
        let out = run(7, "local", Some(&target), 3_000).await;
        assert!(out.ok);
        assert_eq!(out.request_id, 7);
        assert_eq!(out.error_code, "");
    }

    #[tokio::test]
    async fn tcp_probe_reports_refused_service() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        let target = spec(ProxyType::Tcp, addr.to_string());
        let out = run(8, "local", Some(&target), 1_000).await;
        assert!(!out.ok);
        assert_eq!(out.error_code, "connection_refused");
    }

    #[tokio::test]
    async fn socks5_probe_requires_real_request() {
        let target = spec(ProxyType::Socks5, "127.0.0.1:1080".to_string());
        let out = run(9, "socks", Some(&target), 1_000).await;
        assert!(!out.ok);
        assert_eq!(out.error_code, "requires_request");
    }
}
