//! 宿主与插件共用的调用处理预算，受管续体等待期间暂停并保留剩余额度

use std::time::Duration;

use tokio::{sync::watch, time::Instant};

/// 同一次调用的所有等待点共享此期限，避免回调恢复后仍使用旧截止时间
#[derive(Clone)]
pub struct CallDeadline {
    state: watch::Sender<State>,
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
        let (state, _) = watch::channel(State::Running(deadline));
        Self { state }
    }

    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        match *self.state.borrow() {
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
        self.state
            .send_if_modified(|state| {
                let State::Running(at) = *state else {
                    return false;
                };
                let remaining = at.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return false;
                }
                *state = State::Paused(remaining);
                true
            })
            .then(|| CallDeadlinePause {
                deadline: self.clone(),
            })
    }

    /// 初始资源流响应已交付，后续期限由资源 owner 持有
    pub fn clear(&self) {
        self.state.send_replace(State::Unlimited);
    }

    pub async fn wait(&self) {
        let mut state = self.state.subscribe();
        loop {
            let current = *state.borrow_and_update();
            let elapsed = async {
                match current {
                    State::Running(at) => tokio::time::sleep_until(at).await,
                    State::Paused(_) | State::Unlimited => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                _ = state.changed() => {}
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
        self.deadline.state.send_if_modified(|state| {
            let State::Paused(remaining) = *state else {
                return false;
            };
            *state = State::Running(Instant::now() + remaining);
            true
        });
    }
}
