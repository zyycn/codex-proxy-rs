//! 宿主回调客户端、关联编号与有界退休回调注册表

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
};

use serde_json::Value;
use tokio::sync::oneshot;

use crate::{Frame, Message, call::middleware::NEXT_METHOD};

use super::super::frame::EncodedFrame;
use super::{
    CallCancellation, CallDeadline, SessionError, credit::CreditWindow, output::Output,
    scheduling::CallSchedule,
};

/// 宿主回调的成功结果；不实现 `Debug`，避免载荷进入普通诊断
pub struct HostReply {
    pub result: Value,
    pub payload: Vec<u8>,
}

/// 绑定父调用、期限和取消信号的宿主回调客户端
#[derive(Clone)]
pub struct HostClient {
    pub(super) parent_id: u64,
    pub(super) deadline: CallDeadline,
    pub(super) cancellation: CallCancellation,
    pub(super) callbacks: Arc<CallbackRegistry>,
    pub(super) output: Output,
    pub(super) maximum_stream_chunk_bytes: usize,
    pub(super) credits: Arc<CreditWindow>,
    pub(super) schedule: Arc<CallSchedule>,
}

impl HostClient {
    /// 读取父调用剩余处理预算，包含 next 暂停；资源流接管寿命后返回 None
    /// 外部请求期限、网络操作期限和取消信号仍独立生效
    #[must_use]
    pub fn remaining_budget(&self) -> Option<std::time::Duration> {
        self.deadline.remaining()
    }

    pub(crate) fn maximum_stream_chunk_bytes(&self) -> usize {
        let window = self.credits.maximum_bytes();
        if window == 0 {
            self.maximum_stream_chunk_bytes
        } else {
            self.maximum_stream_chunk_bytes
                .min(usize::try_from(window).unwrap_or(usize::MAX))
        }
    }

    /// 发起一次与父调用关联的宿主回调
    ///
    /// # Errors
    ///
    /// 方法或帧无效、容量耗尽、父调用取消、期限到达、连接关闭或宿主返回错误时失败
    pub async fn call(
        &self,
        method: impl Into<String>,
        params: Value,
        payload: Vec<u8>,
    ) -> Result<HostReply, SessionError> {
        let method = method.into();
        if method.is_empty() || method.len() > 128 {
            return Err(SessionError::Protocol);
        }
        let ordered = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => return Err(SessionError::Cancelled),
            () = self.deadline.wait() => return Err(SessionError::Timeout),
            ordered = self.callbacks.order.lock() => ordered,
        };
        let managed_next = method == NEXT_METHOD;
        let (id, received) = self.callbacks.begin(self.parent_id, managed_next)?;
        let mut guard = CallbackGuard {
            id,
            parent_id: self.parent_id,
            callbacks: &self.callbacks,
            output: &self.output,
            sent: false,
        };
        if !managed_next {
            self.schedule.charge_wait();
        }
        let frame = Frame {
            message: Message::Callback {
                id,
                parent_id: self.parent_id,
                method,
                params,
            },
            payload,
        };
        let frame = EncodedFrame::new(frame).map_err(|_| SessionError::Protocol)?;
        let sent = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => Err(SessionError::Cancelled),
            () = self.deadline.wait() => Err(SessionError::Timeout),
            result = self.output.send_data(frame) => result,
        };
        sent?;
        guard.sent = true;
        self.callbacks.mark_sent(id);
        drop(ordered);
        tokio::select! {
            biased;
            () = self.cancellation.cancelled() => {
                Err(SessionError::Cancelled)
            }
            () = self.deadline.wait() => {
                Err(SessionError::Timeout)
            }
            result = received => result.map_err(|_| SessionError::Closed)?,
        }
    }
}

pub(super) struct CallbackRegistry {
    state: Mutex<CallbackState>,
    order: tokio::sync::Mutex<()>,
    maximum: usize,
    maximum_retired: usize,
}

struct CallbackState {
    next_id: u64,
    pending: BTreeMap<u64, PendingCallback>,
    retired: BTreeSet<u64>,
    retired_order: VecDeque<u64>,
}

struct PendingCallback {
    parent_id: u64,
    managed_next: bool,
    sent: bool,
    result: Option<oneshot::Sender<Result<HostReply, SessionError>>>,
}

struct CallbackGuard<'a> {
    id: u64,
    parent_id: u64,
    callbacks: &'a CallbackRegistry,
    output: &'a Output,
    sent: bool,
}

impl Drop for CallbackGuard<'_> {
    fn drop(&mut self) {
        if self.callbacks.retire(self.id) && self.sent {
            let _ = self
                .output
                .send_control(Frame::control(Message::CallbackConsumed {
                    id: self.id,
                    parent_id: self.parent_id,
                }));
        }
    }
}

impl CallbackRegistry {
    pub(super) fn new(maximum: usize) -> Self {
        Self {
            state: Mutex::new(CallbackState {
                next_id: 2,
                pending: BTreeMap::new(),
                retired: BTreeSet::new(),
                retired_order: VecDeque::new(),
            }),
            order: tokio::sync::Mutex::new(()),
            maximum,
            maximum_retired: maximum.saturating_mul(2),
        }
    }

    fn begin(
        &self,
        parent_id: u64,
        managed_next: bool,
    ) -> Result<(u64, oneshot::Receiver<Result<HostReply, SessionError>>), SessionError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.pending.len() >= self.maximum {
            return Err(SessionError::Capacity);
        }
        let id = state.next_id;
        state.next_id = state.next_id.checked_add(2).ok_or(SessionError::Capacity)?;
        let (result, received) = oneshot::channel();
        state.pending.insert(
            id,
            PendingCallback {
                parent_id,
                managed_next,
                sent: false,
                result: Some(result),
            },
        );
        Ok((id, received))
    }

    fn mark_sent(&self, id: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(callback) = state.pending.get_mut(&id) {
            callback.sent = true;
        }
    }

    pub(super) fn pauses_budget(&self, parent_id: u64) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut has_next = false;
        for callback in state
            .pending
            .values()
            .filter(|call| call.parent_id == parent_id)
        {
            if !callback.managed_next || !callback.sent {
                return false;
            }
            has_next = true;
        }
        has_next
    }

    pub(super) fn resolve(&self, frame: Frame) -> Result<(), SessionError> {
        let id = match &frame.message {
            Message::Result { id, .. } | Message::Error { id, .. } => *id,
            _ => return Err(SessionError::Protocol),
        };
        let sender = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(pending) = state.pending.get_mut(&id) {
                Some(pending.result.take().ok_or(SessionError::Protocol)?)
            } else if state.retired.remove(&id) {
                state.retired_order.retain(|retired| *retired != id);
                None
            } else {
                return Err(SessionError::Protocol);
            }
        };
        let Some(sender) = sender else {
            return Ok(());
        };
        let result = match frame.message {
            Message::Result { result, .. } => Ok(HostReply {
                result,
                payload: frame.payload,
            }),
            Message::Error { error, .. } if frame.payload.is_empty() => {
                Err(SessionError::Remote(error))
            }
            _ => return Err(SessionError::Protocol),
        };
        let _ = sender.send(result);
        Ok(())
    }

    fn retire(&self, id: u64) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pending) = state.pending.remove(&id) {
            if pending.result.is_some() {
                retire_callback(&mut state, id, self.maximum_retired);
            }
            return true;
        }
        false
    }

    pub(super) fn finish_parent(&self, parent_id: u64) {
        let pending = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut remaining = BTreeMap::new();
            let mut finished = Vec::new();
            for (id, pending) in std::mem::take(&mut state.pending) {
                if pending.parent_id == parent_id {
                    if pending.result.is_some() {
                        retire_callback(&mut state, id, self.maximum_retired);
                    }
                    finished.push(pending);
                } else {
                    remaining.insert(id, pending);
                }
            }
            state.pending = remaining;
            finished
        };
        for pending in pending {
            if let Some(result) = pending.result {
                let _ = result.send(Err(SessionError::Cancelled));
            }
        }
    }

    pub(super) fn close(&self) {
        let pending = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut state.pending)
        };
        for pending in pending.into_values() {
            if let Some(result) = pending.result {
                let _ = result.send(Err(SessionError::Closed));
            }
        }
    }
}

fn retire_callback(state: &mut CallbackState, id: u64, maximum: usize) {
    if state.retired.insert(id) {
        state.retired_order.push_back(id);
    }
    while state.retired_order.len() > maximum {
        if let Some(expired) = state.retired_order.pop_front() {
            state.retired.remove(&expired);
        }
    }
}
