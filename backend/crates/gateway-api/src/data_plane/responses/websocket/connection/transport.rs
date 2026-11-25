//! 保留 Axum 握手校验，在 WS 消息解码前记录下游真实接收进度

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use axum::{
    extract::{
        FromRequestParts,
        ws::{self, Message},
    },
    http::{HeaderValue, StatusCode, Version, header, request::Parts},
    response::{IntoResponse, Response},
};
use futures::{Sink, Stream, ready};
use hyper::upgrade::{OnUpgrade, Upgraded};
use hyper_util::rt::TokioIo;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::watch,
    time::Instant,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        self,
        protocol::{Role, WebSocketConfig},
    },
};

pub(crate) struct Upgrade {
    pending: OnUpgrade,
    key: Option<HeaderValue>,
}

impl<S: Send + Sync> FromRequestParts<S> for Upgrade {
    type Rejection = ws::rejection::WebSocketUpgradeRejection;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let pending = parts.extensions.get::<OnUpgrade>().cloned();
        // Axum 的 WebSocket 不暴露底层 I/O；只复用其握手校验，由本层接管唯一升级流
        let _validated = ws::WebSocketUpgrade::from_request_parts(parts, state).await?;
        let pending = pending.expect("Axum validated the presence of OnUpgrade");
        let key = (parts.version <= Version::HTTP_11)
            .then(|| parts.headers.get(header::SEC_WEBSOCKET_KEY).cloned())
            .flatten();
        Ok(Self { pending, key })
    }
}

impl Upgrade {
    pub(in super::super) fn on_upgrade<F, Fut>(self, callback: F) -> Response
    where
        F: FnOnce(Socket) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(async move {
            let Ok(upgraded) = self.pending.await else {
                return;
            };
            let (activity, _) = watch::channel(Instant::now());
            let io = Receiving {
                inner: TokioIo::new(upgraded),
                activity: activity.clone(),
            };
            // 与原生 Responses 升级保持一致，不引入消息或帧长度上限
            let config = WebSocketConfig::default()
                .max_message_size(None)
                .max_frame_size(None);
            let inner = WebSocketStream::from_raw_socket(io, Role::Server, Some(config)).await;
            callback(Socket { inner, activity }).await;
        });
        if let Some(key) = self.key {
            (
                StatusCode::SWITCHING_PROTOCOLS,
                [
                    (header::CONNECTION, "upgrade".to_owned()),
                    (header::UPGRADE, "websocket".to_owned()),
                    (
                        header::SEC_WEBSOCKET_ACCEPT,
                        tungstenite::handshake::derive_accept_key(key.as_bytes()),
                    ),
                ],
            )
                .into_response()
        } else {
            StatusCode::OK.into_response()
        }
    }
}

struct Receiving {
    inner: TokioIo<Upgraded>,
    activity: watch::Sender<Instant>,
}

impl AsyncRead for Receiving {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buffer);
        if buffer.filled().len() > before {
            // 完整消息尚未收齐时，客户端可能仍在发送、暂时无法读取 Ping
            self.activity.send_replace(Instant::now());
        }
        result
    }
}

impl AsyncWrite for Receiving {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buffer)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

pub(in super::super) struct Socket {
    inner: WebSocketStream<Receiving>,
    pub(super) activity: watch::Sender<Instant>,
}

impl Stream for Socket {
    type Item = Result<Message, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            let message = match ready!(Pin::new(&mut self.inner).poll_next(cx)) {
                Some(Ok(message)) => message,
                Some(Err(error)) => return Poll::Ready(Some(Err(axum::Error::new(error)))),
                None => return Poll::Ready(None),
            };
            let message = match message {
                tungstenite::Message::Text(text) => Message::Text(
                    ws::Utf8Bytes::try_from(bytes::Bytes::from(text)).map_err(axum::Error::new)?,
                ),
                tungstenite::Message::Binary(data) => Message::Binary(data),
                tungstenite::Message::Ping(data) => Message::Ping(data),
                tungstenite::Message::Pong(data) => Message::Pong(data),
                tungstenite::Message::Close(frame) => {
                    Message::Close(frame.map(|frame| ws::CloseFrame {
                        code: frame.code.into(),
                        reason: frame.reason.to_string().into(),
                    }))
                }
                tungstenite::Message::Frame(_) => continue,
            };
            return Poll::Ready(Some(Ok(message)));
        }
    }
}

impl Sink<Message> for Socket {
    type Error = axum::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner)
            .poll_ready(cx)
            .map_err(axum::Error::new)
    }
    fn start_send(mut self: Pin<&mut Self>, message: Message) -> Result<(), Self::Error> {
        let message = match message {
            Message::Text(text) => tungstenite::Message::Text(
                tungstenite::Utf8Bytes::try_from(bytes::Bytes::from(text))
                    .map_err(axum::Error::new)?,
            ),
            Message::Binary(data) => tungstenite::Message::Binary(data),
            Message::Ping(data) => tungstenite::Message::Ping(data),
            Message::Pong(data) => tungstenite::Message::Pong(data),
            Message::Close(frame) => {
                tungstenite::Message::Close(frame.map(|frame| tungstenite::protocol::CloseFrame {
                    code: frame.code.into(),
                    reason: frame.reason.to_string().into(),
                }))
            }
        };
        Pin::new(&mut self.inner)
            .start_send(message)
            .map_err(axum::Error::new)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner)
            .poll_flush(cx)
            .map_err(axum::Error::new)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.inner)
            .poll_close(cx)
            .map_err(axum::Error::new)
    }
}
