#![forbid(unsafe_code)]
//! 进程内指标注册表与 Prometheus 文本输出。

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use meatus_core::PoolStats;
use serde::Serialize;

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
    last_stream: Mutex<Option<(String, String)>>,
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

/// relay 组成员初始状态；peer 地址来自验签后的 bundle。
#[derive(Debug, Clone)]
pub struct RelayPeerSeed {
    pub id: String,
    pub role: String,
    pub peer_addr: Option<String>,
}

#[derive(Debug, Clone)]
struct RelayPeerState {
    seed: RelayPeerSeed,
    up: bool,
    latency_ms: Option<u64>,
    last_checked: Option<Instant>,
    last_ok: Option<Instant>,
    last_error: Option<String>,
    peer_epoch: Option<u64>,
    config_hash: Option<String>,
    drift: Option<String>,
    rotation_eligible: bool,
    fail_streak: u32,
    success_streak: u32,
}

#[derive(Debug, Clone)]
struct RelayState {
    id: String,
    group: String,
    role: String,
    epoch: u64,
    cert_sync: String,
    config_hash: String,
    fail_threshold: u32,
    recover_threshold: u32,
    peers: Vec<RelayPeerState>,
}

/// `/healthz` 与管理面使用的可序列化 relay 快照。
#[derive(Debug, Clone, Serialize)]
pub struct RelaySnapshot {
    pub id: String,
    pub group: String,
    pub role: String,
    pub epoch: u64,
    pub cert_sync: String,
    /// 本节点签名 bundle 的 config hash；peer 必须完全一致。
    pub config_hash: String,
    /// `healthy` 表示所有 peer 可探测且身份/epoch/config hash 一致。
    pub state: &'static str,
    pub peers_up: usize,
    pub peers_total: usize,
    /// 可供外部 LB/DNS 轮转使用的资格快照。
    pub rotation: RelayRotationSnapshot,
    pub peers: Vec<RelayPeerSnapshot>,
}

/// 轮转池状态；`complete` 表示所有 peer 都在池内。
#[derive(Debug, Clone, Serialize)]
pub struct RelayRotationSnapshot {
    pub state: &'static str,
    pub eligible: Vec<String>,
    pub excluded: Vec<RelayPeerExclusion>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelayPeerExclusion {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelayPeerSnapshot {
    pub id: String,
    pub role: String,
    pub peer_addr: Option<String>,
    pub up: bool,
    pub peer_epoch: Option<u64>,
    pub config_hash: Option<String>,
    /// `config_hash` / `epoch` / `identity`；网络故障不是漂移。
    pub drift: Option<String>,
    pub rotation_eligible: bool,
    pub latency_ms: Option<u64>,
    pub last_checked_age_s: Option<u64>,
    pub last_ok_age_s: Option<u64>,
    pub last_error: Option<String>,
}

/// 单次 peer 探测结果；失败时也保留对端 epoch/config hash，便于识别配置漂移。
#[derive(Debug, Clone)]
pub struct PeerProbeOutcome {
    pub up: bool,
    pub latency_ms: u64,
    pub peer_epoch: Option<u64>,
    pub peer_config_hash: Option<String>,
    pub drift: Option<String>,
    pub error: Option<String>,
}

impl PeerProbeOutcome {
    #[must_use]
    pub fn up(latency_ms: u64, peer_epoch: u64) -> Self {
        Self {
            up: true,
            latency_ms,
            peer_epoch: Some(peer_epoch),
            peer_config_hash: None,
            drift: None,
            error: None,
        }
    }

    #[must_use]
    pub fn down(latency_ms: u64, peer_epoch: Option<u64>, error: impl Into<String>) -> Self {
        Self {
            up: false,
            latency_ms,
            peer_epoch,
            peer_config_hash: None,
            drift: None,
            error: Some(error.into()),
        }
    }

    /// 身份、epoch 或 config hash 不一致；不计入普通网络失败，立即摘除。
    #[must_use]
    pub fn drift(
        latency_ms: u64,
        peer_epoch: Option<u64>,
        peer_config_hash: Option<String>,
        kind: &'static str,
        error: impl Into<String>,
    ) -> Self {
        Self {
            up: false,
            latency_ms,
            peer_epoch,
            peer_config_hash,
            drift: Some(kind.to_string()),
            error: Some(error.into()),
        }
    }

    #[must_use]
    pub fn with_config_hash(mut self, hash: impl Into<String>) -> Self {
        self.peer_config_hash = Some(hash.into());
        self
    }
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

    /// 记录最近一条数据流的 ID，供审计/指标关联。
    pub fn record_stream(&self, trace_id: &str, connection_id: &str) {
        let mut last = self
            .last_stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *last = Some((trace_id.to_string(), connection_id.to_string()));
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
    drain_phase: AtomicU64,
    relay: Mutex<Option<RelayState>>,
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
            drain_phase: AtomicU64::new(0),
            relay: Mutex::new(None),
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

    /// 启动时写入本节点 relay 身份、bundle hash 与 peer 列表；standalone 不调用。
    #[allow(clippy::too_many_arguments)]
    pub fn configure_relay(
        &self,
        id: &str,
        group: &str,
        role: &str,
        epoch: u64,
        cert_sync: &str,
        config_hash: &str,
        fail_threshold: u32,
        recover_threshold: u32,
        peers: Vec<RelayPeerSeed>,
    ) {
        let state = RelayState {
            id: id.to_string(),
            group: group.to_string(),
            role: role.to_string(),
            epoch,
            cert_sync: cert_sync.to_string(),
            config_hash: config_hash.to_string(),
            fail_threshold: fail_threshold.max(1),
            recover_threshold: recover_threshold.max(1),
            peers: peers
                .into_iter()
                .map(|seed| RelayPeerState {
                    seed,
                    up: false,
                    latency_ms: None,
                    last_checked: None,
                    last_ok: None,
                    last_error: None,
                    peer_epoch: None,
                    config_hash: None,
                    drift: None,
                    // 未知成员先不进轮转池，首次成功探测后才加入。
                    rotation_eligible: false,
                    fail_streak: 0,
                    success_streak: 0,
                })
                .collect(),
        };
        *self
            .relay
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(state);
    }

    /// 记录一轮 peer 健康探测；未知 peer 忽略，避免 bundle 外注入。
    pub fn record_peer_probe(&self, peer_id: &str, outcome: PeerProbeOutcome) {
        let now = Instant::now();
        let mut relay = self
            .relay
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(relay) = relay.as_mut() else {
            return;
        };
        let (fail_threshold, recover_threshold) = (relay.fail_threshold, relay.recover_threshold);
        let Some(peer) = relay.peers.iter_mut().find(|peer| peer.seed.id == peer_id) else {
            return;
        };
        peer.up = outcome.up;
        peer.latency_ms = Some(outcome.latency_ms);
        peer.last_checked = Some(now);
        peer.peer_epoch = outcome.peer_epoch;
        if outcome.peer_config_hash.is_some() {
            peer.config_hash = outcome.peer_config_hash;
        }
        peer.drift = outcome.drift.clone();
        if outcome.up {
            peer.last_ok = Some(now);
            peer.last_error = None;
            peer.fail_streak = 0;
            peer.success_streak = peer.success_streak.saturating_add(1);
            if peer.success_streak >= recover_threshold {
                peer.rotation_eligible = true;
            }
        } else {
            peer.last_error = outcome.error;
            peer.success_streak = 0;
            peer.fail_streak = peer.fail_streak.saturating_add(1);
            // 配置漂移比网络抖动严重，立即摘除；普通故障达到阈值才摘除。
            if outcome.drift.is_some() || peer.fail_streak >= fail_threshold {
                peer.rotation_eligible = false;
            }
        }
    }

    /// 返回当前 relay 状态；standalone 返回 `None`。
    #[must_use]
    pub fn relay_snapshot(&self) -> Option<RelaySnapshot> {
        let relay = self
            .relay
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let relay = relay.as_ref()?;
        let now = Instant::now();
        let peers: Vec<_> = relay
            .peers
            .iter()
            .map(|peer| RelayPeerSnapshot {
                id: peer.seed.id.clone(),
                role: peer.seed.role.clone(),
                peer_addr: peer.seed.peer_addr.clone(),
                up: peer.up,
                peer_epoch: peer.peer_epoch,
                config_hash: peer.config_hash.clone(),
                drift: peer.drift.clone(),
                rotation_eligible: peer.rotation_eligible,
                latency_ms: peer.latency_ms,
                last_checked_age_s: peer.last_checked.map(|at| now.duration_since(at).as_secs()),
                last_ok_age_s: peer.last_ok.map(|at| now.duration_since(at).as_secs()),
                last_error: peer.last_error.clone(),
            })
            .collect();
        let peers_up = peers.iter().filter(|peer| peer.up).count();
        let state = if peers_up == peers.len() {
            "healthy"
        } else {
            "degraded"
        };
        let eligible: Vec<String> = peers
            .iter()
            .filter(|peer| peer.rotation_eligible)
            .map(|peer| peer.id.clone())
            .collect();
        let excluded: Vec<RelayPeerExclusion> = peers
            .iter()
            .filter(|peer| !peer.rotation_eligible)
            .map(|peer| RelayPeerExclusion {
                id: peer.id.clone(),
                reason: peer.drift.clone().unwrap_or_else(|| {
                    peer.last_error
                        .clone()
                        .unwrap_or_else(|| "probing".to_string())
                }),
            })
            .collect();
        let rotation_state = if excluded.is_empty() {
            "complete"
        } else if eligible.is_empty() {
            "empty"
        } else {
            "partial"
        };
        Some(RelaySnapshot {
            id: relay.id.clone(),
            group: relay.group.clone(),
            role: relay.role.clone(),
            epoch: relay.epoch,
            cert_sync: relay.cert_sync.clone(),
            config_hash: relay.config_hash.clone(),
            state,
            peers_up,
            peers_total: peers.len(),
            rotation: RelayRotationSnapshot {
                state: rotation_state,
                eligible,
                excluded,
            },
            peers,
        })
    }

    pub fn set_drain_phase(&self, phase: u8) {
        self.drain_phase.store(u64::from(phase), Ordering::Relaxed);
    }

    /// 当前所有 proxy 的活动数据流总数；drain 等待条件。
    pub fn data_active_total(&self) -> u64 {
        self.proxies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|metrics| metrics.active())
            .sum()
    }

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
            "# HELP meatus_server_drain 0=serving 1=draining 2=drained."
        );
        let _ = writeln!(out, "# TYPE meatus_server_drain gauge");
        let _ = writeln!(
            out,
            "meatus_server_drain {}",
            self.drain_phase.load(Ordering::Relaxed)
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

        let _ = writeln!(
            out,
            "# HELP meatus_proxy_last_stream IDs of the latest accepted data stream."
        );
        let _ = writeln!(out, "# TYPE meatus_proxy_last_stream gauge");
        for (name, metrics) in &proxies {
            if let Some((trace_id, connection_id)) = metrics
                .last_stream
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                let _ = writeln!(
                    out,
                    "meatus_proxy_last_stream{{proxy=\"{}\",trace_id=\"{}\",connection_id=\"{}\"}} 1",
                    escape(name),
                    escape(&trace_id),
                    escape(&connection_id)
                );
            }
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

        if let Some(relay) = self.relay_snapshot() {
            let _ = writeln!(
                out,
                "# HELP meatus_relay_info Verified relay group membership."
            );
            let _ = writeln!(out, "# TYPE meatus_relay_info gauge");
            let _ = writeln!(
                out,
                "meatus_relay_info{{relay_id=\"{}\",group=\"{}\",role=\"{}\",epoch=\"{}\",cert_sync=\"{}\"}} 1",
                escape(&relay.id),
                escape(&relay.group),
                escape(&relay.role),
                relay.epoch,
                escape(&relay.cert_sync)
            );
            let _ = writeln!(
                out,
                "# HELP meatus_relay_config_hash Signed bundle config hash pinned by this node."
            );
            let _ = writeln!(out, "# TYPE meatus_relay_config_hash gauge");
            let _ = writeln!(
                out,
                "meatus_relay_config_hash{{relay_id=\"{}\",config_hash=\"{}\"}} 1",
                escape(&relay.id),
                escape(&relay.config_hash)
            );
            let _ = writeln!(
                out,
                "# HELP meatus_relay_epoch Current signed bundle epoch."
            );
            let _ = writeln!(out, "# TYPE meatus_relay_epoch gauge");
            let _ = writeln!(
                out,
                "meatus_relay_epoch{{relay_id=\"{}\"}} {}",
                escape(&relay.id),
                relay.epoch
            );
            let _ = writeln!(
                out,
                "# HELP meatus_relay_peer_up Peer reachable with matching id/group/role/epoch/config hash."
            );
            let _ = writeln!(out, "# TYPE meatus_relay_peer_up gauge");
            let _ = writeln!(
                out,
                "# HELP meatus_relay_peer_rotation_eligible Peer currently eligible for edge rotation."
            );
            let _ = writeln!(out, "# TYPE meatus_relay_peer_rotation_eligible gauge");
            for peer in &relay.peers {
                let _ = writeln!(
                    out,
                    "meatus_relay_peer_up{{relay_id=\"{}\",peer=\"{}\",role=\"{}\"}} {}",
                    escape(&relay.id),
                    escape(&peer.id),
                    escape(&peer.role),
                    u8::from(peer.up)
                );
                let _ = writeln!(
                    out,
                    "meatus_relay_peer_rotation_eligible{{relay_id=\"{}\",peer=\"{}\"}} {}",
                    escape(&relay.id),
                    escape(&peer.id),
                    u8::from(peer.rotation_eligible)
                );
                if let Some(kind) = &peer.drift {
                    let _ = writeln!(
                        out,
                        "# HELP meatus_relay_peer_drift Peer drift flag by kind."
                    );
                    let _ = writeln!(out, "# TYPE meatus_relay_peer_drift gauge");
                    let _ = writeln!(
                        out,
                        "meatus_relay_peer_drift{{relay_id=\"{}\",peer=\"{}\",kind=\"{}\"}} 1",
                        escape(&relay.id),
                        escape(&peer.id),
                        escape(kind)
                    );
                }
            }
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

        proxy.record_stream("trace-1", "conn-1");
        let text = metrics.render();
        assert!(text.contains(
            "meatus_proxy_last_stream{proxy=\"echo\",trace_id=\"trace-1\",connection_id=\"conn-1\"} 1"
        ));
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

    #[test]
    fn relay_status_is_degraded_until_every_peer_is_compatible() {
        const HASH: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let metrics = Metrics::new();
        metrics.configure_relay(
            "relay-a",
            "prod",
            "primary",
            3,
            "push",
            HASH,
            2,
            1,
            vec![RelayPeerSeed {
                id: "relay-b".to_string(),
                role: "secondary".to_string(),
                peer_addr: Some("127.0.0.1:7402".to_string()),
            }],
        );
        let snapshot = metrics.relay_snapshot().expect("relay snapshot");
        assert_eq!(snapshot.state, "degraded");
        assert_eq!(snapshot.peers_up, 0);
        assert_eq!(snapshot.peers_total, 1);
        assert!(snapshot.peers[0].last_checked_age_s.is_none());
        assert_eq!(snapshot.rotation.state, "empty");
        assert!(!snapshot.peers[0].rotation_eligible);

        metrics.record_peer_probe(
            "relay-b",
            PeerProbeOutcome::drift(
                4,
                Some(2),
                Some(HASH.to_string()),
                "epoch",
                "epoch mismatch",
            ),
        );
        let snapshot = metrics.relay_snapshot().expect("relay snapshot");
        assert_eq!(snapshot.state, "degraded");
        assert_eq!(snapshot.peers[0].peer_epoch, Some(2));
        assert_eq!(
            snapshot.peers[0].last_error.as_deref(),
            Some("epoch mismatch")
        );
        assert_eq!(snapshot.peers[0].drift.as_deref(), Some("epoch"));
        assert!(!snapshot.peers[0].rotation_eligible);

        metrics.record_peer_probe("relay-b", PeerProbeOutcome::up(4, 3).with_config_hash(HASH));
        let snapshot = metrics.relay_snapshot().expect("relay snapshot");
        assert_eq!(snapshot.state, "healthy");
        assert_eq!(snapshot.peers_up, 1);
        assert_eq!(snapshot.peers[0].last_error, None);
        assert_eq!(snapshot.peers[0].last_ok_age_s, Some(0));
        assert!(snapshot.peers[0].rotation_eligible);
        assert_eq!(snapshot.rotation.state, "complete");
        assert_eq!(snapshot.rotation.eligible, vec!["relay-b".to_string()]);

        let text = metrics.render();
        assert!(
            text.contains(
                "meatus_relay_info{relay_id=\"relay-a\",group=\"prod\",role=\"primary\",epoch=\"3\",cert_sync=\"push\"} 1"
            ),
            "{text}"
        );
        assert!(
            text.contains("meatus_relay_epoch{relay_id=\"relay-a\"} 3"),
            "{text}"
        );
        assert!(
            text.contains(
                "meatus_relay_peer_up{relay_id=\"relay-a\",peer=\"relay-b\",role=\"secondary\"} 1"
            ),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "meatus_relay_config_hash{{relay_id=\"relay-a\",config_hash=\"{HASH}\"}} 1"
            )),
            "{text}"
        );
        assert!(
            text.contains(
                "meatus_relay_peer_rotation_eligible{relay_id=\"relay-a\",peer=\"relay-b\"} 1"
            ),
            "{text}"
        );
        assert!(!text.contains("meatus_relay_peer_drift"), "{text}");
    }

    #[test]
    fn peer_config_hash_drift_evicts_immediately_and_recovers() {
        const HASH: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const OTHER: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let metrics = Metrics::new();
        metrics.configure_relay(
            "relay-a",
            "prod",
            "primary",
            3,
            "push",
            HASH,
            2,
            1,
            vec![RelayPeerSeed {
                id: "relay-b".to_string(),
                role: "secondary".to_string(),
                peer_addr: Some("127.0.0.1:7402".to_string()),
            }],
        );
        metrics.record_peer_probe("relay-b", PeerProbeOutcome::up(3, 3).with_config_hash(HASH));
        metrics.record_peer_probe(
            "relay-b",
            PeerProbeOutcome::drift(
                3,
                Some(3),
                Some(OTHER.to_string()),
                "config_hash",
                "peer config hash mismatch",
            ),
        );
        let snapshot = metrics.relay_snapshot().expect("relay snapshot");
        assert_eq!(snapshot.state, "degraded");
        assert_eq!(snapshot.peers[0].drift.as_deref(), Some("config_hash"));
        assert!(!snapshot.peers[0].rotation_eligible);
        assert_eq!(snapshot.rotation.state, "empty");
        assert_eq!(snapshot.rotation.excluded[0].reason, "config_hash");

        let text = metrics.render();
        assert!(
            text.contains(
                "meatus_relay_peer_drift{relay_id=\"relay-a\",peer=\"relay-b\",kind=\"config_hash\"} 1"
            ),
            "{text}"
        );

        metrics.record_peer_probe("relay-b", PeerProbeOutcome::up(3, 3).with_config_hash(HASH));
        let snapshot = metrics.relay_snapshot().expect("relay snapshot");
        assert_eq!(snapshot.state, "healthy");
        assert!(snapshot.peers[0].rotation_eligible);
        assert_eq!(snapshot.rotation.state, "complete");
    }
}
