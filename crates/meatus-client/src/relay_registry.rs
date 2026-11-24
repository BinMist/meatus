#![forbid(unsafe_code)]
//! 客户端影子注册表：按 relay 保存 proxy 的公网端口与成员在线状态。
//!
//! G3-02 只做“注册到所有成员 + 记录映射 + 冲突拒绝”，不做流量接管；
//! 接管由 G3-03 按这张表决定新连接走哪个成员。

use std::collections::BTreeMap;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayMemberStatus {
    Connecting,
    Online,
    Degraded,
    Failed,
}

impl RelayMemberStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Online => "online",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayMemberState {
    pub addr: String,
    pub relay_id: Option<String>,
    pub status: RelayMemberStatus,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayPortConflict {
    pub relay_id: String,
    pub proxy: String,
    pub port: u16,
    pub existing_proxy: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelaySnapshot {
    pub group: String,
    pub members: BTreeMap<String, RelayMemberState>,
    /// proxy -> relay_id -> 公网端口；HTTP/HTTPS 的端口为 0。
    pub ports: BTreeMap<String, BTreeMap<String, u16>>,
    pub conflicts: Vec<RelayPortConflict>,
}

/// 线程安全的影子注册表；控制会话与状态读取共享。
#[derive(Debug, Default)]
pub struct RelayRegistry {
    group: String,
    members: RwLock<BTreeMap<String, RelayMemberState>>,
    bindings: RwLock<BTreeMap<String, BTreeMap<String, u16>>>,
    conflicts: RwLock<Vec<RelayPortConflict>>,
}

impl RelayRegistry {
    #[must_use]
    pub fn new(group: impl Into<String>) -> Self {
        Self {
            group: group.into(),
            ..Self::default()
        }
    }

    fn members_read(&self) -> RwLockReadGuard<'_, BTreeMap<String, RelayMemberState>> {
        self.members
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn members_write(&self) -> RwLockWriteGuard<'_, BTreeMap<String, RelayMemberState>> {
        self.members
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn bindings_read(&self) -> RwLockReadGuard<'_, BTreeMap<String, BTreeMap<String, u16>>> {
        self.bindings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn bindings_write(&self) -> RwLockWriteGuard<'_, BTreeMap<String, BTreeMap<String, u16>>> {
        self.bindings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn conflicts_write(&self) -> RwLockWriteGuard<'_, Vec<RelayPortConflict>> {
        self.conflicts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn group(&self) -> &str {
        &self.group
    }

    pub(crate) fn member_connecting(&self, addr: &str) {
        let mut members = self.members_write();
        members.insert(
            addr.to_string(),
            RelayMemberState {
                addr: addr.to_string(),
                relay_id: None,
                status: RelayMemberStatus::Connecting,
                last_error: None,
            },
        );
    }

    pub(crate) fn member_online(&self, addr: &str, relay_id: &str) {
        let mut members = self.members_write();
        let member = members
            .entry(addr.to_string())
            .or_insert_with(|| RelayMemberState {
                addr: addr.to_string(),
                relay_id: None,
                status: RelayMemberStatus::Connecting,
                last_error: None,
            });
        member.relay_id = Some(relay_id.to_string());
        if member.status != RelayMemberStatus::Degraded {
            member.status = RelayMemberStatus::Online;
        }
    }

    pub(crate) fn member_degraded(&self, addr: &str, error: impl Into<String>) {
        let mut members = self.members_write();
        if let Some(member) = members.get_mut(addr) {
            member.status = RelayMemberStatus::Degraded;
            member.last_error = Some(error.into());
        }
    }

    /// 会话结束：标记 failed，并清掉该 relay 的端口映射，避免展示过期路由。
    pub(crate) fn member_session_end(&self, addr: &str, error: Option<String>) {
        let relay_id = {
            let mut members = self.members_write();
            let Some(member) = members.get_mut(addr) else {
                return;
            };
            member.status = RelayMemberStatus::Failed;
            member.last_error = error;
            member.relay_id.clone()
        };
        if let Some(relay_id) = relay_id {
            self.clear_relay_bindings(&relay_id);
        }
    }

    /// 记录一个 proxy 在某个 relay 上的公网端口。
    ///
    /// 同一 relay 上两个 proxy 拿到同一个非零端口时拒绝后者并记录冲突；
    /// HTTP/HTTPS 的端口 0 不参与冲突判断。
    pub(crate) fn record_binding(
        &self,
        relay_id: &str,
        proxy: &str,
        port: u16,
    ) -> Result<(), RelayPortConflict> {
        if port != 0 {
            let bindings = self.bindings_read();
            for (existing_proxy, relays) in bindings.iter() {
                if existing_proxy != proxy
                    && relays
                        .get(relay_id)
                        .is_some_and(|existing| *existing == port)
                {
                    let conflict = RelayPortConflict {
                        relay_id: relay_id.to_string(),
                        proxy: proxy.to_string(),
                        port,
                        existing_proxy: existing_proxy.clone(),
                    };
                    drop(bindings);
                    self.note_conflict(conflict.clone());
                    return Err(conflict);
                }
            }
        }
        self.bindings_write()
            .entry(proxy.to_string())
            .or_default()
            .insert(relay_id.to_string(), port);
        Ok(())
    }

    pub(crate) fn remove_binding(&self, relay_id: &str, proxy: &str) {
        let mut bindings = self.bindings_write();
        if let Some(relays) = bindings.get_mut(proxy) {
            relays.remove(relay_id);
            if relays.is_empty() {
                bindings.remove(proxy);
            }
        }
    }

    fn clear_relay_bindings(&self, relay_id: &str) {
        let mut bindings = self.bindings_write();
        bindings.retain(|_, relays| {
            relays.remove(relay_id);
            !relays.is_empty()
        });
    }

    fn note_conflict(&self, conflict: RelayPortConflict) {
        let mut conflicts = self.conflicts_write();
        if !conflicts.contains(&conflict) {
            conflicts.push(conflict);
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> RelaySnapshot {
        RelaySnapshot {
            group: self.group.clone(),
            members: self.members_read().clone(),
            ports: self.bindings_read().clone(),
            conflicts: self.conflicts_write().clone(),
        }
    }

    /// 某个 proxy 在指定 relay 上的端口；用于状态展示与接管选择。
    #[must_use]
    pub fn port_for(&self, proxy: &str, relay_id: &str) -> Option<u16> {
        self.bindings_read()
            .get(proxy)
            .and_then(|relays| relays.get(relay_id).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_ports_per_relay() {
        let registry = RelayRegistry::new("prod");
        registry.member_connecting("127.0.0.1:7001");
        registry.member_online("127.0.0.1:7001", "relay-a");
        registry.record_binding("relay-a", "nas", 20_001).unwrap();
        registry.record_binding("relay-a", "web", 0).unwrap();
        assert_eq!(registry.port_for("nas", "relay-a"), Some(20_001));
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.group, "prod");
        assert_eq!(
            snapshot.members["127.0.0.1:7001"].status,
            RelayMemberStatus::Online
        );
        assert_eq!(snapshot.ports["nas"]["relay-a"], 20_001);
    }

    #[test]
    fn rejects_duplicate_port_on_same_relay() {
        let registry = RelayRegistry::new("prod");
        registry.record_binding("relay-a", "nas", 20_001).unwrap();
        let conflict = registry
            .record_binding("relay-a", "web", 20_001)
            .unwrap_err();
        assert_eq!(conflict.existing_proxy, "nas");
        assert_eq!(registry.port_for("web", "relay-a"), None);
        assert_eq!(registry.snapshot().conflicts.len(), 1);
    }

    #[test]
    fn same_port_on_other_relay_is_allowed() {
        let registry = RelayRegistry::new("prod");
        registry.record_binding("relay-a", "nas", 20_001).unwrap();
        registry.record_binding("relay-b", "nas", 20_001).unwrap();
        assert_eq!(registry.snapshot().ports["nas"].len(), 2);
    }

    #[test]
    fn session_end_clears_relay_bindings() {
        let registry = RelayRegistry::new("prod");
        registry.member_connecting("127.0.0.1:7002");
        registry.member_online("127.0.0.1:7002", "relay-b");
        registry.record_binding("relay-b", "nas", 21_001).unwrap();
        registry.member_session_end("127.0.0.1:7002", Some("io".to_string()));
        assert_eq!(registry.port_for("nas", "relay-b"), None);
        assert_eq!(
            registry.snapshot().members["127.0.0.1:7002"].status,
            RelayMemberStatus::Failed
        );
    }
}
