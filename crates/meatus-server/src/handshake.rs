#![forbid(unsafe_code)]
//! 控制面握手保护：同 IP 滑动窗口限流与失败延迟。

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const HANDSHAKE_WINDOW: Duration = Duration::from_secs(60);
pub const HANDSHAKE_MAX_PER_WINDOW: usize = 10;
pub const HANDSHAKE_FAIL_DELAY: Duration = Duration::from_secs(3);
const MAX_TRACKED_IPS: usize = 4096;

/// 按来源 IP 记录最近一分钟内的握手尝试。
///
/// 超过 `HANDSHAKE_MAX_PER_WINDOW` 后拒绝新握手；窗口按滑动时间计算。
/// 条目设上限，避免攻击者用大量伪造源地址把内存撑大。
#[derive(Default)]
pub struct HandshakeLimiter {
    attempts: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
}

impl HandshakeLimiter {
    #[must_use]
    pub fn allow(&self, ip: IpAddr) -> bool {
        self.allow_at(ip, Instant::now())
    }

    fn allow_at(&self, ip: IpAddr, now: Instant) -> bool {
        let ip = normalize_ip(ip);
        let mut attempts = self
            .attempts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if !attempts.contains_key(&ip) && attempts.len() >= MAX_TRACKED_IPS {
            prune_expired(&mut attempts, now);
            if attempts.len() >= MAX_TRACKED_IPS {
                return false;
            }
        }

        let entries = attempts.entry(ip).or_default();
        prune_entry(entries, now);
        if entries.len() >= HANDSHAKE_MAX_PER_WINDOW {
            return false;
        }
        entries.push_back(now);
        true
    }
}

fn prune_expired(attempts: &mut HashMap<IpAddr, VecDeque<Instant>>, now: Instant) {
    attempts.retain(|_, entries| {
        prune_entry(entries, now);
        !entries.is_empty()
    });
}

fn prune_entry(entries: &mut VecDeque<Instant>, now: Instant) {
    while entries
        .front()
        .is_some_and(|at| now.saturating_duration_since(*at) >= HANDSHAKE_WINDOW)
    {
        entries.pop_front();
    }
}

fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        IpAddr::V4(_) => ip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("valid ip")
    }

    #[test]
    fn allows_ten_attempts_then_blocks_eleventh() {
        let limiter = HandshakeLimiter::default();
        let now = Instant::now();
        for offset in 0..HANDSHAKE_MAX_PER_WINDOW {
            assert!(limiter.allow_at(
                ip("203.0.113.10"),
                now + Duration::from_millis(offset as u64)
            ));
        }
        assert!(!limiter.allow_at(ip("203.0.113.10"), now + Duration::from_secs(1)));
    }

    #[test]
    fn window_rolls_forward_after_sixty_seconds() {
        let limiter = HandshakeLimiter::default();
        let now = Instant::now();
        for offset in 0..HANDSHAKE_MAX_PER_WINDOW {
            assert!(limiter.allow_at(
                ip("203.0.113.10"),
                now + Duration::from_millis(offset as u64)
            ));
        }
        assert!(!limiter.allow_at(ip("203.0.113.10"), now + Duration::from_secs(59)));
        assert!(limiter.allow_at(ip("203.0.113.10"), now + Duration::from_secs(61)));
    }

    #[test]
    fn separate_ips_have_separate_buckets() {
        let limiter = HandshakeLimiter::default();
        let now = Instant::now();
        for _ in 0..HANDSHAKE_MAX_PER_WINDOW {
            assert!(limiter.allow_at(ip("203.0.113.10"), now));
        }
        assert!(!limiter.allow_at(ip("203.0.113.10"), now));
        assert!(limiter.allow_at(ip("203.0.113.11"), now));
    }

    #[test]
    fn ipv4_mapped_loopback_shares_the_ipv4_bucket() {
        let limiter = HandshakeLimiter::default();
        let now = Instant::now();
        for _ in 0..HANDSHAKE_MAX_PER_WINDOW {
            assert!(limiter.allow_at(ip("127.0.0.1"), now));
        }
        assert!(!limiter.allow_at(ip("::ffff:127.0.0.1"), now));
    }

    #[test]
    fn ipv4_mapped_normalization_is_stable() {
        assert_eq!(normalize_ip(ip("::ffff:127.0.0.1")), ip("127.0.0.1"));
        assert_eq!(normalize_ip(ip("2001:db8::1")), ip("2001:db8::1"));
    }

    #[test]
    fn tracked_ip_capacity_is_bounded() {
        let limiter = HandshakeLimiter::default();
        let now = Instant::now();
        for index in 0..MAX_TRACKED_IPS {
            let source = IpAddr::V4(Ipv4Addr::new(
                10,
                (index / 256) as u8,
                (index % 256) as u8,
                1,
            ));
            assert!(limiter.allow_at(source, now));
        }
        assert!(!limiter.allow_at(ip("198.51.100.1"), now));
    }
}
