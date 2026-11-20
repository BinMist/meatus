#![forbid(unsafe_code)]
//! 服务端会话数据池注册表。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use meatus_core::{ConnectionPool, OpenHandle, PoolConnId, PoolError, PoolStats};
use tokio_util::sync::CancellationToken;

struct SessionPool {
    pool: Arc<ConnectionPool>,
    tenant_id: String,
    client_id: String,
    cancel: CancellationToken,
}

/// 按主会话 `run_id` 管理数据连接池；主会话结束时统一取消。
pub struct SessionPools {
    inner: Mutex<HashMap<String, Arc<SessionPool>>>,
    stats: Arc<PoolStats>,
}

impl Default for SessionPools {
    fn default() -> Self {
        Self::with_stats(Arc::new(PoolStats::default()))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl SessionPools {
    #[must_use]
    pub fn with_stats(stats: Arc<PoolStats>) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            stats,
        }
    }

    /// 主控制连接收到 Welcome 后创建池；同一 `run_id` 重复创建视为错误。
    pub fn create(
        &self,
        run_id: &str,
        tenant_id: &str,
        client_id: &str,
        handle: OpenHandle,
        pool_size: u32,
        max_streams_per_conn: u32,
    ) -> Result<Arc<ConnectionPool>, PoolError> {
        let size = usize::try_from(pool_size.clamp(1, 16)).unwrap_or(1);
        let max_streams = usize::try_from(max_streams_per_conn.clamp(1, 1024)).unwrap_or(1);
        let pool = Arc::new(ConnectionPool::with_stats(
            size,
            max_streams,
            Arc::clone(&self.stats),
        ));
        pool.add(handle)?;
        let mut inner = lock(&self.inner);
        inner.insert(
            run_id.to_string(),
            Arc::new(SessionPool {
                pool: Arc::clone(&pool),
                tenant_id: tenant_id.to_string(),
                client_id: client_id.to_string(),
                cancel: CancellationToken::new(),
            }),
        );
        Ok(pool)
    }

    /// 数据连接认证后加入主会话的池；返回池、连接编号与会话取消令牌。
    pub fn add_data(
        &self,
        session_id: &str,
        tenant_id: &str,
        client_id: &str,
        handle: OpenHandle,
    ) -> Result<(Arc<ConnectionPool>, PoolConnId, CancellationToken), PoolError> {
        let session = lock(&self.inner)
            .get(session_id)
            .cloned()
            .ok_or(PoolError::Closed)?;
        if session.tenant_id != tenant_id || session.client_id != client_id {
            return Err(PoolError::Closed);
        }
        let id = session.pool.add(handle)?;
        Ok((Arc::clone(&session.pool), id, session.cancel.clone()))
    }

    /// 移除主会话并取消其数据连接。
    pub fn remove(&self, run_id: &str) {
        if let Some(session) = lock(&self.inner).remove(run_id) {
            session.cancel.cancel();
        }
    }

    #[must_use]
    pub fn get(&self, run_id: &str) -> Option<Arc<ConnectionPool>> {
        lock(&self.inner)
            .get(run_id)
            .map(|session| Arc::clone(&session.pool))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::compat::TokioAsyncReadCompatExt;

    fn handle() -> (OpenHandle, meatus_core::Connection, tokio::io::DuplexStream) {
        let (a, b) = tokio::io::duplex(1024);
        let connection = meatus_core::transport::attach(a.compat(), meatus_core::Role::Client);
        let open = connection.open_handle();
        (open, connection, b)
    }

    #[tokio::test]
    async fn data_connections_are_scoped_to_session_identity() {
        let pools = SessionPools::default();
        let (open, _conn, _peer) = handle();
        let pool = pools
            .create("run-1", "tenant-a", "client-a", open, 4, 64)
            .expect("create");
        assert_eq!(pool.len(), 1);

        let (open2, _conn2, _peer2) = handle();
        let err = match pools.add_data("run-1", "tenant-b", "client-a", open2) {
            Err(err) => err,
            Ok(_) => panic!("tenant mismatch must be rejected"),
        };
        assert_eq!(err, PoolError::Closed);

        let (open3, _conn3, _peer3) = handle();
        let (_pool, id, _token) = pools
            .add_data("run-1", "tenant-a", "client-a", open3)
            .expect("add data");
        assert_eq!(pools.get("run-1").expect("pool").len(), 2);
        pools.get("run-1").expect("pool").remove(id);

        pools.remove("run-1");
        assert!(pools.get("run-1").is_none());
    }
}
