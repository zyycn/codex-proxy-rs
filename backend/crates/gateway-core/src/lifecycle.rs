//! Host 与 API 之间的连接注册、drain 与取消契约。

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::channel::oneshot;

struct CancellationState {
    cancelled: AtomicBool,
    waiters: Mutex<Vec<oneshot::Sender<()>>>,
}

/// 可克隆的请求、任务与连接取消信号。
#[derive(Clone)]
pub struct CancellationToken(Arc<CancellationState>);

impl fmt::Debug for CancellationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(CancellationState {
            cancelled: AtomicBool::new(false),
            waiters: Mutex::new(Vec::new()),
        }))
    }

    pub fn cancel(&self) {
        if self.0.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        let waiters = {
            let mut guard = lock_unpoisoned(&self.0.waiters);
            std::mem::take(&mut *guard)
        };
        for waiter in waiters {
            let _ = waiter.send(());
        }
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let (sender, receiver) = oneshot::channel();
        {
            let mut waiters = lock_unpoisoned(&self.0.waiters);
            if self.is_cancelled() {
                return;
            }
            waiters.push(sender);
        }
        let _ = receiver.await;
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 进程已进入 drain，新连接不得再注册。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionDraining;

impl fmt::Display for ConnectionDraining {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("connection lifecycle is draining")
    }
}

impl std::error::Error for ConnectionDraining {}

/// 一次成功的活跃连接注册。
///
/// 实现必须在 guard `Drop` 时原子减少活跃连接计数。
pub trait ConnectionGuard: Send + 'static {}

/// API 消费、Host 实现的进程连接生命周期。
pub trait ConnectionLifecycle: Send + Sync {
    /// 原子地检查 drain 状态并注册一个活跃连接。
    ///
    /// 当本方法成功时，drain 必须等待返回的 guard 被释放；
    /// 当 drain 已经线性化生效时，本方法必须返回 [`ConnectionDraining`]。
    fn try_register(&self) -> Result<Box<dyn ConnectionGuard>, ConnectionDraining>;

    fn cancellation(&self) -> CancellationToken;

    fn is_draining(&self) -> bool;
}
