//! 优雅 drain 状态机：`serving -> draining -> drained`。

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tokio_util::sync::CancellationToken;

pub const SERVING: u8 = 0;
pub const DRAINING: u8 = 1;
pub const DRAINED: u8 = 2;

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
    pub fn is_draining(&self) -> bool {
        self.phase() != SERVING
    }

    /// 进入 draining：停止 accept 新连接，存量连接继续。
    pub fn begin(&self) {
        if self.inner.phase.swap(DRAINING, Ordering::SeqCst) == SERVING {
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
        assert!(!drain.is_draining());
        drain.begin();
        assert_eq!(drain.phase(), DRAINING);
        assert!(drain.is_draining());
        drain.begin();
        assert_eq!(drain.phase(), DRAINING, "begin is idempotent");
        drain.finish();
        assert_eq!(drain.phase(), DRAINED);
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
