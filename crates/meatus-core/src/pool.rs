#![forbid(unsafe_code)]
//! 数据连接池：按连接统计在途流，限制单连接并发与总连接数。

use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

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

/// 跨会话共享的池指标；由服务端 `Metrics` 读取。
#[derive(Debug, Default)]
pub struct PoolStats {
    connections: AtomicI64,
    opens: AtomicU64,
    reuses: AtomicU64,
    waits: AtomicU64,
    timeouts: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolSnapshot {
    pub connections: u64,
    pub opens: u64,
    pub reuses: u64,
    pub waits: u64,
    pub timeouts: u64,
}

impl PoolStats {
    #[must_use]
    pub fn snapshot(&self) -> PoolSnapshot {
        PoolSnapshot {
            connections: u64::try_from(self.connections.load(Ordering::Relaxed)).unwrap_or(0),
            opens: self.opens.load(Ordering::Relaxed),
            reuses: self.reuses.load(Ordering::Relaxed),
            waits: self.waits.load(Ordering::Relaxed),
            timeouts: self.timeouts.load(Ordering::Relaxed),
        }
    }
}

struct PooledConn {
    id: PoolConnId,
    handle: OpenHandle,
    inflight: AtomicUsize,
    uses: AtomicU64,
    last_used: Mutex<Instant>,
    /// 控制连接只承载控制帧；有独立数据连接时不得再分配数据流。
    control_only: bool,
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
    stats: Arc<PoolStats>,
    notify: Arc<Notify>,
    wait_timeout: Duration,
}

impl ConnectionPool {
    #[must_use]
    pub fn new(max_conns: usize, max_streams_per_conn: usize) -> Self {
        Self::with_stats(
            max_conns,
            max_streams_per_conn,
            Arc::new(PoolStats::default()),
        )
    }

    #[must_use]
    pub fn with_stats(
        max_conns: usize,
        max_streams_per_conn: usize,
        stats: Arc<PoolStats>,
    ) -> Self {
        Self {
            max_conns: max_conns.max(1),
            max_streams_per_conn: max_streams_per_conn.max(1),
            conns: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            stats,
            notify: Arc::new(Notify::new()),
            wait_timeout: Duration::from_secs(5),
        }
    }

    /// 加入一条数据连接；超过 `size` 返回 [`PoolError::Full`]。
    pub fn add(&self, handle: OpenHandle) -> Result<PoolConnId, PoolError> {
        self.add_with_role(handle, false)
    }

    /// 加入主控制连接。控制连接仅作为没有数据连接时的兜底，
    /// 只要池内存在数据连接就不会被选中承载数据流。
    pub fn add_control(&self, handle: OpenHandle) -> Result<PoolConnId, PoolError> {
        self.add_with_role(handle, true)
    }

    fn add_with_role(
        &self,
        handle: OpenHandle,
        control_only: bool,
    ) -> Result<PoolConnId, PoolError> {
        let mut conns = lock(&self.conns);
        if !control_only && conns.iter().filter(|conn| !conn.control_only).count() >= self.max_conns
        {
            return Err(PoolError::Full);
        }
        if control_only && conns.iter().any(|conn| conn.control_only) {
            return Err(PoolError::Full);
        }
        let id = PoolConnId(self.next_id.fetch_add(1, Ordering::Relaxed));
        conns.push(Arc::new(PooledConn {
            id,
            handle,
            inflight: AtomicUsize::new(0),
            uses: AtomicU64::new(0),
            last_used: Mutex::new(Instant::now()),
            control_only,
        }));
        self.stats.connections.fetch_add(1, Ordering::Relaxed);
        Ok(id)
    }

    /// 按编号移除连接；连接已被移除时幂等返回。
    pub fn remove(&self, id: PoolConnId) {
        let mut removed = false;
        lock(&self.conns).retain(|conn| {
            if conn.id == id {
                removed = true;
                false
            } else {
                true
            }
        });
        if removed {
            self.stats.connections.fetch_sub(1, Ordering::Relaxed);
            self.notify.notify_waiters();
        }
    }

    /// 打开一条流，优先选择在途流最少的连接。
    ///
    /// 全部饱和时最多等待 5 秒；等待超时计入 `timeouts`。
    /// 打开失败会移除坏连接并尝试下一条。
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
            let has_data_conn = candidates.iter().any(|(conn, _)| !conn.control_only);
            for (conn, inflight) in &candidates {
                if conn.control_only && has_data_conn {
                    continue;
                }
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
                self.stats.waits.fetch_add(1, Ordering::Relaxed);
                if tokio::time::timeout(self.wait_timeout, self.notify.notified())
                    .await
                    .is_err()
                {
                    self.stats.timeouts.fetch_add(1, Ordering::Relaxed);
                    return Err(PoolError::StreamLimit);
                }
                continue;
            };
            if !conn.try_acquire(self.max_streams_per_conn, Instant::now()) {
                continue;
            }
            let reused = conn.uses.fetch_add(1, Ordering::Relaxed) > 0;
            match conn.handle.open().await {
                Ok(stream) => {
                    self.stats.opens.fetch_add(1, Ordering::Relaxed);
                    if reused {
                        self.stats.reuses.fetch_add(1, Ordering::Relaxed);
                    }
                    return Ok((
                        stream,
                        PoolGuard {
                            conn: Arc::clone(&conn),
                            notify: Arc::clone(&self.notify),
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
        conns.retain(|conn| {
            conn.control_only || conn.idle_for(now).is_none_or(|elapsed| elapsed < idle)
        });
        let removed = before - conns.len();
        if removed > 0 {
            self.stats
                .connections
                .fetch_sub(i64::try_from(removed).unwrap_or(0), Ordering::Relaxed);
            self.notify.notify_waiters();
        }
        removed
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

    #[cfg(test)]
    fn inflight_by_role(&self, control_only: bool) -> usize {
        lock(&self.conns)
            .iter()
            .filter(|conn| conn.control_only == control_only)
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

    #[must_use]
    pub fn stats(&self) -> Arc<PoolStats> {
        Arc::clone(&self.stats)
    }
}

/// 在途流计数守卫；流关闭时自动归还额度并唤醒等待者。
pub struct PoolGuard {
    conn: Arc<PooledConn>,
    notify: Arc<Notify>,
}

impl Drop for PoolGuard {
    fn drop(&mut self) {
        self.conn.release();
        self.notify.notify_one();
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
        assert_eq!(pool.stats().snapshot().connections, 2);
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
        assert_eq!(pool.stats().snapshot().connections, 1);
    }

    #[tokio::test]
    async fn waiting_open_is_released_by_guard() {
        let pool = Arc::new(ConnectionPool::new(1, 1));
        let h = make_handle();
        pool.add(h.open).expect("add");
        let (_stream, guard) = pool.open().await.expect("first open");
        let waiter = tokio::spawn({
            let pool = Arc::clone(&pool);
            async move { pool.open().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(pool.stats().snapshot().waits, 1);
        drop(guard);
        let (_stream2, guard2) = waiter.await.expect("join").expect("second open");
        assert_eq!(pool.inflight(), 1);
        drop(guard2);
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
        let (_s3, g3) = pool.open().await.expect("third");
        assert_eq!(pool.inflight(), 3);
        let snapshot = pool.stats().snapshot();
        assert_eq!(snapshot.opens, 3);
        assert_eq!(snapshot.reuses, 1);
        drop((g1, g2, g3));
        assert_eq!(pool.inflight(), 0);
    }

    #[tokio::test]
    async fn control_connection_does_not_consume_data_capacity() {
        let pool = ConnectionPool::new(1, 4);
        let control = make_handle();
        let data = make_handle();
        pool.add_control(control.open).expect("add control");
        pool.add(data.open)
            .expect("control must not occupy data slot");
        assert_eq!(pool.len(), 2);
    }

    #[tokio::test]
    async fn control_connection_is_not_used_for_data_when_data_exists() {
        let pool = ConnectionPool::new(2, 4);
        let control = make_handle();
        let data = make_handle();
        pool.add_control(control.open).expect("add control");
        pool.add(data.open).expect("add data");

        let mut guards = Vec::new();
        for _ in 0..4 {
            let (_stream, guard) = pool.open().await.expect("open data stream");
            guards.push(guard);
        }
        assert_eq!(pool.inflight_by_role(true), 0, "control must stay clean");
        assert_eq!(pool.inflight_by_role(false), 4);
        drop(guards);
        assert_eq!(pool.inflight(), 0);
    }

    #[tokio::test]
    async fn control_connection_is_only_a_fallback_without_data() {
        let pool = ConnectionPool::new(1, 4);
        let control = make_handle();
        pool.add_control(control.open).expect("add control");
        let (_stream, guard) = pool.open().await.expect("control fallback");
        assert_eq!(pool.inflight(), 1);
        drop(guard);
    }

    #[tokio::test]
    async fn idle_reaper_keeps_control_connection() {
        let pool = ConnectionPool::new(2, 4);
        let control = make_handle();
        let data = make_handle();
        pool.add_control(control.open).expect("add control");
        pool.add(data.open).expect("add data");
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(pool.reap_idle(Duration::from_millis(1)), 1);
        assert_eq!(pool.len(), 1, "control connection must survive reaping");
    }

    #[tokio::test]
    async fn broken_connection_is_removed_and_pool_recovers() {
        let pool = ConnectionPool::new(2, 1);
        let first = make_handle();
        let Handle {
            open,
            _inbound,
            _peer,
        } = first;
        pool.add(open).expect("add first");
        // 模拟底层连接被断开：驱动任务结束，OpenHandle::open 返回 Closed。
        drop(_inbound);
        drop(_peer);
        assert_eq!(pool.open().await.err(), Some(PoolError::Closed));
        assert!(pool.is_empty(), "broken connection must be evicted");

        let second = make_handle();
        pool.add(second.open).expect("add second");
        assert!(pool.open().await.is_ok(), "pool must recover");
        assert_eq!(pool.len(), 1);
        let snapshot = pool.stats().snapshot();
        assert_eq!(snapshot.connections, 1);
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
        assert_eq!(pool.stats().snapshot().connections, 0);
    }
}
