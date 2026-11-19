#![forbid(unsafe_code)]
//! 进程内指标注册表与 Prometheus 文本输出。

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Default)]
pub struct ProxyMetrics {
    active: AtomicU64,
    total: AtomicU64,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    local_fail: AtomicU64,
    udp_packets: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProxySnapshot {
    pub active_conns: u64,
    pub total_conns: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub local_fail: u64,
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

    #[must_use]
    pub fn snapshot(&self) -> ProxySnapshot {
        ProxySnapshot {
            active_conns: self.active(),
            total_conns: self.total(),
            bytes_in: self.bytes_in(),
            bytes_out: self.bytes_out(),
            local_fail: self.local_fail(),
        }
    }
}

#[derive(Debug)]
pub struct Metrics {
    started: Instant,
    connected: AtomicU64,
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
        let proxy = metrics.proxy("echo");
        proxy.connection_started();
        proxy.add_bytes(10, 20);
        metrics.handshake_failed("auth");

        let text = metrics.render();
        assert!(text.contains("meatus_client_connected 0"));
        assert!(text.contains("meatus_proxy_active{proxy=\"echo\"} 1"));
        assert!(text.contains("meatus_bytes_total{proxy=\"echo\",direction=\"in\"} 10"));
        assert!(text.contains("meatus_handshake_fail_total{reason=\"auth\"} 1"));
    }
}
