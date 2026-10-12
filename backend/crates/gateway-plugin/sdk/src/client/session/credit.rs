//! 响应流字节与帧信用的唯一状态和等待唤醒

use std::sync::Mutex;

use super::{CallCancellation, CallDeadline, SessionError};

#[derive(Default)]
pub(super) struct CreditWindow {
    state: Mutex<CreditState>,
    changed: tokio::sync::Notify,
}

#[derive(Default)]
struct CreditState {
    bytes: u64,
    frames: u64,
    maximum_bytes: u64,
}

impl CreditWindow {
    pub(super) fn maximum_bytes(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .maximum_bytes
    }

    pub(super) fn grant(&self, bytes: u32, frames: u32) -> Result<(), SessionError> {
        if bytes == 0 || frames == 0 {
            return Err(SessionError::Protocol);
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.bytes = state
            .bytes
            .checked_add(u64::from(bytes))
            .ok_or(SessionError::Protocol)?;
        state.frames = state
            .frames
            .checked_add(u64::from(frames))
            .ok_or(SessionError::Protocol)?;
        state.maximum_bytes = state.maximum_bytes.max(state.bytes);
        drop(state);
        // 每个调用只有一个顺序消费 Credit 的任务；notify_one 会保留许可，避免
        // grant 恰好发生在状态检查和等待注册之间时丢失唤醒
        self.changed.notify_one();
        Ok(())
    }

    pub(super) async fn initial_window(
        &self,
        deadline: &CallDeadline,
        cancellation: &CallCancellation,
    ) -> Result<u64, SessionError> {
        loop {
            let changed = self.changed.notified();
            {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.bytes > 0 && state.frames > 0 {
                    return Ok(state.maximum_bytes);
                }
            }
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(SessionError::Cancelled),
                () = deadline.wait() => return Err(SessionError::Timeout),
                () = changed => {}
            }
        }
    }

    pub(super) async fn take(
        &self,
        bytes: u64,
        deadline: &CallDeadline,
        cancellation: &CallCancellation,
    ) -> Result<(), SessionError> {
        loop {
            let changed = self.changed.notified();
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if bytes > state.maximum_bytes {
                    return Err(SessionError::Protocol);
                }
                if state.bytes >= bytes && state.frames > 0 {
                    state.bytes -= bytes;
                    state.frames -= 1;
                    return Ok(());
                }
            }
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(SessionError::Cancelled),
                () = deadline.wait() => return Err(SessionError::Timeout),
                () = changed => {}
            }
        }
    }
}
