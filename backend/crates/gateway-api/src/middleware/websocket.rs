//! 服务器握手与双工传输；自定义协议及多轮执行由接管会话的插件编排

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{
        FromRequestParts as _,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
};
use futures::{
    FutureExt as _, SinkExt as _, StreamExt as _,
    future::BoxFuture,
    stream::{SplitSink, SplitStream},
};
use gateway_core::{
    engine::middleware::{FrozenMiddlewarePlan, MiddlewareError, MiddlewareHeader},
    lifecycle::CancellationToken,
    middleware::{
        compose,
        http::{self as http, upgrade},
        websocket as core,
    },
};
use http_body_util::BodyExt as _;

pub(super) struct Upgrade {
    pub(super) context: http::Context,
}

impl upgrade::WebSocketUpgrade for Upgrade {
    fn accept(
        &self,
        mut parts: ::http::request::Parts,
        protocols: Vec<String>,
    ) -> BoxFuture<'static, Result<(http::Response, upgrade::PendingSession), MiddlewareError>>
    {
        let context = self.context.clone();
        Box::pin(async move {
            let websocket = WebSocketUpgrade::from_request_parts(&mut parts, &())
                .await
                .map_err(|_| MiddlewareError::InvalidState)?;
            let headers = parts
                .headers
                .iter()
                .map(|(name, value)| {
                    MiddlewareHeader::new(
                        name.as_str(),
                        bytes::Bytes::copy_from_slice(value.as_bytes()),
                    )
                })
                .collect::<Vec<_>>()
                .into();
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let response = websocket
                .protocols(protocols)
                .max_message_size(usize::MAX)
                .max_frame_size(usize::MAX)
                .on_upgrade(move |socket| async move {
                    let _ = sender.send(socket);
                });
            let pending = async move {
                let socket = tokio::select! {
                    biased;
                    () = context.cancellation.cancelled() => return Err(Arc::new(MiddlewareError::Fault)),
                    socket = receiver => socket.map_err(|_| Arc::new(MiddlewareError::Fault))?,
                };
                let (writer, reader) = socket.split();
                let writer = Arc::new(Writer { socket: tokio::sync::Mutex::new(writer), cancellation: context.cancellation.clone() });
                let (incoming, received) = tokio::sync::mpsc::channel(1);
                let cancellation = context.cancellation.clone();
                let reader_task = tokio::spawn(read_messages(reader, incoming, cancellation));
                Ok(Arc::new(Session {
                    reader: tokio::sync::Mutex::new(received), reader_task: reader_task.abort_handle(), writer,
                    plan: context.plan, connection_id: context.request_id, headers,
                    cancellation: context.cancellation,
                }) as Arc<dyn core::Session>)
            }.boxed().shared();
            Ok((
                response
                    .map(|body: Body| body.map_err(|error| Box::new(error) as _).boxed_unsync()),
                pending,
            ))
        })
    }
}

/// 原生协议与插件接管会话共用消息链；调用取消不等待插件释放 transport 锁
pub(crate) async fn transform(
    context: core::Context,
    message: core::Message,
) -> Result<Option<core::Message>, MiddlewareError> {
    let Some(plan) = context
        .plan
        .as_ref()
        .filter(|plan| plan.has_websocket())
        .cloned()
    else {
        return Ok(Some(message));
    };
    let cancellation = context.cancellation.clone();
    let next = compose(Vec::new(), |message| Box::pin(async { Ok(Some(message)) }));
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(MiddlewareError::Fault),
        result = plan.handle_websocket(context, message, next) => result,
    }
}

struct Session {
    reader: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Message>>,
    reader_task: tokio::task::AbortHandle,
    writer: Arc<Writer>,
    plan: Option<FrozenMiddlewarePlan>,
    connection_id: String,
    headers: Arc<[MiddlewareHeader]>,
    cancellation: CancellationToken,
}

impl Session {
    async fn transform(
        &self,
        message: core::Message,
        direction: core::Direction,
    ) -> Result<Option<core::Message>, MiddlewareError> {
        transform(
            core::Context {
                plan: self.plan.clone(),
                connection_id: self.connection_id.clone(),
                direction,
                headers: self.headers.clone(),
                cancellation: self.cancellation.clone(),
                sender: self.writer.clone(),
            },
            message,
        )
        .await
    }
}

impl core::Session for Session {
    fn receive(&self) -> BoxFuture<'_, Result<Option<core::Message>, MiddlewareError>> {
        Box::pin(async move {
            loop {
                let message = tokio::select! {
                    biased;
                    () = self.cancellation.cancelled() => return Err(MiddlewareError::Fault),
                    message = async { self.reader.lock().await.recv().await } => message,
                };
                let Some(message) = message else {
                    return Ok(None);
                };
                let message = into_core(message);
                // transport 锁只保护一次读取；插件可以在收包处理中主动发送
                if let Some(message) = self.transform(message, core::Direction::Incoming).await? {
                    return Ok(Some(message));
                }
            }
        })
    }
}
impl core::Sender for Session {
    fn send(&self, message: core::Message) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            if let Some(message) = self.transform(message, core::Direction::Outgoing).await? {
                self.writer.send(message).await?;
            }
            Ok(())
        })
    }
}

struct Writer {
    socket: tokio::sync::Mutex<SplitSink<WebSocket, Message>>,
    cancellation: CancellationToken,
}
impl core::Sender for Writer {
    fn send(&self, message: core::Message) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            let message = from_core(message)?;
            tokio::select! {
                biased;
                () = self.cancellation.cancelled() => Err(MiddlewareError::Fault),
                result = async {
                    let mut socket = self.socket.lock().await;
                    let mut guard = CancelOnDrop(Some(self.cancellation.clone()));
                    socket.send(message).await.map_err(|_| MiddlewareError::Fault)?;
                    guard.0.take();
                    Ok(())
                } => result,
            }
        })
    }
}
pub(crate) struct CancelOnDrop(pub(crate) Option<CancellationToken>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancellation) = &self.0 {
            cancellation.cancel();
        }
    }
}

pub(crate) fn into_core(message: Message) -> core::Message {
    let (kind, payload) = match message {
        Message::Text(data) => (core::Kind::Text, data.into()),
        Message::Binary(data) => (core::Kind::Binary, data),
        Message::Ping(data) => (core::Kind::Ping, data),
        Message::Pong(data) => (core::Kind::Pong, data),
        Message::Close(Some(frame)) => (
            core::Kind::Close {
                code: Some(frame.code),
            },
            frame.reason.into(),
        ),
        Message::Close(None) => (core::Kind::Close { code: None }, bytes::Bytes::new()),
    };
    core::Message { kind, payload }
}
pub(crate) fn from_core(message: core::Message) -> Result<Message, MiddlewareError> {
    Ok(match message.kind {
        core::Kind::Text => Message::Text(
            message
                .payload
                .try_into()
                .map_err(|_| MiddlewareError::InvalidState)?,
        ),
        core::Kind::Binary => Message::Binary(message.payload),
        core::Kind::Ping if message.payload.len() <= 125 => Message::Ping(message.payload),
        core::Kind::Pong if message.payload.len() <= 125 => Message::Pong(message.payload),
        core::Kind::Close { code: Some(code) } if message.payload.len() <= 123 => {
            Message::Close(Some(CloseFrame {
                code,
                reason: message
                    .payload
                    .try_into()
                    .map_err(|_| MiddlewareError::InvalidState)?,
            }))
        }
        core::Kind::Close { code: None } if message.payload.is_empty() => Message::Close(None),
        _ => return Err(MiddlewareError::InvalidState),
    })
}

impl Drop for Session {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

async fn read_messages(
    mut socket: SplitStream<WebSocket>,
    incoming: tokio::sync::mpsc::Sender<Message>,
    cancellation: CancellationToken,
) {
    let _guard = CancelOnDrop(Some(cancellation.clone()));
    loop {
        let message = tokio::select! {
            biased;
            () = cancellation.cancelled() => return,
            message = socket.next() => message,
        };
        let Some(Ok(message)) = message else {
            return;
        };
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return,
            result = incoming.send(message) => if result.is_err() { return; },
        }
    }
}
