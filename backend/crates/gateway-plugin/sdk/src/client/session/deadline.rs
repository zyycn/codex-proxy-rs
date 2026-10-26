//! 宿主与插件共用的调用处理预算，受管续体等待期间暂停并保留剩余额度

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{sync::Notify, time::Instant};

/// 同一次调用的所有等待点共享此期限，避免回调恢复后仍使用旧截止时间
#[derive(Clone)]
pub struct CallDeadline {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    changed: Notify,
}

#[derive(Clone, Copy)]
enum State {
    Running(Instant),
    Paused(Duration),
    Unlimited,
}

/// 随受管续体释放而恢复计时；取消和错误出口同样归还暂停状态
pub struct CallDeadlinePause {
    deadline: CallDeadline,
}

impl CallDeadline {
    #[must_use]
    pub fn new(deadline: Instant) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::Running(deadline)),
                changed: Notify::new(),
            }),
        }
    }

    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        match *self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            State::Running(at) => Some(at.saturating_duration_since(Instant::now())),
            State::Paused(remaining) => Some(remaining),
            State::Unlimited => None,
        }
    }

    #[must_use]
    pub fn is_elapsed(&self) -> bool {
        self.remaining()
            .is_some_and(|remaining| remaining.is_zero())
    }

    /// 只有尚未耗尽的运行预算可以暂停；重复 next 不能重置或复活预算
    #[must_use]
    pub fn pause(&self) -> Option<CallDeadlinePause> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let State::Running(at) = *state else {
            return None;
        };
        let remaining = at.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        *state = State::Paused(remaining);
        // 暂停只延后期限，无需唤醒处理任务；旧计时器到期会重新核对状态
        // 否则父任务会把自身停表通知误当成业务唤醒，反复请求恢复
        Some(CallDeadlinePause {
            deadline: self.clone(),
        })
    }

    /// 初始资源流响应已交付，后续期限由资源 owner 持有
    pub fn clear(&self) {
        *self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = State::Unlimited;
        self.shared.changed.notify_waiters();
    }

    pub async fn wait(&self) {
        loop {
            let changed = self.shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let current = *self
                .shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let elapsed = async {
                match current {
                    State::Running(at) => tokio::time::sleep_until(at).await,
                    State::Paused(_) | State::Unlimited => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                () = changed => {}
                () = elapsed => {
                    // 暂停可与旧计时器唤醒并发，返回前重新核对当前预算
                    if self.is_elapsed() {
                        return;
                    }
                }
            }
        }
    }
}

impl Drop for CallDeadlinePause {
    fn drop(&mut self) {
        let mut state = self
            .deadline
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let State::Paused(remaining) = *state {
            *state = State::Running(Instant::now() + remaining);
            drop(state);
            self.deadline.shared.changed.notify_waiters();
        }
    }
}
