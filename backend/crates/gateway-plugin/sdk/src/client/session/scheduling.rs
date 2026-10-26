//! 父处理任务的协作式挂起、唤醒与宿主恢复授权

use std::{
    future::{Future, poll_fn},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

use crate::{Frame, Message};

use super::{CallDeadline, CallDeadlinePause, SessionError, output::Output};

/// 这里只保存协议镜像；执行名额与准入决策始终由宿主持有
pub(super) struct CallSchedule {
    id: u64,
    output: Output,
    deadline: CallDeadline,
    state: Mutex<State>,
    ready: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

struct State {
    epoch: u64,
    phase: Phase,
    pause: Option<CallDeadlinePause>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Running,
    Suspended,
    Resuming,
}

impl CallSchedule {
    pub(super) fn new(id: u64, output: Output, deadline: CallDeadline) -> Self {
        Self {
            id,
            output,
            deadline,
            state: Mutex::new(State {
                epoch: 0,
                phase: Phase::Running,
                pause: None,
            }),
            ready: AtomicBool::new(false),
            waker: Mutex::new(None),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn resumed(&self, epoch: u64) -> Result<(), SessionError> {
        let waker = {
            let mut state = self.state();
            if state.phase != Phase::Resuming || state.epoch != epoch {
                return Err(SessionError::Protocol);
            }
            state.phase = Phase::Running;
            state.pause.take();
            self.waker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }

    /// 后台工作发起应计时的回调时，不能借父任务等待 next 停表
    pub(super) fn charge_wait(&self) {
        self.state().pause.take();
    }

    pub(super) async fn drive<F: Future>(
        self: &Arc<Self>,
        future: F,
        pauses_budget: impl Fn() -> bool,
    ) -> Result<F::Output, SessionError> {
        let mut future = std::pin::pin!(future);
        let waker = Waker::from(Arc::clone(self));
        poll_fn(|cx| {
            {
                let mut state = self.state();
                *self
                    .waker
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cx.waker().clone());
                match state.phase {
                    Phase::Resuming => return Poll::Pending,
                    Phase::Suspended => {
                        if self.ready.load(Ordering::Acquire) {
                            self.output.send_control(Frame::control(Message::Resume {
                                id: self.id,
                                epoch: state.epoch,
                            }))?;
                            state.phase = Phase::Resuming;
                        }
                        return Poll::Pending;
                    }
                    Phase::Running => self.ready.store(false, Ordering::Release),
                }
            }
            let mut task_context = Context::from_waker(&waker);
            if let Poll::Ready(result) = future.as_mut().poll(&mut task_context) {
                return Poll::Ready(Ok(result));
            }
            let mut state = self.state();
            state.epoch = state.epoch.checked_add(1).ok_or(SessionError::Capacity)?;
            // 与回调共享单一 FIFO，宿主看到挂起前一定已看到本次轮询提交的回调
            self.output.send_control(Frame::control(Message::Suspend {
                id: self.id,
                epoch: state.epoch,
            }))?;
            state.phase = Phase::Suspended;
            if pauses_budget() {
                state.pause = self.deadline.pause();
            }
            if self.ready.load(Ordering::Acquire) {
                // future 可以在 poll 返回 Pending 前唤醒自己，这次唤醒也必须经过授权
                cx.waker().wake_by_ref();
            }
            Poll::Pending
        })
        .await
    }
}

impl Wake for CallSchedule {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        // 期限恢复会同步唤醒回调等待者；唤醒路径不能再次取得调度状态锁
        self.ready.store(true, Ordering::Release);
        let waker = self
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}
