//! 插件消息组合在 transport 锁之外执行，主动发送复用唯一 writer
use super::{ConnectionWriteError, PumpContext, write_message};
pub(super) use crate::middleware::websocket::CancelOnDrop;
use crate::middleware::websocket::{from_core, into_core};
use axum::extract::ws::Message;
use futures::{Sink, future::BoxFuture};
use gateway_core::{engine::middleware::MiddlewareError, middleware::websocket as core};
use std::sync::Arc;

pub(super) struct Writer<S> {
    socket: tokio::sync::Mutex<S>,
    context: Arc<PumpContext>,
}
impl<S, E> Writer<S>
where
    S: Sink<Message, Error = E> + Unpin + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    pub(super) fn new(socket: S, context: Arc<PumpContext>) -> Self {
        Self {
            socket: tokio::sync::Mutex::new(socket),
            context,
        }
    }
    pub(super) async fn write(&self, message: Message) -> Result<(), ConnectionWriteError> {
        // 预算包含排队与真实写入，取消不等待其他调用释放锁
        tokio::select! {
            biased;
            () = self.context.cancellation.cancelled() => Err(ConnectionWriteError::Closed),
            result = tokio::time::timeout(self.context.config.write_timeout, async {
                let mut socket = self.socket.lock().await;
                // 发送中断后不能继续复用可能含半写入帧的连接
                let mut in_flight = CancelOnDrop(Some(self.context.cancellation.clone()));
                let result = write_message(&mut *socket, message, &self.context).await;
                if result.is_ok() { in_flight.0.take(); }
                result
            }) => result.unwrap_or(Err(ConnectionWriteError::Timeout { timeout: self.context.config.write_timeout })),
        }
    }
}
impl<S, E> core::Sender for Writer<S>
where
    S: Sink<Message, Error = E> + Unpin + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    fn send(&self, message: core::Message) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            let closing = matches!(message.kind, core::Kind::Close { .. });
            self.write(from_core(message)?)
                .await
                .map_err(|_| MiddlewareError::Fault)?;
            if closing {
                self.context.cancellation.cancel();
            }
            Ok(())
        })
    }
}

pub(super) async fn transform(
    message: Message,
    direction: core::Direction,
    sender: Arc<dyn core::Sender>,
    context: &PumpContext,
) -> Result<Option<Message>, MiddlewareError> {
    let Some(plan) = &context.middleware else {
        return Ok(Some(message));
    };
    crate::middleware::websocket::transform(
        core::Context {
            plan: Some(plan.clone()),
            connection_id: context.connection_id.to_string(),
            direction,
            headers: context.headers.clone(),
            cancellation: context.cancellation.clone(),
            sender,
        },
        into_core(message),
    )
    .await?
    .map(from_core)
    .transpose()
}
