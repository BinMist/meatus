#![forbid(unsafe_code)]
//! 客户端按 proxy 聚合的累计统计，用于每 10s 上报 `Stats`。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use meatus_core::ProxySpec;
use meatus_proto::v1::Stats;

#[derive(Debug, Default)]
pub struct ProxyCounter {
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    active: AtomicU64,
    total: AtomicU64,
    local_fail: AtomicU64,
}

impl ProxyCounter {
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

    pub fn local_fail(&self) {
        self.local_fail.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self, name: &str) -> Stats {
        Stats {
            proxy_name: name.to_string(),
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
            active_conns: self.active.load(Ordering::Relaxed) as u32,
            connections_total: self.total.load(Ordering::Relaxed),
            local_connect_fail: self.local_fail.load(Ordering::Relaxed),
        }
    }
}

/// 控制面 Ping/Pong 健康度，供日志与 G6-03 公网门禁解析。
#[derive(Debug, Default)]
pub struct ControlHealth {
    pongs: AtomicU64,
    timeouts: AtomicU64,
    consecutive_misses: AtomicU64,
    last_pong_unix_ms: AtomicU64,
    max_gap_ms: AtomicU64,
    /// RTT 直方图：`<1s / 1-5s / 5-20s / 20-60s / >=60s`。
    rtt_buckets: [AtomicU64; 5],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControlHealthSnapshot {
    pub pongs: u64,
    pub timeouts: u64,
    pub consecutive_misses: u64,
    pub last_pong_unix_ms: u64,
    pub max_gap_ms: u64,
    pub rtt_buckets: [u64; 5],
}

const RTT_BUCKET_EDGES_MS: [u64; 5] = [1_000, 5_000, 20_000, 60_000, u64::MAX];

impl ControlHealth {
    /// 记录一次 Pong；`rtt` 是本地发出 Ping 到收到 Pong 的时间。
    pub fn record_pong(&self, rtt: Duration, now_unix_ms: u64) {
        self.pongs.fetch_add(1, Ordering::Relaxed);
        self.consecutive_misses.store(0, Ordering::Relaxed);
        let rtt_ms = u64::try_from(rtt.as_millis()).unwrap_or(u64::MAX);
        let bucket = RTT_BUCKET_EDGES_MS
            .iter()
            .position(|edge| rtt_ms < *edge)
            .unwrap_or(RTT_BUCKET_EDGES_MS.len() - 1);
        self.rtt_buckets[bucket].fetch_add(1, Ordering::Relaxed);

        let previous = self.last_pong_unix_ms.swap(now_unix_ms, Ordering::Relaxed);
        if previous != 0 && now_unix_ms > previous {
            let gap = now_unix_ms - previous;
            self.max_gap_ms.fetch_max(gap, Ordering::Relaxed);
        }
    }

    /// 记录当前连续未收到 Pong 的 Ping 数；每次发 Ping 后刷新，Pong 到达时清零。
    pub fn record_misses(&self, consecutive: u64) {
        self.consecutive_misses
            .store(consecutive, Ordering::Relaxed);
    }

    /// 记录一次控制面 Pong 超时；返回累计超时次数。
    pub fn record_timeout(&self, gap: Duration) -> u64 {
        self.timeouts.fetch_add(1, Ordering::Relaxed);
        let gap_ms = u64::try_from(gap.as_millis()).unwrap_or(u64::MAX);
        self.max_gap_ms.fetch_max(gap_ms, Ordering::Relaxed);
        self.timeouts.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn snapshot(&self) -> ControlHealthSnapshot {
        let mut buckets = [0u64; 5];
        for (slot, counter) in buckets.iter_mut().zip(self.rtt_buckets.iter()) {
            *slot = counter.load(Ordering::Relaxed);
        }
        ControlHealthSnapshot {
            pongs: self.pongs.load(Ordering::Relaxed),
            timeouts: self.timeouts.load(Ordering::Relaxed),
            consecutive_misses: self.consecutive_misses.load(Ordering::Relaxed),
            last_pong_unix_ms: self.last_pong_unix_ms.load(Ordering::Relaxed),
            max_gap_ms: self.max_gap_ms.load(Ordering::Relaxed),
            rtt_buckets: buckets,
        }
    }
}

#[derive(Debug)]
pub struct ClientStats {
    counters: Mutex<HashMap<String, Arc<ProxyCounter>>>,
}

impl ClientStats {
    #[must_use]
    pub fn new(proxies: &[ProxySpec]) -> Self {
        let counters = proxies
            .iter()
            .map(|proxy| (proxy.name.clone(), Arc::new(ProxyCounter::default())))
            .collect();
        Self {
            counters: Mutex::new(counters),
        }
    }

    /// 热重载新增 proxy 时登记计数器；重复登记不会丢历史。
    pub fn register(&self, name: &str) {
        let mut counters = self
            .counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        counters
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(ProxyCounter::default()));
    }

    /// 管理面删除 proxy 时同步移除计数器，避免继续上报已删除条目。
    pub fn unregister(&self, name: &str) {
        self.counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(name);
    }

    #[must_use]
    pub fn counter(&self, name: &str) -> Option<Arc<ProxyCounter>> {
        self.counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .cloned()
    }

    #[must_use]
    pub fn snapshots(&self) -> Vec<Stats> {
        self.counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(name, counter)| counter.snapshot(name))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(name: &str) -> ProxySpec {
        ProxySpec {
            name: name.to_string(),
            kind: meatus_core::ProxyType::Tcp,
            local_addr: "127.0.0.1:22".to_string(),
            remote_port: None,
            custom_domains: Vec::new(),
            options: meatus_core::ProxyOptions::default(),
        }
    }

    #[test]
    fn snapshots_are_cumulative() {
        let stats = ClientStats::new(&[proxy("echo")]);
        let counter = stats.counter("echo").expect("counter");
        counter.connection_started();
        counter.add_bytes(10, 20);
        counter.local_fail();
        counter.connection_finished();

        let snapshot = stats.snapshots();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].connections_total, 1);
        assert_eq!(snapshot[0].bytes_in, 10);
        assert_eq!(snapshot[0].bytes_out, 20);
        assert_eq!(snapshot[0].local_connect_fail, 1);
    }

    #[test]
    fn control_health_tracks_rtt_buckets_and_gaps() {
        let health = ControlHealth::default();
        health.record_pong(Duration::from_millis(250), 1_000);
        health.record_pong(Duration::from_millis(3_500), 11_000);
        health.record_pong(Duration::from_millis(25_000), 31_000);
        health.record_pong(Duration::from_millis(120_000), 151_000);

        let snapshot = health.snapshot();
        assert_eq!(snapshot.pongs, 4);
        assert_eq!(snapshot.rtt_buckets, [1, 1, 0, 1, 1]);
        assert_eq!(snapshot.max_gap_ms, 120_000);
        assert_eq!(snapshot.consecutive_misses, 0);
        assert_eq!(snapshot.last_pong_unix_ms, 151_000);

        health.record_misses(2);
        health.record_timeout(Duration::from_secs(61));
        health.record_timeout(Duration::from_secs(62));
        let snapshot = health.snapshot();
        assert_eq!(snapshot.timeouts, 2);
        assert_eq!(snapshot.consecutive_misses, 2);
        assert_eq!(snapshot.max_gap_ms, 120_000);

        health.record_pong(Duration::from_millis(100), 200_000);
        let snapshot = health.snapshot();
        assert_eq!(snapshot.consecutive_misses, 0);
        assert_eq!(snapshot.rtt_buckets, [2, 1, 0, 1, 1]);
    }

    #[test]
    fn control_health_bucket_edges_are_half_open() {
        let health = ControlHealth::default();
        health.record_pong(Duration::from_millis(1_000), 1);
        health.record_pong(Duration::from_millis(5_000), 2);
        health.record_pong(Duration::from_millis(20_000), 3);
        health.record_pong(Duration::from_millis(60_000), 4);
        assert_eq!(health.snapshot().rtt_buckets, [0, 1, 1, 1, 1]);
    }
}
