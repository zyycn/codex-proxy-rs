//! 会话输出队列、控制帧预留与唯一串行写泵

use std::sync::Arc;

use tokio::{
    io::AsyncWrite,
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
};

use crate::Frame;

use super::super::frame::EncodedFrame;
use super::{CallCancellation, CallDeadline, SessionError};

pub(super) struct Output {
    pub(super) sender: mpsc::Sender<Outbound>,
    pub(super) data_slots: Arc<Semaphore>,
    pub(super) failed: watch::Sender<bool>,
}

impl Clone for Output {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            data_slots: Arc::clone(&self.data_slots),
            failed: self.failed.clone(),
        }
    }
}

impl Output {
    pub(super) async fn send_data(&self, frame: EncodedFrame) -> Result<(), SessionError> {
        let permit = Arc::clone(&self.data_slots)
            .acquire_owned()
            .await
            .map_err(|_| SessionError::Closed)?;
        self.sender
            .send(Outbound {
                frame,
                _data_slot: Some(permit),
            })
            .await
            .map_err(|_| SessionError::Closed)
    }

    pub(super) async fn send_data_until(
        &self,
        frame: EncodedFrame,
        deadline: &CallDeadline,
        cancellation: &CallCancellation,
    ) -> Result<(), SessionError> {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(SessionError::Cancelled),
            () = deadline.wait() => Err(SessionError::Timeout),
            result = self.send_data(frame) => result,
        }
    }

    pub(super) fn send_control(&self, frame: Frame) -> Result<(), SessionError> {
        self.send_encoded_control(EncodedFrame::new(frame)?)
    }

    pub(super) fn send_encoded_control(&self, frame: EncodedFrame) -> Result<(), SessionError> {
        self.sender
            .try_send(Outbound {
                frame,
                _data_slot: None,
            })
            .map_err(|_| {
                // drop 路径也会提交资源确认，队列耗尽不能静默丢失生命周期消息
                self.failed.send_replace(true);
                SessionError::Capacity
            })
    }

    pub(super) async fn failed(&self) {
        let mut failed = self.failed.subscribe();
        let _ = failed.wait_for(|failed| *failed).await;
    }
}

pub(super) struct Outbound {
    frame: EncodedFrame,
    _data_slot: Option<OwnedSemaphorePermit>,
}

pub(super) async fn writer_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut received: mpsc::Receiver<Outbound>,
) -> Result<(), SessionError> {
    while let Some(outbound) = received.recv().await {
        outbound.frame.write(&mut writer).await?;
    }
    Ok(())
}
