//! 令牌桶限速与带限速的双向拷贝。
//!
//! 限速只延迟读侧，不丢包；`rate = 0` 表示不限速。桶容量为 1 秒流量，
//! 首次可突发 1 秒额度，之后按字节补充。

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::time::{Instant, sleep};

struct Bucket {
    tokens: f64,
    last: Instant,
}

pub struct RateLimiter {
    rate: AtomicU64,
    bucket: Mutex<Bucket>,
}

impl RateLimiter {
    #[must_use]
    pub fn new(bytes_per_sec: u64) -> Self {
        let rate = bytes_per_sec as f64;
        Self {
            rate: AtomicU64::new(bytes_per_sec),
            bucket: Mutex::new(Bucket {
                tokens: rate,
                last: Instant::now(),
            }),
        }
    }

    /// 运行时调整速率；用于月配额耗尽后的降速与下个周期恢复。
    pub fn set_rate(&self, bytes_per_sec: u64) {
        self.rate.store(bytes_per_sec, Ordering::Relaxed);
        let cap = bytes_per_sec as f64;
        let mut bucket = self
            .bucket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        bucket.tokens = bucket.tokens.min(cap);
        bucket.last = Instant::now();
    }

    #[must_use]
    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    /// 等待直到本次 `bytes` 的额度可用。`0` 表示不限速。
    pub async fn consume(&self, bytes: u64) {
        let rate = self.rate();
        if rate == 0 || bytes == 0 {
            return;
        }

        let wait = {
            let rate = rate as f64;
            let mut bucket = self
                .bucket
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let elapsed = now.duration_since(bucket.last).as_secs_f64();
            bucket.last = now;
            bucket.tokens = (bucket.tokens + elapsed * rate).min(rate);
            bucket.tokens -= bytes as f64;
            if bucket.tokens < 0.0 {
                Duration::from_secs_f64(-bucket.tokens / rate)
            } else {
                Duration::ZERO
            }
        };

        if !wait.is_zero() {
            sleep(wait).await;
        }
    }

    /// 非阻塞消费；额度不足直接返回 `false`，用于新建连接速率。
    #[must_use]
    pub fn try_consume(&self, bytes: u64) -> bool {
        let rate = self.rate();
        if rate == 0 || bytes == 0 {
            return true;
        }

        let rate = rate as f64;
        let mut bucket = self
            .bucket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.last = now;
        bucket.tokens = (bucket.tokens + elapsed * rate).min(rate);
        if bucket.tokens >= bytes as f64 {
            bucket.tokens -= bytes as f64;
            true
        } else {
            false
        }
    }
}

/// 与 `tokio::io::copy_bidirectional` 相同的语义，但两个方向共享一个令牌桶。
///
/// 返回 `(a_to_b, b_to_a)` 字节数。
pub async fn copy_bidirectional_limited<A, B>(
    a: &mut A,
    b: &mut B,
    limiter: &RateLimiter,
) -> std::io::Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut a_reader, mut a_writer) = tokio::io::split(a);
    let (mut b_reader, mut b_writer) = tokio::io::split(b);
    tokio::try_join!(
        copy_one_direction(&mut a_reader, &mut b_writer, limiter),
        copy_one_direction(&mut b_reader, &mut a_writer, limiter),
    )
}

async fn copy_one_direction<R, W>(
    reader: &mut ReadHalf<R>,
    writer: &mut WriteHalf<W>,
    limiter: &RateLimiter,
) -> std::io::Result<u64>
where
    R: AsyncRead + AsyncWrite + Unpin,
    W: AsyncRead + AsyncWrite + Unpin,
{
    let mut buffer = vec![0u8; 16 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        limiter.consume(read as u64).await;
        writer.write_all(&buffer[..read]).await?;
        total += read as u64;
    }
    writer.shutdown().await?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn zero_rate_is_unlimited() {
        let limiter = RateLimiter::new(0);
        limiter.consume(1024 * 1024).await;
    }

    #[tokio::test]
    async fn burst_under_capacity_is_immediate() {
        let limiter = RateLimiter::new(1024);
        limiter.consume(512).await;
        limiter.consume(256).await;
    }

    #[test]
    fn rate_is_stored() {
        let limiter = RateLimiter::new(4096);
        assert_eq!(limiter.rate(), 4096);
        limiter.set_rate(1024);
        assert_eq!(limiter.rate(), 1024);
    }

    #[test]
    fn try_consume_rejects_when_tokens_empty() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.try_consume(1));
        assert!(limiter.try_consume(1));
        assert!(!limiter.try_consume(1));
    }
}
