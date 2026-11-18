#![forbid(unsafe_code)]
//! 管理面状态快照：连接中的 client、已注册 proxy 与实时流量计数。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::metrics::Metrics;

#[derive(Debug, Clone)]
struct ClientEntry {
    tenant_id: String,
    client_id: String,
    run_id: String,
    connected_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct ProxyEntry {
    pub name: String,
    pub proxy_type: String,
    pub remote_port: u16,
    pub domains: Vec<String>,
    pub tenant_id: String,
    pub client_id: String,
    pub run_id: String,
    pub local_addr: String,
    pub rate_limit_bps: u64,
    pub max_conns: u32,
    pub idle_timeout_s: u32,
    pub allow_cidrs: Vec<String>,
}

#[derive(Debug, Default)]
struct Inner {
    clients: HashMap<String, ClientEntry>,
    proxies: HashMap<String, ProxyEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct StatusRegistry {
    inner: Arc<RwLock<Inner>>,
}

impl StatusRegistry {
    pub fn client_connected(&self, tenant_id: &str, client_id: &str, run_id: &str) {
        let key = format!("{tenant_id}\0{client_id}\0{run_id}");
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clients
            .insert(
                key,
                ClientEntry {
                    tenant_id: tenant_id.to_string(),
                    client_id: client_id.to_string(),
                    run_id: run_id.to_string(),
                    connected_at_ms: now_ms(),
                },
            );
    }

    pub fn client_disconnected(&self, tenant_id: &str, client_id: &str, run_id: &str) {
        let key = format!("{tenant_id}\0{client_id}\0{run_id}");
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clients
            .remove(&key);
    }

    pub fn register_proxy(&self, entry: ProxyEntry) {
        let key = proxy_key(
            &entry.tenant_id,
            &entry.client_id,
            &entry.run_id,
            &entry.name,
        );
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .proxies
            .insert(key, entry);
    }

    pub fn unregister_proxy(&self, tenant_id: &str, client_id: &str, run_id: &str, name: &str) {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .proxies
            .remove(&proxy_key(tenant_id, client_id, run_id, name));
    }

    pub fn render(&self, metrics: &Metrics) -> Value {
        let inner = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let mut clients: Vec<Value> = Vec::new();
        for entry in inner.clients.values() {
            let proxy_count = inner
                .proxies
                .values()
                .filter(|proxy| {
                    proxy.tenant_id == entry.tenant_id
                        && proxy.client_id == entry.client_id
                        && proxy.run_id == entry.run_id
                })
                .count();
            clients.push(json!({
                "tenant_id": entry.tenant_id,
                "client_id": entry.client_id,
                "run_id": entry.run_id,
                "connected_at_ms": entry.connected_at_ms,
                "proxy_count": proxy_count,
            }));
        }
        clients.sort_by(|a, b| {
            a["tenant_id"]
                .as_str()
                .cmp(&b["tenant_id"].as_str())
                .then_with(|| a["client_id"].as_str().cmp(&b["client_id"].as_str()))
        });

        let mut proxies: Vec<Value> = Vec::new();
        for entry in inner.proxies.values() {
            let snapshot = metrics.proxy(&entry.name).snapshot();
            proxies.push(json!({
                "name": entry.name,
                "type": entry.proxy_type,
                "remote_port": entry.remote_port,
                "domains": entry.domains,
                "tenant_id": entry.tenant_id,
                "client_id": entry.client_id,
                "run_id": entry.run_id,
                "local_addr": entry.local_addr,
                "rate_limit_bps": entry.rate_limit_bps,
                "max_conns": entry.max_conns,
                "idle_timeout_s": entry.idle_timeout_s,
                "allow_cidrs": entry.allow_cidrs,
                "active_conns": snapshot.active_conns,
                "conn_total": snapshot.total_conns,
                "bytes_in": snapshot.bytes_in,
                "bytes_out": snapshot.bytes_out,
                "local_connect_fail": snapshot.local_fail,
            }));
        }
        proxies.sort_by(|a, b| {
            a["tenant_id"]
                .as_str()
                .cmp(&b["tenant_id"].as_str())
                .then_with(|| a["client_id"].as_str().cmp(&b["client_id"].as_str()))
                .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
        });

        let totals = json!({
            "clients": clients.len(),
            "proxies": proxies.len(),
            "active_conns": proxies.iter().map(|p| p["active_conns"].as_u64().unwrap_or(0)).sum::<u64>(),
            "bytes_in": proxies.iter().map(|p| p["bytes_in"].as_u64().unwrap_or(0)).sum::<u64>(),
            "bytes_out": proxies.iter().map(|p| p["bytes_out"].as_u64().unwrap_or(0)).sum::<u64>(),
        });

        json!({
            "status": "ok",
            "uptime_s": metrics.uptime_secs(),
            "clients": clients,
            "proxies": proxies,
            "totals": totals,
        })
    }
}

fn proxy_key(tenant_id: &str, client_id: &str, run_id: &str, name: &str) -> String {
    format!("{tenant_id}\0{client_id}\0{run_id}\0{name}")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}
