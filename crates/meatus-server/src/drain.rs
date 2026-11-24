//! 优雅 drain 状态机：`serving -> preparing -> draining -> drained`。
//!
//! `preparing` 只摘除 readiness，仍接受新连接，给外部 LB 留出摘后端的时间；
//! 宽限期结束后进入 `draining`，停止 accept 并等待存量流清空。

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tokio_util::sync::CancellationToken;

pub const SERVING: u8 = 0;
pub const DRAINING: u8 = 1;
pub const DRAINED: u8 = 2;
pub const PREPARING: u8 = 3;

#[derive(Clone, Debug, Default)]
pub struct DrainState {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    phase: AtomicU8,
    stop_accept: CancellationToken,
}

impl DrainState {
    #[must_use]
    pub fn phase(&self) -> u8 {
        self.inner.phase.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.phase() == SERVING
    }

    #[must_use]
    pub fn is_preparing(&self) -> bool {
        self.phase() == PREPARING
    }

    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.phase() != SERVING
    }

    /// 摘除 readiness，但不停止 accept；供 LB 在宽限期内完成摘后端。
    pub fn pre_drain(&self) {
        if self.phase() == SERVING {
            self.inner.phase.store(PREPARING, Ordering::SeqCst);
        }
    }

    /// 进入 draining：停止 accept 新连接，存量连接继续。
    pub fn begin(&self) {
        let previous = self.inner.phase.swap(DRAINING, Ordering::SeqCst);
        if previous == SERVING || previous == PREPARING {
            self.inner.stop_accept.cancel();
        }
    }

    /// 进入 drained：存量已清空或超时，可安全退出。
    pub fn finish(&self) {
        self.inner.phase.store(DRAINED, Ordering::SeqCst);
        self.inner.stop_accept.cancel();
    }

    /// accept 循环用；drain 开始后立即就绪。
    pub async fn stopped_accepting(&self) {
        self.inner.stop_accept.cancelled().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_serving_draining_drained() {
        let drain = DrainState::default();
        assert_eq!(drain.phase(), SERVING);
        assert!(drain.is_ready());
        assert!(!drain.is_draining());
        drain.begin();
        assert_eq!(drain.phase(), DRAINING);
        assert!(drain.is_draining());
        drain.begin();
        assert_eq!(drain.phase(), DRAINING, "begin is idempotent");
        drain.finish();
        assert_eq!(drain.phase(), DRAINED);
    }

    #[test]
    fn pre_drain_withdraws_readiness_without_stopping_accept() {
        let drain = DrainState::default();
        drain.pre_drain();
        assert_eq!(drain.phase(), PREPARING);
        assert!(drain.is_preparing());
        assert!(!drain.is_ready());
        assert_eq!(drain.phase(), PREPARING, "pre_drain is idempotent");
    }

    #[tokio::test]
    async fn pre_drain_keeps_accept_loop_alive_until_begin() {
        let drain = DrainState::default();
        let waiter = tokio::spawn({
            let drain = drain.clone();
            async move { drain.stopped_accepting().await }
        });
        drain.pre_drain();
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished(), "pre_drain must not stop accept");
        drain.begin();
        waiter.await.expect("drain waiter");
    }

    #[tokio::test]
    async fn begin_releases_accept_waiters() {
        let drain = DrainState::default();
        let waiter = tokio::spawn({
            let drain = drain.clone();
            async move { drain.stopped_accepting().await }
        });
        tokio::task::yield_now().await;
        drain.begin();
        waiter.await.expect("drain waiter");
    }
}
