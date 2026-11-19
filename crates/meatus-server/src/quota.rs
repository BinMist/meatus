#![forbid(unsafe_code)]
//! 自然月流量配额：按 client 与 proxy 双维度累计，月切换时自动归零。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use meatus_core::{QuotaAction, RateLimiter};
use serde_json::{Value, json};

use crate::metrics::ProxyMetrics;

const QUOTA_THROTTLE_BPS: u64 = 64 * 1024;

#[derive(Debug, Clone)]
struct Entry {
    month: String,
    used: u64,
}

#[derive(Debug, Default)]
pub struct MonthlyQuota {
    inner: Mutex<HashMap<String, Entry>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct QuotaSnapshot {
    pub used_bytes: u64,
    pub limit_bytes: u64,
    pub reset_at_ms: i64,
}

impl MonthlyQuota {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn used(&self, key: &str) -> u64 {
        let now = month_key(SystemTime::now());
        let mut inner = self.lock();
        match inner.get_mut(key) {
            Some(entry) if entry.month == now => entry.used,
            Some(entry) => {
                entry.month = now;
                entry.used = 0;
                0
            }
            None => 0,
        }
    }

    pub fn add(&self, client_key: &str, proxy_key: &str, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let now = month_key(SystemTime::now());
        let mut inner = self.lock();
        for key in [client_key, proxy_key] {
            let entry = inner.entry(key.to_string()).or_insert_with(|| Entry {
                month: now.clone(),
                used: 0,
            });
            if entry.month != now {
                entry.month = now.clone();
                entry.used = 0;
            }
            entry.used = entry.used.saturating_add(bytes);
        }
    }

    #[must_use]
    pub fn snapshot(&self, client_key: &str, proxy_key: &str, limit: u64) -> QuotaSnapshot {
        let used = self.used(client_key).max(self.used(proxy_key));
        QuotaSnapshot {
            used_bytes: used,
            limit_bytes: limit,
            reset_at_ms: next_month_start_ms(),
        }
    }
}

/// 一个 proxy 的配额门卫；所有连接入口共用。
#[derive(Clone)]
pub struct ProxyQuota {
    tracker: Arc<MonthlyQuota>,
    client_key: String,
    proxy_key: String,
    limit: u64,
    action: QuotaAction,
    limiter: Arc<RateLimiter>,
    base_rate: u64,
    metrics: Arc<ProxyMetrics>,
}

impl ProxyQuota {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tracker: Arc<MonthlyQuota>,
        tenant_id: &str,
        client_id: &str,
        proxy_name: &str,
        limit: u64,
        action: QuotaAction,
        limiter: Arc<RateLimiter>,
        base_rate: u64,
        metrics: Arc<ProxyMetrics>,
    ) -> Self {
        limiter.set_rate(base_rate);
        Self {
            tracker,
            client_key: format!("{tenant_id}\0{client_id}"),
            proxy_key: format!("{tenant_id}\0{proxy_name}"),
            limit,
            action,
            limiter,
            base_rate,
            metrics,
        }
    }

    /// 返回 `false` 表示拒绝新连接；`true` 表示放行（可能已降速）。
    #[must_use]
    pub fn allow_connection(&self) -> bool {
        if self.limit == 0 {
            return true;
        }
        let used = self
            .tracker
            .snapshot(&self.client_key, &self.proxy_key, self.limit)
            .used_bytes;
        if used < self.limit {
            if self.limiter.rate() != self.base_rate {
                self.limiter.set_rate(self.base_rate);
            }
            return true;
        }
        match self.action {
            QuotaAction::Reject => {
                self.metrics.quota_denied();
                false
            }
            QuotaAction::Throttle => {
                let throttled = QUOTA_THROTTLE_BPS.min(if self.base_rate == 0 {
                    QUOTA_THROTTLE_BPS
                } else {
                    self.base_rate
                });
                self.limiter.set_rate(throttled);
                self.metrics.quota_throttled();
                true
            }
        }
    }

    pub fn record(&self, bytes_in: u64, bytes_out: u64) {
        self.tracker.add(
            &self.client_key,
            &self.proxy_key,
            bytes_in.saturating_add(bytes_out),
        );
    }

    #[must_use]
    pub fn snapshot(&self) -> QuotaSnapshot {
        self.tracker
            .snapshot(&self.client_key, &self.proxy_key, self.limit)
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        let snapshot = self.snapshot();
        json!({
            "used_bytes": snapshot.used_bytes,
            "limit_bytes": snapshot.limit_bytes,
            "reset_at_ms": snapshot.reset_at_ms,
        })
    }
}

fn month_key(at: SystemTime) -> String {
    let secs = at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let (year, month, _) = civil_from_days(secs.div_euclid(86_400));
    format!("{year:04}-{month:02}")
}

fn next_month_start_ms() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    let days = secs.div_euclid(86_400);
    let (year, month, _) = civil_from_days(days);
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    days_from_civil(next_year, next_month, 1) * 86_400 * 1000
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    (year as i32, month as u32, day as u32)
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let month = i64::from(month);
    let day = i64::from(day);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn month_key_matches_known_dates() {
        let at = UNIX_EPOCH + std::time::Duration::from_secs(1_797_000_000);
        assert_eq!(month_key(at), "2026-12");
    }

    #[test]
    fn quota_counts_both_dimensions_and_resets() {
        let tracker = MonthlyQuota::default();
        tracker.add("c", "p", 10);
        assert_eq!(tracker.snapshot("c", "p", 100).used_bytes, 10);
        tracker.add("c", "p", 5);
        assert_eq!(tracker.snapshot("c", "p", 100).used_bytes, 15);
    }

    #[test]
    fn reject_denies_and_throttle_allows() {
        let metrics = Arc::new(ProxyMetrics::default());
        let limiter = Arc::new(RateLimiter::new(1_000_000));
        let tracker = Arc::new(MonthlyQuota::default());
        tracker.add("t\0c", "t\0p", 10);
        let reject = ProxyQuota::new(
            Arc::clone(&tracker),
            "t",
            "c",
            "p",
            10,
            QuotaAction::Reject,
            Arc::clone(&limiter),
            1_000_000,
            Arc::clone(&metrics),
        );
        assert!(!reject.allow_connection());
        let throttle = ProxyQuota::new(
            tracker,
            "t",
            "c",
            "p",
            10,
            QuotaAction::Throttle,
            Arc::clone(&limiter),
            1_000_000,
            metrics,
        );
        assert!(throttle.allow_connection());
        assert_eq!(limiter.rate(), QUOTA_THROTTLE_BPS);
    }
}
