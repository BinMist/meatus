#![forbid(unsafe_code)]
//! 进程内指标注册表与 Prometheus 文本输出。

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use meatus_core::PoolStats;

#[derive(Debug, Default)]
pub struct ProxyMetrics {
    active: AtomicU64,
    total: AtomicU64,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    local_fail: AtomicU64,
    udp_packets: AtomicU64,
    quota_denied: AtomicU64,
    quota_throttled: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProxySnapshot {
    pub active_conns: u64,
    pub total_conns: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub local_fail: u64,
    pub quota_denied: u64,
    pub quota_throttled: u64,
}

impl ProxyMetrics {
    pub fn connection_started(&self) {
        self.active.fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn connection_finished(&self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn add_bytes(&self, inbound: u64, outbound: u64) {
        self.bytes_in.fetch_add(inbound, Ordering::Relaxed);
        self.bytes_out.fetch_add(outbound, Ordering::Relaxed);
    }

    pub fn add_packet(&self, inbound: u64, outbound: u64) {
        self.udp_packets.fetch_add(1, Ordering::Relaxed);
        self.add_bytes(inbound, outbound);
    }

    pub fn set_local_fail(&self, value: u64) {
        self.local_fail.store(value, Ordering::Relaxed);
    }

    pub fn quota_denied(&self) {
        self.quota_denied.fetch_add(1, Ordering::Relaxed);
    }

    pub fn quota_throttled(&self) {
        self.quota_throttled.fetch_add(1, Ordering::Relaxed);
    }

    fn active(&self) -> u64 {
        self.active.load(Ordering::Relaxed)
    }

    fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    fn bytes_in(&self) -> u64 {
        self.bytes_in.load(Ordering::Relaxed)
    }

    fn bytes_out(&self) -> u64 {
        self.bytes_out.load(Ordering::Relaxed)
    }

    fn local_fail(&self) -> u64 {
        self.local_fail.load(Ordering::Relaxed)
    }

    fn udp_packets(&self) -> u64 {
        self.udp_packets.load(Ordering::Relaxed)
    }

    fn quota_denied_count(&self) -> u64 {
        self.quota_denied.load(Ordering::Relaxed)
    }

    fn quota_throttled_count(&self) -> u64 {
        self.quota_throttled.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn snapshot(&self) -> ProxySnapshot {
        ProxySnapshot {
            active_conns: self.active(),
            total_conns: self.total(),
            bytes_in: self.bytes_in(),
            bytes_out: self.bytes_out(),
            local_fail: self.local_fail(),
            quota_denied: self.quota_denied_count(),
            quota_throttled: self.quota_throttled_count(),
        }
    }
}

#[derive(Debug)]
pub struct Metrics {
    started: Instant,
    connected: AtomicU64,
    transport_tcp: AtomicU64,
    transport_quic: AtomicU64,
    pool_stats: Arc<PoolStats>,
    probe_ok: AtomicU64,
    probe_fail: AtomicU64,
    proxies: Mutex<HashMap<String, Arc<ProxyMetrics>>>,
    handshake_fail: Mutex<HashMap<String, u64>>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            connected: AtomicU64::new(0),
            transport_tcp: AtomicU64::new(0),
            transport_quic: AtomicU64::new(0),
            pool_stats: Arc::new(PoolStats::default()),
            probe_ok: AtomicU64::new(0),
            probe_fail: AtomicU64::new(0),
            proxies: Mutex::new(HashMap::new()),
            handshake_fail: Mutex::new(HashMap::new()),
        }
    }
}

impl Metrics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_pool_stats(pool_stats: Arc<PoolStats>) -> Self {
        Self {
            pool_stats,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn pool_stats(&self) -> Arc<PoolStats> {
        Arc::clone(&self.pool_stats)
    }

    #[must_use]
    pub fn proxy(&self, name: &str) -> Arc<ProxyMetrics> {
        let mut proxies = self
            .proxies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        proxies
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(ProxyMetrics::default()))
            .clone()
    }

    /// 记录一次控制面传输选择；`transport` 只接受 `tcp` / `quic`。
    pub fn transport_connected(&self, transport: &str) {
        match transport {
            "tcp" => self.transport_tcp.fetch_add(1, Ordering::Relaxed),
            "quic" => self.transport_quic.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
    }

    pub fn connected_client(&self, delta: i64) {
        if delta > 0 {
            self.connected.fetch_add(delta as u64, Ordering::Relaxed);
        } else {
            self.connected.fetch_sub((-delta) as u64, Ordering::Relaxed);
        }
    }

    pub fn handshake_failed(&self, reason: &str) {
        let mut failures = self
            .handshake_fail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *failures.entry(reason.to_string()).or_insert(0) += 1;
    }

    pub fn probe_finished(&self, ok: bool) {
        if ok {
            self.probe_ok.fetch_add(1, Ordering::Relaxed);
        } else {
            self.probe_fail.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[must_use]
    pub fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(4096);
        let _ = writeln!(
            out,
            "# HELP meatus_client_connected Active control sessions."
        );
        let _ = writeln!(out, "# TYPE meatus_client_connected gauge");
        let _ = writeln!(
            out,
            "meatus_client_connected {}",
            self.connected.load(Ordering::Relaxed)
        );

        let _ = writeln!(
            out,
            "# HELP meatus_transport_connections_total Control-plane transport connections."
        );
        let _ = writeln!(out, "# TYPE meatus_transport_connections_total counter");
        let _ = writeln!(
            out,
            "meatus_transport_connections_total{{transport=\"tcp\"}} {}",
            self.transport_tcp.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            out,
            "meatus_transport_connections_total{{transport=\"quic\"}} {}",
            self.transport_quic.load(Ordering::Relaxed)
        );

        let pool = self.pool_stats.snapshot();
        let _ = writeln!(
            out,
            "# HELP meatus_pool_connections Pooled data connections."
        );
        let _ = writeln!(out, "# TYPE meatus_pool_connections gauge");
        let _ = writeln!(out, "meatus_pool_connections {}", pool.connections);
        let _ = writeln!(
            out,
            "# HELP meatus_pool_opens_total Streams opened through pools."
        );
        let _ = writeln!(out, "# TYPE meatus_pool_opens_total counter");
        let _ = writeln!(out, "meatus_pool_opens_total {}", pool.opens);
        let _ = writeln!(
            out,
            "# HELP meatus_pool_reuses_total Opens on already-used connections."
        );
        let _ = writeln!(out, "# TYPE meatus_pool_reuses_total counter");
        let _ = writeln!(out, "meatus_pool_reuses_total {}", pool.reuses);
        let _ = writeln!(
            out,
            "# HELP meatus_pool_waits_total Opens that had to wait for capacity."
        );
        let _ = writeln!(out, "# TYPE meatus_pool_waits_total counter");
        let _ = writeln!(out, "meatus_pool_waits_total {}", pool.waits);
        let _ = writeln!(
            out,
            "# HELP meatus_pool_timeouts_total Capacity waits that timed out."
        );
        let _ = writeln!(out, "# TYPE meatus_pool_timeouts_total counter");
        let _ = writeln!(out, "meatus_pool_timeouts_total {}", pool.timeouts);

        let _ = writeln!(out, "# HELP meatus_probe_total Admin proxy probes.");
        let _ = writeln!(out, "# TYPE meatus_probe_total counter");
        let _ = writeln!(
            out,
            "meatus_probe_total{{result=\"ok\"}} {}",
            self.probe_ok.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            out,
            "meatus_probe_total{{result=\"fail\"}} {}",
            self.probe_fail.load(Ordering::Relaxed)
        );

        let proxies = self
            .proxies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let _ = writeln!(out, "# HELP meatus_proxy_active Active data connections.");
        let _ = writeln!(out, "# TYPE meatus_proxy_active gauge");
        for (name, metrics) in &proxies {
            let _ = writeln!(
                out,
                "meatus_proxy_active{{proxy=\"{}\"}} {}",
                escape(name),
                metrics.active()
            );
        }

        let _ = writeln!(out, "# HELP meatus_stream_total Accepted data streams.");
        let _ = writeln!(out, "# TYPE meatus_stream_total counter");
        for (name, metrics) in &proxies {
            let _ = writeln!(
                out,
                "meatus_stream_total{{proxy=\"{}\"}} {}",
                escape(name),
                metrics.total()
            );
        }

        let _ = writeln!(out, "# HELP meatus_bytes_total Tunneled bytes.");
        let _ = writeln!(out, "# TYPE meatus_bytes_total counter");
        for (name, metrics) in &proxies {
            let _ = writeln!(
                out,
                "meatus_bytes_total{{proxy=\"{}\",direction=\"in\"}} {}",
                escape(name),
                metrics.bytes_in()
            );
            let _ = writeln!(
                out,
                "meatus_bytes_total{{proxy=\"{}\",direction=\"out\"}} {}",
                escape(name),
                metrics.bytes_out()
            );
        }

        let _ = writeln!(
            out,
            "# HELP meatus_local_connect_fail_total Client-side local connect failures."
        );
        let _ = writeln!(out, "# TYPE meatus_local_connect_fail_total counter");
        for (name, metrics) in &proxies {
            let _ = writeln!(
                out,
                "meatus_local_connect_fail_total{{proxy=\"{}\"}} {}",
                escape(name),
                metrics.local_fail()
            );
        }

        let _ = writeln!(
            out,
            "# HELP meatus_quota_denied_total Rejected by monthly quota."
        );
        let _ = writeln!(out, "# TYPE meatus_quota_denied_total counter");
        for (name, metrics) in &proxies {
            let _ = writeln!(
                out,
                "meatus_quota_denied_total{{proxy=\"{}\"}} {}",
                escape(name),
                metrics.quota_denied_count()
            );
        }

        let _ = writeln!(
            out,
            "# HELP meatus_quota_throttled_total Throttled by monthly quota."
        );
        let _ = writeln!(out, "# TYPE meatus_quota_throttled_total counter");
        for (name, metrics) in &proxies {
            let _ = writeln!(
                out,
                "meatus_quota_throttled_total{{proxy=\"{}\"}} {}",
                escape(name),
                metrics.quota_throttled_count()
            );
        }

        let _ = writeln!(
            out,
            "# HELP meatus_udp_packet_total UDP datagrams forwarded."
        );
        let _ = writeln!(out, "# TYPE meatus_udp_packet_total counter");
        for (name, metrics) in &proxies {
            let _ = writeln!(
                out,
                "meatus_udp_packet_total{{proxy=\"{}\"}} {}",
                escape(name),
                metrics.udp_packets()
            );
        }

        let failures = self
            .handshake_fail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let _ = writeln!(
            out,
            "# HELP meatus_handshake_fail_total Handshake failures."
        );
        let _ = writeln!(out, "# TYPE meatus_handshake_fail_total counter");
        for (reason, count) in &failures {
            let _ = writeln!(
                out,
                "meatus_handshake_fail_total{{reason=\"{}\"}} {}",
                escape(reason),
                count
            );
        }
        out
    }
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_contains_core_metrics() {
        let metrics = Metrics::new();
        metrics.transport_connected("tcp");
        let proxy = metrics.proxy("echo");
        proxy.connection_started();
        proxy.add_bytes(10, 20);
        metrics.handshake_failed("auth");

        let text = metrics.render();
        assert!(text.contains("meatus_client_connected 0"));
        assert!(text.contains("meatus_proxy_active{proxy=\"echo\"} 1"));
        assert!(text.contains("meatus_bytes_total{proxy=\"echo\",direction=\"in\"} 10"));
        assert!(text.contains("meatus_handshake_fail_total{reason=\"auth\"} 1"));
        assert!(text.contains("meatus_transport_connections_total{transport=\"tcp\"} 1"));
        assert!(text.contains("meatus_transport_connections_total{transport=\"quic\"} 0"));
        assert!(text.contains("meatus_pool_connections 0"));
        assert!(text.contains("meatus_pool_waits_total 0"));
    }

    #[tokio::test]
    async fn pool_metrics_reflect_connection_and_opens() {
        let metrics = Metrics::new();
        let pool = meatus_core::ConnectionPool::with_stats(1, 1, metrics.pool_stats());
        let (a, b) = tokio::io::duplex(1024);
        let connection = meatus_core::transport::attach(
            tokio_util::compat::TokioAsyncReadCompatExt::compat(a),
            meatus_core::Role::Client,
        );
        let _peer = b;
        pool.add(connection.open_handle()).expect("add");
        let (_stream, guard) = pool.open().await.expect("open");
        let text = metrics.render();
        assert!(text.contains("meatus_pool_connections 1"), "{text}");
        assert!(text.contains("meatus_pool_opens_total 1"), "{text}");
        drop(guard);
    }
}
