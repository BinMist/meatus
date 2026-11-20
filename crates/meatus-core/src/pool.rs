#![forbid(unsafe_code)]
//! 数据连接池：按连接统计在途流，限制单连接并发与总连接数。

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::transport::{MultiplexedStream, OpenHandle};

/// 池内连接的唯一编号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PoolConnId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PoolError {
    #[error("connection pool is full")]
    Full,
    #[error("all pooled connections reached max_streams_per_conn")]
    StreamLimit,
    #[error("pooled connection is closed")]
    Closed,
}

struct PooledConn {
    id: PoolConnId,
    handle: OpenHandle,
    inflight: AtomicUsize,
    last_used: Mutex<Instant>,
}

impl PooledConn {
    fn try_acquire(&self, max_streams: usize, now: Instant) -> bool {
        let mut current = self.inflight.load(Ordering::Acquire);
        loop {
            if current >= max_streams {
                return false;
            }
            match self.inflight.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    *lock(&self.last_used) = now;
                    return true;
                }
                Err(next) => current = next,
            }
        }
    }

    fn release(&self) {
        self.inflight.fetch_sub(1, Ordering::AcqRel);
        *lock(&self.last_used) = Instant::now();
    }

    fn idle_for(&self, now: Instant) -> Option<Duration> {
        if self.inflight.load(Ordering::Acquire) != 0 {
            return None;
        }
        Some(now.saturating_duration_since(*lock(&self.last_used)))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 一个客户端会话的数据连接池。
///
/// `size` 含控制连接本身；`max_streams_per_conn` 是每条 yamux 连接的并发流上限。
pub struct ConnectionPool {
    max_conns: usize,
    max_streams_per_conn: usize,
    conns: Mutex<Vec<Arc<PooledConn>>>,
    next_id: AtomicU64,
}

impl ConnectionPool {
    #[must_use]
    pub fn new(max_conns: usize, max_streams_per_conn: usize) -> Self {
        Self {
            max_conns: max_conns.max(1),
            max_streams_per_conn: max_streams_per_conn.max(1),
            conns: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// 加入一条连接；超过 `size` 返回 [`PoolError::Full`]。
    pub fn add(&self, handle: OpenHandle) -> Result<PoolConnId, PoolError> {
        let mut conns = lock(&self.conns);
        if conns.len() >= self.max_conns {
            return Err(PoolError::Full);
        }
        let id = PoolConnId(self.next_id.fetch_add(1, Ordering::Relaxed));
        conns.push(Arc::new(PooledConn {
            id,
            handle,
            inflight: AtomicUsize::new(0),
            last_used: Mutex::new(Instant::now()),
        }));
        Ok(id)
    }

    /// 按编号移除连接；连接已被移除时幂等返回。
    pub fn remove(&self, id: PoolConnId) {
        lock(&self.conns).retain(|conn| conn.id != id);
    }

    /// 打开一条流，优先选择在途流最少的连接。
    ///
    /// 打开失败会移除坏连接并尝试下一条；全部饱和或全部失败时返回错误。
    pub async fn open(&self) -> Result<(MultiplexedStream, PoolGuard), PoolError> {
        loop {
            let candidates = {
                let conns = lock(&self.conns);
                conns
                    .iter()
                    .map(|conn| (Arc::clone(conn), conn.inflight.load(Ordering::Acquire)))
                    .collect::<Vec<_>>()
            };
            if candidates.is_empty() {
                return Err(PoolError::Closed);
            }
            let mut chosen = None;
            for (conn, inflight) in &candidates {
                if *inflight >= self.max_streams_per_conn {
                    continue;
                }
                if chosen.as_ref().is_none_or(|best: &Arc<PooledConn>| {
                    conn.inflight.load(Ordering::Acquire) < best.inflight.load(Ordering::Acquire)
                }) {
                    chosen = Some(Arc::clone(conn));
                }
            }
            let Some(conn) = chosen else {
                return Err(PoolError::StreamLimit);
            };
            if !conn.try_acquire(self.max_streams_per_conn, Instant::now()) {
                continue;
            }
            match conn.handle.open().await {
                Ok(stream) => {
                    return Ok((
                        stream,
                        PoolGuard {
                            conn: Arc::clone(&conn),
                            released: false,
                        },
                    ));
                }
                Err(_) => {
                    conn.release();
                    self.remove(conn.id);
                }
            }
        }
    }

    /// 回收空闲超过 `idle` 且没有在途流的连接，返回回收数量。
    pub fn reap_idle(&self, idle: Duration) -> usize {
        let now = Instant::now();
        let mut conns = lock(&self.conns);
        let before = conns.len();
        conns.retain(|conn| conn.idle_for(now).is_none_or(|elapsed| elapsed < idle));
        before - conns.len()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        lock(&self.conns).len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn inflight(&self) -> usize {
        lock(&self.conns)
            .iter()
            .map(|conn| conn.inflight.load(Ordering::Acquire))
            .sum()
    }

    #[must_use]
    pub fn max_conns(&self) -> usize {
        self.max_conns
    }

    #[must_use]
    pub fn max_streams_per_conn(&self) -> usize {
        self.max_streams_per_conn
    }
}

/// 在途流计数守卫；流关闭时自动归还额度。
pub struct PoolGuard {
    conn: Arc<PooledConn>,
    released: bool,
}

impl Drop for PoolGuard {
    fn drop(&mut self) {
        if !self.released {
            self.conn.release();
            self.released = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;
    use tokio_util::compat::TokioAsyncReadCompatExt;

    struct Handle {
        open: OpenHandle,
        _inbound: crate::multiplex::Inbound,
        _peer: DuplexStream,
    }

    fn make_handle() -> Handle {
        let (a, b) = tokio::io::duplex(1024);
        let (open, inbound) = crate::multiplex::spawn(a.compat(), yamux::Mode::Client);
        Handle {
            open,
            _inbound: inbound,
            _peer: b,
        }
    }

    #[tokio::test]
    async fn rejects_more_than_size() {
        let pool = ConnectionPool::new(2, 4);
        let h1 = make_handle();
        let h2 = make_handle();
        assert_eq!(pool.add(h1.open.clone()), Ok(PoolConnId(1)));
        assert_eq!(pool.add(h2.open), Ok(PoolConnId(2)));
        assert_eq!(pool.add(h1.open.clone()), Err(PoolError::Full));
        assert_eq!(pool.len(), 2);
    }

    #[tokio::test]
    async fn remove_is_idempotent_and_frees_a_slot() {
        let pool = ConnectionPool::new(1, 4);
        let h1 = make_handle();
        let h2 = make_handle();
        let id = pool.add(h1.open).expect("first");
        assert_eq!(pool.add(h2.open.clone()), Err(PoolError::Full));
        pool.remove(id);
        pool.remove(id);
        assert_eq!(pool.add(h2.open), Ok(PoolConnId(2)));
        assert_eq!(pool.len(), 1);
    }

    #[tokio::test]
    async fn stream_limit_is_enforced_and_guard_releases() {
        let pool = ConnectionPool::new(1, 1);
        let h = make_handle();
        pool.add(h.open).expect("add");
        let (_stream, guard) = pool.open().await.expect("first open");
        assert_eq!(pool.inflight(), 1);
        assert_eq!(pool.open().await.err(), Some(PoolError::StreamLimit));
        drop(guard);
        assert_eq!(pool.inflight(), 0);
    }

    #[tokio::test]
    async fn open_prefers_the_least_loaded_connection() {
        let pool = ConnectionPool::new(2, 2);
        let h1 = make_handle();
        let h2 = make_handle();
        pool.add(h1.open).expect("add 1");
        pool.add(h2.open).expect("add 2");
        let (_s1, g1) = pool.open().await.expect("first");
        let (_s2, g2) = pool.open().await.expect("second");
        assert_eq!(pool.inflight(), 2);
        drop((g1, g2));
        assert_eq!(pool.inflight(), 0);
    }

    #[tokio::test]
    async fn idle_reap_keeps_recent_connections() {
        let pool = ConnectionPool::new(1, 4);
        let h = make_handle();
        pool.add(h.open).expect("add");
        assert_eq!(pool.reap_idle(Duration::from_secs(1)), 0);
        assert_eq!(pool.len(), 1);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(pool.reap_idle(Duration::from_millis(10)), 1);
        assert!(pool.is_empty());
    }
}
