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
    let peer_epoch = Some(health.epoch);
    if health.id != peer.id {
        return PeerProbeOutcome::down(
            latency(started),
            peer_epoch,
            format!("peer id mismatch: expected {}, got {}", peer.id, health.id),
        );
    }
    if health.group != relay.group() {
        return PeerProbeOutcome::down(
            latency(started),
            peer_epoch,
            format!(
                "peer group mismatch: expected {}, got {}",
                relay.group(),
                health.group
            ),
        );
    }
    if health.role != peer.role.as_str() {
        return PeerProbeOutcome::down(
            latency(started),
            peer_epoch,
            format!(
                "peer role mismatch: expected {}, got {}",
                peer.role.as_str(),
                health.role
            ),
        );
    }
    if health.epoch != relay.epoch() {
        return PeerProbeOutcome::down(
            latency(started),
            peer_epoch,
            format!(
                "peer epoch mismatch: expected {}, got {}",
                relay.epoch(),
                health.epoch
            ),
        );
    }
    PeerProbeOutcome::up(latency(started), health.epoch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_health_response_requires_identity_and_epoch() {
        let text =
            br#"{"id":"relay-b","group":"prod","role":"secondary","epoch":7,"cert_sync":"off"}"#;
        let health: PeerHealthResponse = serde_json::from_slice(text).expect("decode");
        assert_eq!(health.id, "relay-b");
        assert_eq!(health.group, "prod");
        assert_eq!(health.role, "secondary");
        assert_eq!(health.epoch, 7);
    }

    #[test]
    fn peer_probe_outcome_marks_failure_with_epoch() {
        let outcome = PeerProbeOutcome::down(3, Some(8), "peer epoch mismatch");
        assert!(!outcome.up);
        assert_eq!(outcome.peer_epoch, Some(8));
        assert_eq!(outcome.error.as_deref(), Some("peer epoch mismatch"));
    }
}
