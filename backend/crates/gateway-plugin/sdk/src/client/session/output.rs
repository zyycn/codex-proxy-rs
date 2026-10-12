//! 会话输出队列、控制帧预留与唯一串行写泵

use std::sync::Arc;

use tokio::{
    io::AsyncWrite,
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
};

use crate::Frame;

use super::super::frame::{validate_frame, write_frame};
use super::{CallCancellation, CallDeadline, SessionError};

pub(super) struct Output {
    pub(super) sender: mpsc::Sender<Outbound>,
    pub(super) data_slots: Arc<Semaphore>,
}

impl Clone for Output {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            data_slots: Arc::clone(&self.data_slots),
        }
    }
}

impl Output {
    pub(super) async fn send_data(&self, frame: Frame) -> Result<(), SessionError> {
        validate_frame(&frame)?;
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
        frame: Frame,
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
        validate_frame(&frame)?;
        self.sender
            .try_send(Outbound {
                frame,
                _data_slot: None,
            })
            .map_err(|_| SessionError::Capacity)
    }
}

pub(super) struct Outbound {
    frame: Frame,
    _data_slot: Option<OwnedSemaphorePermit>,
}

pub(super) async fn writer_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut received: mpsc::Receiver<Outbound>,
) -> Result<(), SessionError> {
    while let Some(outbound) = received.recv().await {
        write_frame(&mut writer, &outbound.frame).await?;
    }
    Ok(())
}
