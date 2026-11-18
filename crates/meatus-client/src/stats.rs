#![forbid(unsafe_code)]
//! 客户端按 proxy 聚合的累计统计，用于每 10s 上报 `Stats`。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
}
