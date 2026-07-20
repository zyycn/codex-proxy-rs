//! Host 与 API 之间的连接注册、drain 与取消契约。

use std::fmt;

use crate::engine::CancellationToken;

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
