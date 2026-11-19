#![forbid(unsafe_code)]
//! 管理面状态快照：连接中的 client、已注册 proxy 与实时流量计数。
//!
//! proxy 名称按 `tenant + name` 全局唯一：注册前先 claim，冲突时明确返回当前
//! owner，避免两个 client 同时占用同名 proxy 导致指标与路由归属不清。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::metrics::Metrics;
use crate::quota::MonthlyQuota;

#[derive(Debug, Clone)]
struct ClientEntry {
    tenant_id: String,
    client_id: String,
    run_id: String,
    connected_at_ms: i64,
    last_heartbeat_ms: i64,
    caps: Vec<String>,
}

#[derive(Debug, Clone)]
struct ClaimEntry {
    tenant_id: String,
    client_id: String,
    run_id: String,
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
    pub monthly_bytes: u64,
    pub quota_action: String,
    pub idle_timeout_s: u32,
    pub allow_cidrs: Vec<String>,
    pub allow_domains: Vec<String>,
}

#[derive(Debug, Default)]
struct Inner {
    clients: HashMap<String, ClientEntry>,
    proxies: HashMap<String, ProxyEntry>,
    claims: HashMap<String, ClaimEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct StatusRegistry {
    inner: Arc<RwLock<Inner>>,
}

/// 已成功 claim、尚未提交的 proxy 名称。未提交即 Drop 时自动释放 claim。
#[derive(Debug)]
pub struct ProxyClaim {
    registry: StatusRegistry,
    key: String,
    committed: bool,
}

impl ProxyClaim {
    pub fn commit(mut self, entry: ProxyEntry) {
        let mut inner = self
            .registry
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.proxies.insert(self.key.clone(), entry);
        inner.claims.remove(&self.key);
        self.committed = true;
    }
}

impl Drop for ProxyClaim {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.registry
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .claims
            .remove(&self.key);
    }
}

impl StatusRegistry {
    pub fn client_connected(
        &self,
        tenant_id: &str,
        client_id: &str,
        run_id: &str,
        caps: &[String],
    ) {
        let now = now_ms();
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clients
            .insert(
                client_key(tenant_id, client_id, run_id),
                ClientEntry {
                    tenant_id: tenant_id.to_string(),
                    client_id: client_id.to_string(),
                    run_id: run_id.to_string(),
                    connected_at_ms: now,
                    last_heartbeat_ms: now,
                    caps: caps.to_vec(),
                },
            );
    }

    /// 任意控制帧都算一次心跳；用于面板“最后心跳”和离线判断。
    pub fn client_heartbeat(&self, tenant_id: &str, client_id: &str, run_id: &str) {
        if let Some(entry) = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clients
            .get_mut(&client_key(tenant_id, client_id, run_id))
        {
            entry.last_heartbeat_ms = now_ms();
        }
    }

    pub fn client_disconnected(&self, tenant_id: &str, client_id: &str, run_id: &str) {
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner
            .clients
            .remove(&client_key(tenant_id, client_id, run_id));
        // 会话结束时先摘除归属该 run 的状态；ProxyRuntime 的 Drop 仍负责
        // 释放监听与端口，这里的删除是幂等的。
        inner.proxies.retain(|_, proxy| {
            !(proxy.tenant_id == tenant_id
                && proxy.client_id == client_id
                && proxy.run_id == run_id)
        });
        inner.claims.retain(|_, claim| {
            !(claim.tenant_id == tenant_id
                && claim.client_id == client_id
                && claim.run_id == run_id)
        });
    }

    /// 跨 run 统计某 client 当前持有的 proxy 数，用于 `max_proxies` 校验。
    #[must_use]
    pub fn client_proxy_count(&self, tenant_id: &str, client_id: &str) -> usize {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .proxies
            .values()
            .filter(|proxy| proxy.tenant_id == tenant_id && proxy.client_id == client_id)
            .count()
    }

    /// 全局 claim `tenant + proxy name`。同一名称只允许一个活跃 owner。
    pub fn claim_proxy(
        &self,
        tenant_id: &str,
        client_id: &str,
        run_id: &str,
        name: &str,
    ) -> Result<ProxyClaim, String> {
        let key = proxy_key(tenant_id, name);
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = inner.proxies.get(&key) {
            return Err(owner_conflict(name, &existing.client_id, &existing.run_id));
        }
        if let Some(existing) = inner.claims.get(&key) {
            return Err(owner_conflict(name, &existing.client_id, &existing.run_id));
        }
        inner.claims.insert(
            key.clone(),
            ClaimEntry {
                tenant_id: tenant_id.to_string(),
                client_id: client_id.to_string(),
                run_id: run_id.to_string(),
            },
        );
        drop(inner);
        Ok(ProxyClaim {
            registry: self.clone(),
            key,
            committed: false,
        })
    }

    pub fn unregister_proxy(&self, tenant_id: &str, client_id: &str, run_id: &str, name: &str) {
        let key = proxy_key(tenant_id, name);
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owned = inner.proxies.get(&key).is_some_and(|entry| {
            entry.tenant_id == tenant_id && entry.client_id == client_id && entry.run_id == run_id
        });
        if owned {
            inner.proxies.remove(&key);
        }
        let claimed = inner.claims.get(&key).is_some_and(|entry| {
            entry.tenant_id == tenant_id && entry.client_id == client_id && entry.run_id == run_id
        });
        if claimed {
            inner.claims.remove(&key);
        }
    }

    #[must_use]
    pub fn client_supports(&self, run_id: &str, cap: &str) -> bool {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clients
            .values()
            .find(|entry| entry.run_id == run_id)
            .is_some_and(|entry| entry.caps.iter().any(|value| value == cap))
    }

    #[must_use]
    pub fn proxy_entries(&self, name: &str, run_id: Option<&str>) -> Vec<ProxyEntry> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .proxies
            .values()
            .filter(|entry| entry.name == name && run_id.is_none_or(|id| entry.run_id == id))
            .cloned()
            .collect()
    }

    pub fn render(
        &self,
        metrics: &Metrics,
        quotas: &MonthlyQuota,
        config: &meatus_core::ServerConfig,
    ) -> Value {
        let now = now_ms();
        let inner = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let mut clients: Vec<Value> = Vec::new();
        for entry in inner.clients.values() {
            let owned: Vec<&ProxyEntry> = inner
                .proxies
                .values()
                .filter(|proxy| {
                    proxy.tenant_id == entry.tenant_id
                        && proxy.client_id == entry.client_id
                        && proxy.run_id == entry.run_id
                })
                .collect();
            let mut active_conns = 0u64;
            let mut bytes_in = 0u64;
            let mut bytes_out = 0u64;
            let mut proxy_names = Vec::with_capacity(owned.len());
            for proxy in &owned {
                let snapshot = metrics.proxy(&proxy.name).snapshot();
                active_conns += snapshot.active_conns;
                bytes_in += snapshot.bytes_in;
                bytes_out += snapshot.bytes_out;
                proxy_names.push(proxy.name.clone());
            }
            proxy_names.sort();
            let client_limit = config
                .auth
                .clients
                .iter()
                .find(|client| client.tenant == entry.tenant_id && client.id == entry.client_id)
                .and_then(|client| client.limits.monthly_bytes)
                .unwrap_or(0);
            let client_key = format!("{}\0{}", entry.tenant_id, entry.client_id);
            let client_quota = quotas.snapshot(&client_key, &client_key, client_limit);
            clients.push(json!({
                "tenant_id": entry.tenant_id,
                "client_id": entry.client_id,
                "run_id": entry.run_id,
                "connected_at_ms": entry.connected_at_ms,
                "last_heartbeat_ms": entry.last_heartbeat_ms,
                "heartbeat_age_ms": now.saturating_sub(entry.last_heartbeat_ms),
                "proxy_count": owned.len(),
                "proxies": proxy_names,
                "active_conns": active_conns,
                "bytes_in": bytes_in,
                "bytes_out": bytes_out,
                "quota": {
                    "month": month_label(client_quota.reset_at_ms),
                    "used_bytes": client_quota.used_bytes,
                    "limit_bytes": client_quota.limit_bytes,
                    "reset_at_ms": client_quota.reset_at_ms,
                },
            }));
        }
        clients.sort_by(|a, b| {
            a["tenant_id"]
                .as_str()
                .cmp(&b["tenant_id"].as_str())
                .then_with(|| a["client_id"].as_str().cmp(&b["client_id"].as_str()))
                .then_with(|| a["run_id"].as_str().cmp(&b["run_id"].as_str()))
        });

        let mut proxies: Vec<Value> = Vec::new();
        for entry in inner.proxies.values() {
            let snapshot = metrics.proxy(&entry.name).snapshot();
            let client_key = format!("{}\0{}", entry.tenant_id, entry.client_id);
            let proxy_key = format!("{}\0{}", entry.tenant_id, entry.name);
            let quota = quotas.snapshot(&client_key, &proxy_key, entry.monthly_bytes);
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
                "allow_domains": entry.allow_domains,
                "active_conns": snapshot.active_conns,
                "conn_total": snapshot.total_conns,
                "bytes_in": snapshot.bytes_in,
                "bytes_out": snapshot.bytes_out,
                "local_connect_fail": snapshot.local_fail,
                "quota_denied": snapshot.quota_denied,
                "quota_throttled": snapshot.quota_throttled,
                "quota": {
                    "month": month_label(quota.reset_at_ms),
                    "used_bytes": quota.used_bytes,
                    "limit_bytes": quota.limit_bytes,
                    "reset_at_ms": quota.reset_at_ms,
                    "action": entry.quota_action,
                },
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

fn month_label(_reset_at_ms: i64) -> String {
    // 面板只需要“自然月（UTC）”重置语义，具体月份由前端按 reset_at_ms 展示。
    "UTC".to_string()
}

fn client_key(tenant_id: &str, client_id: &str, run_id: &str) -> String {
    format!("{tenant_id}\0{client_id}\0{run_id}")
}

fn proxy_key(tenant_id: &str, name: &str) -> String {
    format!("{tenant_id}\0{name}")
}

fn owner_conflict(name: &str, client_id: &str, run_id: &str) -> String {
    format!(
        "proxy {name} is already owned by client {client_id} (run {})",
        &run_id[..run_id.len().min(12)]
    )
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> meatus_core::ServerConfig {
        meatus_core::parse_server_config(
            "[server]\nbind_addr = \"127.0.0.1\"\ncontrol_port = 17000\n\n[auth]\ntoken_sha256 = \"2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b\"\n",
        )
        .expect("config")
    }

    fn entry(tenant: &str, client: &str, run: &str, name: &str) -> ProxyEntry {
        ProxyEntry {
            name: name.to_string(),
            proxy_type: "tcp".to_string(),
            remote_port: 0,
            domains: Vec::new(),
            tenant_id: tenant.to_string(),
            client_id: client.to_string(),
            run_id: run.to_string(),
            local_addr: "127.0.0.1:8080".to_string(),
            rate_limit_bps: 0,
            max_conns: 0,
            monthly_bytes: 0,
            quota_action: "reject".to_string(),
            idle_timeout_s: 0,
            allow_cidrs: Vec::new(),
            allow_domains: Vec::new(),
        }
    }

    #[test]
    fn ten_clients_do_not_overwrite_each_other() {
        let status = StatusRegistry::default();
        let metrics = Metrics::new();
        for index in 0..10 {
            let client = format!("client-{index}");
            let run = format!("run-{index}");
            status.client_connected("default", &client, &run, &[]);
            let claim = status
                .claim_proxy("default", &client, &run, &format!("proxy-{index}"))
                .expect("claim");
            claim.commit(entry("default", &client, &run, &format!("proxy-{index}")));
        }
        let quotas = MonthlyQuota::default();
        let config = test_config();
        let snapshot = status.render(&metrics, &quotas, &config);
        assert_eq!(snapshot["totals"]["clients"], 10);
        assert_eq!(snapshot["totals"]["proxies"], 10);
        for client in snapshot["clients"].as_array().expect("clients") {
            assert_eq!(client["proxy_count"], 1);
        }
    }

    #[test]
    fn duplicate_proxy_claim_is_rejected_with_owner() {
        let status = StatusRegistry::default();
        status.client_connected("default", "nas-a", "run-a", &[]);
        status.client_connected("default", "nas-b", "run-b", &[]);
        let claim = status
            .claim_proxy("default", "nas-a", "run-a", "web")
            .expect("first claim");
        claim.commit(entry("default", "nas-a", "run-a", "web"));

        let error = status
            .claim_proxy("default", "nas-b", "run-b", "web")
            .expect_err("must reject duplicate owner");
        assert!(error.contains("nas-a"), "{error}");
        assert!(error.contains("web"), "{error}");

        status.unregister_proxy("default", "nas-a", "run-a", "web");
        assert!(
            status
                .claim_proxy("default", "nas-b", "run-b", "web")
                .is_ok()
        );
    }

    #[test]
    fn uncommitted_claim_is_released_on_drop() {
        let status = StatusRegistry::default();
        let claim = status
            .claim_proxy("default", "nas", "run", "web")
            .expect("claim");
        drop(claim);
        assert!(status.claim_proxy("default", "nas", "run", "web").is_ok());
    }

    #[test]
    fn heartbeat_and_traffic_are_exposed_per_client() {
        let status = StatusRegistry::default();
        let metrics = Metrics::new();
        status.client_connected("default", "nas", "run", &[]);
        let claim = status
            .claim_proxy("default", "nas", "run", "web")
            .expect("claim");
        claim.commit(entry("default", "nas", "run", "web"));
        metrics.proxy("web").add_bytes(120, 80);
        std::thread::sleep(std::time::Duration::from_millis(20));
        status.client_heartbeat("default", "nas", "run");
        let quotas = MonthlyQuota::default();
        let config = test_config();
        let snapshot = status.render(&metrics, &quotas, &config);
        let client = &snapshot["clients"][0];
        assert_eq!(client["bytes_in"], 120);
        assert_eq!(client["bytes_out"], 80);
        assert!(client["last_heartbeat_ms"].as_i64().unwrap() > 0);
        assert!(client["heartbeat_age_ms"].as_i64().unwrap() < 1000);
    }

    #[test]
    fn disconnect_removes_proxies_and_claims_for_run() {
        let status = StatusRegistry::default();
        let metrics = Metrics::new();
        status.client_connected("default", "nas", "run", &[]);
        let claim = status
            .claim_proxy("default", "nas", "run", "web")
            .expect("claim");
        claim.commit(entry("default", "nas", "run", "web"));
        status.client_disconnected("default", "nas", "run");
        let quotas = MonthlyQuota::default();
        let config = test_config();
        let snapshot = status.render(&metrics, &quotas, &config);
        assert_eq!(snapshot["totals"]["clients"], 0);
        assert_eq!(snapshot["totals"]["proxies"], 0);
        assert!(status.claim_proxy("default", "nas", "run-2", "web").is_ok());
    }
}
