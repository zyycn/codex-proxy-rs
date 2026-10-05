//! Codex Live sideband 的账号级上游拨号与抽象帧中继。
//!
//! sideband 连接使用钉住账号的凭据直接拨 `api.openai.com` 的 realtime
//! WebSocket，并把上游帧映射为 Core 的 [`LiveFrame`]；协议 adapter 在
//! 另一侧做同样映射，两侧都不解释帧内容。

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gateway_core::live::{
    LiveClose, LiveFrame, LiveGatewayError, LiveGatewayErrorKind, LiveRelay, LiveRelaySink,
    LiveRelayStream,
};
use tokio_tungstenite::tungstenite::Message;

use super::{
    error::CodexWebSocketExchangeError, handshake::connect_sideband_websocket,
    model::CodexWebSocketConnection, pump::RawWsStream,
};

/// sideband 上游连接；原始流加协商子协议。
pub(crate) struct CodexLiveSideband {
    pub(crate) stream: RawWsStream,
    pub(crate) subprotocol: Option<String>,
}

/// 以给定 endpoint 与业务头拨号 sideband 上游。
///
/// `business_headers` 包含认证、账号与客户端协议头；标准握手字段由本层生成，子协议
/// offer 由调用方以 `sec-websocket-protocol` 头显式携带。
pub(crate) async fn connect_live_sideband(
    endpoint: String,
    mut business_headers: Vec<(String, String)>,
    outbound_proxy: Option<gateway_core::account::OutboundProxy>,
) -> Result<CodexLiveSideband, CodexWebSocketExchangeError> {
    use tungstenite::client::IntoClientRequest;

    // 业务头不包含 RFC 6455 握手字段，由底层客户端生成本次连接的随机 key
    let request = endpoint.as_str().into_client_request()?;
    for (name, value) in request.headers() {
        if !business_headers
            .iter()
            .any(|(header, _)| header.eq_ignore_ascii_case(name.as_str()))
        {
            business_headers.push((
                name.to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            ));
        }
    }
    let connection = CodexWebSocketConnection {
        endpoint,
        headers: business_headers,
        outbound_proxy,
        connection_budget: None,
    };
    let (stream, response) = connect_sideband_websocket(&connection).await?;
    let subprotocol = response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    Ok(CodexLiveSideband {
        stream,
        subprotocol,
    })
}

/// 把上游原始 WebSocket 流拆装为 Core 抽象中继。
pub(crate) fn into_live_relay(stream: RawWsStream, subprotocol: Option<String>) -> LiveRelay {
    let (sink, receiver) = stream.split();
    LiveRelay::new(
        subprotocol,
        Box::new(TungsteniteLiveStream { inner: receiver }),
        Box::new(TungsteniteLiveSink { inner: sink }),
    )
}

fn live_error(kind: LiveGatewayErrorKind, error: impl std::fmt::Display) -> LiveGatewayError {
    LiveGatewayError::new(kind, format!("codex live sideband: {error}"))
}

fn frame_from_message(message: Message) -> Option<LiveFrame> {
    match message {
        Message::Text(text) => Some(LiveFrame::Text(Bytes::copy_from_slice(text.as_bytes()))),
        Message::Binary(binary) => Some(LiveFrame::Binary(binary)),
        Message::Ping(payload) => Some(LiveFrame::Ping(payload)),
        Message::Pong(payload) => Some(LiveFrame::Pong(payload)),
        Message::Close(close) => Some(LiveFrame::Close(close.map(|frame| LiveClose {
            code: u16::from(frame.code),
            reason: frame.reason.as_str().to_owned(),
        }))),
        // 帧级响应帧由 tungstenite 自动应答，不进入中继。
        Message::Frame(_) => None,
    }
}

fn message_from_frame(frame: LiveFrame) -> Message {
    match frame {
        LiveFrame::Text(payload) => {
            Message::Text(String::from_utf8_lossy(&payload).into_owned().into())
        }
        LiveFrame::Binary(payload) => Message::Binary(payload),
        LiveFrame::Ping(payload) => Message::Ping(payload),
        LiveFrame::Pong(payload) => Message::Pong(payload),
        LiveFrame::Close(close) => Message::Close(close.map(|close| {
            tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: close.code.into(),
                reason: close.reason.into(),
            }
        })),
    }
}

struct TungsteniteLiveStream {
    inner: futures::stream::SplitStream<RawWsStream>,
}

impl LiveRelayStream for TungsteniteLiveStream {
    fn next_frame(&mut self) -> futures::future::BoxFuture<'_, Option<LiveFrame>> {
        Box::pin(async move {
            loop {
                match self.inner.next().await {
                    Some(Ok(message)) => {
                        if let Some(frame) = frame_from_message(message) {
                            return Some(frame);
                        }
                    }
                    // 上游关闭或传输失败统一折叠为流结束；关闭码投影由协议层完成。
                    Some(Err(_)) | None => return None,
                }
            }
        })
    }
}

struct TungsteniteLiveSink {
    inner: futures::stream::SplitSink<RawWsStream, Message>,
}

impl LiveRelaySink for TungsteniteLiveSink {
    fn send_frame(
        &mut self,
        frame: LiveFrame,
    ) -> futures::future::BoxFuture<'_, Result<(), LiveGatewayError>> {
        Box::pin(async move {
            self.inner
                .send(message_from_frame(frame))
                .await
                .map_err(|error| live_error(LiveGatewayErrorKind::UpstreamUnavailable, error))
        })
    }

    fn close(&mut self, close: Option<LiveClose>) -> futures::future::BoxFuture<'_, ()> {
        Box::pin(async move {
            let _ = self
                .inner
                .send(message_from_frame(LiveFrame::Close(close)))
                .await;
            let _ = self.inner.close().await;
        })
    }
}
