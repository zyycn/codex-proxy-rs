//! 受管 WebSocket 的握手、消息收发、超时与连接关闭

use std::time::Duration;

use bytes::Bytes;
use futures::{
    SinkExt as _, StreamExt as _,
    stream::{SplitSink, SplitStream},
};
use gateway_core::account::OutboundProxy;
use hyper_util::rt::TokioIo;
use tokio::sync::{Mutex, OwnedSemaphorePermit, watch};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        handshake::{client::generate_key, derive_accept_key},
        protocol::{Role, WebSocketConfig},
    },
};

use super::{HttpBody, HttpClient, HttpError, HttpErrorKind, HttpRequest, NetworkPolicy};

const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

type Socket = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;

/// 读写分别串行，允许等待输出时发送控制消息；关闭会唤醒在途操作并释放容量
pub struct ManagedWebSocket {
    writer: Mutex<Option<(SplitSink<Socket, Message>, OwnedSemaphorePermit)>>,
    reader: Mutex<Option<SplitStream<Socket>>>,
    closed: watch::Sender<bool>,
}

pub enum WebSocketMessage {
    Text(String),
    Binary(Bytes),
}

pub struct WebSocketResponse {
    pub status: u16,
    pub headers: Vec<(String, Vec<u8>)>,
    pub connection: Option<ManagedWebSocket>,
    pub body: Option<HttpBody>,
}

impl HttpClient {
    /// 使用与 HTTP 相同的 DNS、代理、证书和容量边界执行 HTTP/1.1 Upgrade
    pub async fn open_websocket(
        &self,
        request: HttpRequest,
        proxy: Option<&OutboundProxy>,
        network: &NetworkPolicy,
        timeout: Duration,
    ) -> Result<WebSocketResponse, HttpError> {
        if request.method != "GET"
            || !request.body.is_empty()
            || request
                .headers
                .iter()
                .any(|(name, _)| name.to_ascii_lowercase().starts_with("sec-websocket-"))
        {
            return Err(HttpError::invalid("WebSocket request"));
        }
        let key = generate_key();
        let mut response = self
            .send(request, proxy, network, timeout, None, Some(&key))
            .await?;
        if response.status != 101 {
            let response = response.into_http();
            return Ok(WebSocketResponse {
                status: response.status,
                headers: response.headers,
                connection: None,
                body: Some(response.body),
            });
        }
        let headers = response.response.headers();
        if headers
            .get("sec-websocket-accept")
            .and_then(|value| value.to_str().ok())
            != Some(derive_accept_key(key.as_bytes()).as_str())
            || !headers
                .get("upgrade")
                .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"))
            || !headers
                .get("connection")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| {
                    value
                        .split(',')
                        .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
                })
            || headers.contains_key("sec-websocket-extensions")
            || headers.contains_key("sec-websocket-protocol")
        {
            return Err(HttpError::sent("WebSocket handshake"));
        }
        let upgraded = tokio::time::timeout_at(
            response.deadline,
            hyper::upgrade::on(&mut response.response),
        )
        .await
        .map_err(|_| timeout_error())?
        .map_err(|_| HttpError::sent("WebSocket upgrade"))?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_MESSAGE_BYTES));
        let socket =
            WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, Some(config))
                .await;
        let (writer, reader) = socket.split();
        Ok(WebSocketResponse {
            status: response.status,
            headers: response.headers,
            connection: Some(ManagedWebSocket {
                writer: Mutex::new(Some((writer, response.permit))),
                reader: Mutex::new(Some(reader)),
                closed: watch::channel(false).0,
            }),
            body: None,
        })
    }
}

impl ManagedWebSocket {
    pub async fn send(
        &self,
        message: WebSocketMessage,
        timeout: Duration,
    ) -> Result<(), HttpError> {
        let message = match message {
            WebSocketMessage::Text(text) if text.len() <= MAX_MESSAGE_BYTES => {
                Message::Text(text.into())
            }
            WebSocketMessage::Binary(bytes) if bytes.len() <= MAX_MESSAGE_BYTES => {
                Message::Binary(bytes)
            }
            _ => return Err(HttpError::invalid("WebSocket message limits")),
        };
        let mut closed = self.closed.subscribe();
        let result = tokio::select! {
            biased;
            _ = closed.wait_for(|closed| *closed) => Err(HttpError::sent("WebSocket closed")),
            result = tokio::time::timeout(timeout, async {
                let mut writer = self.writer.lock().await;
                let (writer, _) = writer.as_mut().ok_or_else(|| HttpError::invalid("WebSocket closed"))?;
                writer.send(message).await.map_err(|_| HttpError::sent("WebSocket send"))
            }) => result.unwrap_or_else(|_| Err(timeout_error())),
        };
        if result.is_err() {
            self.close().await;
        }
        result
    }

    pub async fn read(&self, timeout: Duration) -> Result<Option<WebSocketMessage>, HttpError> {
        let mut closed = self.closed.subscribe();
        let result = tokio::select! {
            biased;
            _ = closed.wait_for(|closed| *closed) => Ok(None),
            result = tokio::time::timeout(timeout, self.receive()) => result.unwrap_or_else(|_| Err(timeout_error())),
        };
        if !matches!(result, Ok(Some(_))) {
            self.close().await;
        }
        result
    }

    async fn receive(&self) -> Result<Option<WebSocketMessage>, HttpError> {
        let mut reader = self.reader.lock().await;
        let Some(reader) = reader.as_mut() else {
            return Ok(None);
        };
        loop {
            match reader.next().await {
                Some(Ok(Message::Text(text))) => {
                    return Ok(Some(WebSocketMessage::Text(text.to_string())));
                }
                Some(Ok(Message::Binary(bytes))) => {
                    return Ok(Some(WebSocketMessage::Binary(bytes)));
                }
                Some(Ok(Message::Ping(_))) => {
                    let mut writer = self.writer.lock().await;
                    let Some((writer, _)) = writer.as_mut() else {
                        return Ok(None);
                    };
                    writer
                        .flush()
                        .await
                        .map_err(|_| HttpError::sent("WebSocket heartbeat"))?;
                }
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => return Ok(None),
                Some(Ok(Message::Frame(_))) => return Err(HttpError::sent("WebSocket frame")),
                Some(Err(_)) => return Err(HttpError::sent("WebSocket receive")),
            }
        }
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    /// 先取消在途读写，再释放两端；返回时连接与容量均已回收
    pub async fn close(&self) {
        self.closed.send_replace(true);
        let mut writer = self.writer.lock().await;
        let mut reader = self.reader.lock().await;
        reader.take();
        writer.take();
    }
}

fn timeout_error() -> HttpError {
    HttpError::sent("WebSocket deadline").with_kind(HttpErrorKind::Timeout)
}
