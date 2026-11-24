#![forbid(unsafe_code)]
//! Relay peer 健康探测与状态上报。
//!
//! 复用 `health_bind` 上的 `/relay/v1/health`：本机每 `health_interval_s`
//! 探测一次 bundle 内其余成员，结果写入 `Metrics`，由 `/healthz` 与
//! `/metrics` 统一读取。

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::ServerState;
use crate::metrics::PeerProbeOutcome;
use crate::relay_peer::PEER_PATH_HEALTH;

#[derive(Debug, Deserialize)]
struct PeerHealthResponse {
    id: String,
    group: String,
    role: String,
    epoch: u64,
    config_hash: String,
}

pub(crate) async fn run(state: Arc<ServerState>, cancel: CancellationToken) -> anyhow::Result<()> {
    let Some(relay) = state.relay.as_ref() else {
        return Ok(());
    };
    let Some(section) = state.config.relay.as_ref() else {
        return Ok(());
    };
    let timeout = Duration::from_secs(section.peer_timeout_s);
    let client = reqwest::Client::builder().timeout(timeout).build()?;
    let mut ticker = tokio::time::interval(Duration::from_secs(section.health_interval_s));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = ticker.tick() => {
                for peer in relay.peers() {
                    let outcome = probe_peer(&client, relay, peer).await;
                    state.metrics.record_peer_probe(&peer.id, outcome);
                }
            }
        }
    }
    debug!("relay health probe stopped");
    Ok(())
}

async fn probe_peer(
    client: &reqwest::Client,
    relay: &crate::relay::RelayContext,
    peer: &meatus_core::RelayMember,
) -> PeerProbeOutcome {
    let started = Instant::now();
    let latency =
        |started: Instant| u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let Some(address) = peer.peer_addr.as_deref() else {
        return PeerProbeOutcome::down(
            latency(started),
            None,
            "peer_addr missing from verified bundle",
        );
    };
    let url = format!("http://{address}{PEER_PATH_HEALTH}");
    let response = match client.get(&url).send().await {
        Ok(response) => response,
        Err(err) => {
            return PeerProbeOutcome::down(latency(started), None, format!("probe {url}: {err}"));
        }
    };
    let status = response.status();
    let body = match response.bytes().await {
        Ok(body) => body,
        Err(err) => {
            return PeerProbeOutcome::down(
                latency(started),
                None,
                format!("read health from {url}: {err}"),
            );
        }
    };
    if !status.is_success() {
        return PeerProbeOutcome::down(
            latency(started),
            None,
            format!("health from {url} returned {status}"),
        );
    }
    let health: PeerHealthResponse = match serde_json::from_slice(&body) {
        Ok(health) => health,
        Err(err) => {
            return PeerProbeOutcome::down(
                latency(started),
                None,
                format!("decode health from {url}: {err}"),
            );
        }
    };
    evaluate_health(relay, peer, &health, latency(started))
}

/// 纯函数校验 peer 上报；配置 hash 不一致视为漂移，立即失去轮转资格。
fn evaluate_health(
    relay: &crate::relay::RelayContext,
    peer: &meatus_core::RelayMember,
    health: &PeerHealthResponse,
    latency_ms: u64,
) -> PeerProbeOutcome {
    let peer_epoch = Some(health.epoch);
    let peer_hash = Some(health.config_hash.clone());
    if health.id != peer.id {
        return PeerProbeOutcome::drift(
            latency_ms,
            peer_epoch,
            peer_hash,
            "identity",
            format!("peer id mismatch: expected {}, got {}", peer.id, health.id),
        );
    }
    if health.group != relay.group() {
        return PeerProbeOutcome::drift(
            latency_ms,
            peer_epoch,
            peer_hash,
            "identity",
            format!(
                "peer group mismatch: expected {}, got {}",
                relay.group(),
                health.group
            ),
        );
    }
    if health.role != peer.role.as_str() {
        return PeerProbeOutcome::drift(
            latency_ms,
            peer_epoch,
            peer_hash,
            "identity",
            format!(
                "peer role mismatch: expected {}, got {}",
                peer.role.as_str(),
                health.role
            ),
        );
    }
    if health.epoch != relay.epoch() {
        return PeerProbeOutcome::drift(
            latency_ms,
            peer_epoch,
            peer_hash,
            "epoch",
            format!(
                "peer epoch mismatch: expected {}, got {}",
                relay.epoch(),
                health.epoch
            ),
        );
    }
    if health.config_hash != relay.config_hash() {
        return PeerProbeOutcome::drift(
            latency_ms,
            peer_epoch,
            peer_hash,
            "config_hash",
            format!(
                "peer config hash mismatch: expected {}, got {}",
                relay.config_hash(),
                health.config_hash
            ),
        );
    }
    PeerProbeOutcome::up(latency_ms, health.epoch).with_config_hash(health.config_hash.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_health_response_requires_identity_epoch_and_hash() {
        let text = br#"{"id":"relay-b","group":"prod","role":"secondary","epoch":7,"cert_sync":"off","config_hash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#;
        let health: PeerHealthResponse = serde_json::from_slice(text).expect("decode");
        assert_eq!(health.id, "relay-b");
        assert_eq!(health.group, "prod");
        assert_eq!(health.role, "secondary");
        assert_eq!(health.epoch, 7);
        assert!(health.config_hash.starts_with("sha256:"));
    }

    #[test]
    fn peer_probe_outcome_marks_failure_with_epoch() {
        let outcome = PeerProbeOutcome::down(3, Some(8), "peer epoch mismatch");
        assert!(!outcome.up);
        assert_eq!(outcome.peer_epoch, Some(8));
        assert_eq!(outcome.error.as_deref(), Some("peer epoch mismatch"));
    }

    #[test]
    fn config_hash_mismatch_is_an_immediate_drift() {
        use meatus_core::{RelayBundle, RelayMember, RelayRole};

        const HASH: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let main = RelayMember {
            id: "relay-a".to_string(),
            role: RelayRole::Primary,
            control_addr: "127.0.0.1:7000".to_string(),
            http_domains: Vec::new(),
            peer_addr: Some("127.0.0.1:7401".to_string()),
            ports: [20_000, 20_999],
        };
        let peer = RelayMember {
            id: "relay-b".to_string(),
            role: RelayRole::Secondary,
            control_addr: "127.0.0.1:7001".to_string(),
            http_domains: Vec::new(),
            peer_addr: Some("127.0.0.1:7402".to_string()),
            ports: [21_000, 21_999],
        };
        let relay = crate::relay::RelayContext {
            bundle: RelayBundle {
                version: 1,
                group: "prod".to_string(),
                epoch: 3,
                generated_at: "2026-11-24T00:00:00Z".to_string(),
                relays: vec![main.clone(), peer.clone()],
                config_hash: HASH.to_string(),
                signature: String::new(),
            },
            member: main,
        };
        let good = PeerHealthResponse {
            id: "relay-b".to_string(),
            group: "prod".to_string(),
            role: "secondary".to_string(),
            epoch: 3,
            config_hash: HASH.to_string(),
        };
        let outcome = evaluate_health(&relay, &peer, &good, 2);
        assert!(outcome.up);
        assert_eq!(outcome.drift, None);
        assert_eq!(outcome.peer_config_hash.as_deref(), Some(HASH));

        let drifted = PeerHealthResponse {
            config_hash: format!("sha256:{}", "bb".repeat(32)),
            ..good
        };
        let outcome = evaluate_health(&relay, &peer, &drifted, 2);
        assert!(!outcome.up);
        assert_eq!(outcome.drift.as_deref(), Some("config_hash"));
        assert!(
            outcome
                .error
                .as_deref()
                .is_some_and(|err| err.contains("config hash mismatch")),
            "{}",
            outcome.error.as_deref().unwrap_or("")
        );
    }
}
