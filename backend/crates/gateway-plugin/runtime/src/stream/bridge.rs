//! 将插件 RPC 流消息桥接为有界接收流，并归还消费信用

use std::sync::Arc;

use tokio::{
    sync::{OwnedSemaphorePermit, mpsc, oneshot},
    time::Instant,
};

use crate::{RpcError, RpcReply, RpcSession, rpc::Shared};

use super::flow_control::ReceiveWindow;

pub(crate) struct StreamIngress {
    chunks: mpsc::Sender<Vec<u8>>,
    terminal: oneshot::Sender<Result<(), RpcError>>,
    window: ReceiveWindow,
}

impl StreamIngress {
    pub fn new(
        bytes: u32,
        frames: u32,
    ) -> (
        Self,
        mpsc::Receiver<Vec<u8>>,
        oneshot::Receiver<Result<(), RpcError>>,
    ) {
        let (chunks, received) = mpsc::channel(frames as usize);
        let (terminal, ended) = oneshot::channel();
        (
            Self {
                chunks,
                terminal,
                window: ReceiveWindow::new(bytes, frames),
            },
            received,
            ended,
        )
    }

    pub fn receive(&mut self, sequence: u64, payload: Vec<u8>) -> Result<(), RpcError> {
        self.window.receive(sequence, payload.len())?;
        self.chunks
            .try_send(payload)
            .map_err(|_| RpcError::Protocol)
    }

    pub fn release(&mut self, bytes: u32) -> Result<(), RpcError> {
        self.window.release(bytes)
    }

    pub fn finish(self, result: Result<(), RpcError>) {
        let _ = self.terminal.send(result);
    }
}

/// 消费者读取后才补充窗口；终态走独立通道，不会排在满数据队列后等待
pub struct RpcStream {
    pub initial: RpcReply,
    pub(crate) chunks: mpsc::Receiver<Vec<u8>>,
    pub(crate) terminal: Option<oneshot::Receiver<Result<(), RpcError>>>,
    pub(crate) deadline: Option<Instant>,
    pub(crate) id: u64,
    pub(crate) shared: Arc<Shared>,
    pub(crate) _slot: OwnedSemaphorePermit,
    pub(crate) _session: Arc<RpcSession>,
}

impl RpcStream {
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>, RpcError> {
        if self.terminal.is_none() {
            return Ok(None);
        }
        let result = match self.deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, self.chunks.recv()).await,
            None => Ok(self.chunks.recv().await),
        };
        match result {
            Ok(Some(chunk)) => {
                self.shared
                    .release_stream_credit(self.id, chunk.len() as u32)?;
                Ok(Some(chunk))
            }
            Ok(None) => {
                let Some(terminal) = self.terminal.take() else {
                    return Ok(None);
                };
                terminal.await.map_err(|_| RpcError::Closed)??;
                Ok(None)
            }
            Err(_) => {
                self.shared.cancel(self.id, RpcError::Timeout);
                self.terminal = None;
                Err(RpcError::Timeout)
            }
        }
    }
}

impl Drop for RpcStream {
    fn drop(&mut self) {
        if self.shared.context(self.id).is_some() {
            self.shared.cancel(self.id, RpcError::Cancelled);
        }
    }
}
