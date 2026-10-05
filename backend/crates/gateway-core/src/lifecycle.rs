//! 请求与连接的截止、租约、取消和 drain 契约

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use event_listener::Event;
use futures::future::{BoxFuture, pending};
use futures_timer::Delay;

struct CancellationState {
    cancelled: AtomicBool,
    event: Event,
}

/// 可克隆的请求、任务与连接取消信号
#[derive(Clone)]
pub struct CancellationToken {
    state: Arc<CancellationState>,
    ancestors: Arc<[Arc<CancellationState>]>,
}

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
        Self {
            state: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
                event: Event::new(),
            }),
            ancestors: Arc::from([]),
        }
    }

    /// 创建只从父级继承取消的子 token；取消子级不会反向取消父请求
    #[must_use]
    pub fn child_token(&self) -> Self {
        let ancestors = std::iter::once(Arc::clone(&self.state))
            .chain(self.ancestors.iter().cloned())
            .collect::<Vec<_>>();
        Self {
            state: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
                event: Event::new(),
            }),
            ancestors: ancestors.into(),
        }
    }

    pub fn cancel(&self) {
        if self.state.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        self.state.event.notify(usize::MAX);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
            || self
                .ancestors
                .iter()
                .any(|state| state.cancelled.load(Ordering::Acquire))
    }

    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut waiters: Vec<BoxFuture<'static, ()>> = Vec::with_capacity(1 + self.ancestors.len());
        waiters.push(wait_for_state(Arc::clone(&self.state)));
        waiters.extend(self.ancestors.iter().cloned().map(wait_for_state));
        let _ = futures::future::select_all(waiters).await;
    }
}

fn wait_for_state(state: Arc<CancellationState>) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        if state.cancelled.load(Ordering::Acquire) {
            return;
        }
        // listener 随等待 future 丢弃而注销，避免 select 败选分支在自身及祖先状态中积累
        let listener = state.event.listen();
        // 注册后复查，覆盖取消发生在首次检查与注册之间的竞态
        if state.cancelled.load(Ordering::Acquire) {
            return;
        }
        listener.await;
    })
}

/// 进程已进入 drain，新连接不得再注册
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionDraining;

impl fmt::Display for ConnectionDraining {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("connection lifecycle is draining")
    }
}

impl std::error::Error for ConnectionDraining {}

/// 一次成功的活跃连接注册
///
/// 实现必须在 guard `Drop` 时原子减少活跃连接计数
pub trait ConnectionGuard: Send + 'static {}

/// API 消费、Host 实现的进程连接生命周期
pub trait ConnectionLifecycle: Send + Sync {
    /// 原子地检查 drain 状态并注册一个活跃连接
    ///
    /// 当本方法成功时，drain 必须等待返回的 guard 被释放；
    /// 当 drain 已经线性化生效时，本方法必须返回 [`ConnectionDraining`]
    fn try_register(&self) -> Result<Box<dyn ConnectionGuard>, ConnectionDraining>;

    fn cancellation(&self) -> CancellationToken;

    fn is_draining(&self) -> bool;
}

/// 请求默认没有总时长限制；显式截止与用于异常回收的可续期租约分开
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Deadline(Option<SystemTime>);

/// 租约失去续期后有界回收，不作为模型请求的总执行预算
pub const REQUEST_LEASE_TTL: Duration = Duration::from_secs(10 * 60);

impl Deadline {
    pub fn from_timeout(started_at: SystemTime, timeout: Option<Duration>) -> Option<Self> {
        match timeout {
            Some(timeout) => started_at.checked_add(timeout).map(|at| Self(Some(at))),
            None => Some(Self(None)),
        }
    }

    #[must_use]
    pub const fn at(self) -> Option<SystemTime> {
        self.0
    }

    #[must_use]
    pub fn remaining(self) -> Option<Duration> {
        self.0
            .map(|at| at.duration_since(SystemTime::now()).unwrap_or_default())
    }

    #[must_use]
    pub fn bounded(self, maximum: Duration) -> Duration {
        self.remaining()
            .map_or(maximum, |remaining| remaining.min(maximum))
    }

    #[must_use]
    pub fn is_elapsed(self) -> bool {
        self.remaining()
            .is_some_and(|remaining| remaining.is_zero())
    }

    #[must_use]
    pub fn min(self, other: SystemTime) -> Self {
        Self(Some(self.0.map_or(other, |at| at.min(other))))
    }

    #[must_use]
    pub fn lease_deadline(self) -> SystemTime {
        let expires = SystemTime::now() + REQUEST_LEASE_TTL;
        self.0.map_or(expires, |at| at.min(expires))
    }

    pub fn wait(self) -> BoxFuture<'static, ()> {
        match self.remaining() {
            Some(remaining) => Box::pin(Delay::new(remaining)),
            None => Box::pin(pending()),
        }
    }
}

impl From<SystemTime> for Deadline {
    fn from(at: SystemTime) -> Self {
        Self(Some(at))
    }
}

/// 持有者释放即停止续期，具体执行器由 Store 提供
pub trait LeaseGuard: Send + Sync {}
impl<T: Send + Sync> LeaseGuard for T {}
