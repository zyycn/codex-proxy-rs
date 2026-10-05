//! 插件 WebSocket 中间件的消息、载荷与会话收发接口

use super::super::session::{CallCancellation, CallReply, HostClient, PluginCall, SessionError};
use super::{MiddlewareInput, MiddlewareOutput, invalid_input};
use crate::client::{HostReply, read::PendingRead};
use crate::{
    CallContext, ErrorCode, PluginFault, Stage,
    call::middleware::{
        BODY_READ_METHOD, HANDLE_METHOD, MiddlewareBodyRead, MiddlewareHeader, NEXT_METHOD,
        websocket as wire,
    },
};
pub use wire::{Direction as WebSocketDirection, Kind as WebSocketKind};

/// 原始消息正文按需读取；直接传给 next 或返回时不读取、不复制
pub struct WebSocketPayload {
    source: wire::Payload,
    bytes: Vec<u8>,
    host: Option<HostClient>,
    read: PendingRead<Result<HostReply, SessionError>>,
}
impl WebSocketPayload {
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            source: wire::Payload::Bytes,
            bytes,
            host: None,
            read: PendingRead::default(),
        }
    }
    pub async fn read(&mut self) -> Result<Option<Vec<u8>>, PluginFault> {
        let wire::Payload::Handle { handle } = &self.source else {
            return if self.bytes.is_empty() {
                Ok(None)
            } else {
                Ok(Some(std::mem::take(&mut self.bytes)))
            };
        };
        let host = self.host.as_ref().ok_or_else(invalid_input)?;
        let reply = self
            .read
            .run(|| {
                let host = host.clone();
                let handle = handle.clone();
                async move {
                    host.call(
                        BODY_READ_METHOD,
                        serde_json::to_value(MiddlewareBodyRead {
                            handle,
                            maximum_bytes: u32::try_from(host.maximum_stream_chunk_bytes())
                                .map_err(|_| SessionError::Protocol)?,
                        })
                        .map_err(|_| SessionError::Protocol)?,
                        Vec::new(),
                    )
                    .await
                }
            })
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let read: wire::Read = serde_json::from_value(reply.result).map_err(|_| invalid_input())?;
        if read.eof {
            if !reply.payload.is_empty() {
                return Err(invalid_input());
            }
            self.source = wire::Payload::Bytes;
            Ok(None)
        } else {
            Ok(Some(reply.payload))
        }
    }
    /// 不再需要正文时释放句柄；会话结束也会回收所有未释放资源
    pub async fn close(&mut self) -> Result<(), PluginFault> {
        if let wire::Payload::Handle { handle } = &self.source {
            let host = self.host.as_ref().ok_or_else(invalid_input)?;
            let reply = host
                .call(
                    crate::call::middleware::BODY_CLOSE_METHOD,
                    serde_json::to_value(crate::call::middleware::MiddlewareBodyClose {
                        handle: handle.clone(),
                    })
                    .map_err(|_| invalid_input())?,
                    Vec::new(),
                )
                .await
                .map_err(SessionError::into_plugin_fault)?;
            if reply.result != serde_json::json!({}) || !reply.payload.is_empty() {
                return Err(invalid_input());
            }
        }
        self.source = wire::Payload::Bytes;
        self.bytes.clear();
        self.read.clear();
        Ok(())
    }
    /// 收集大小由插件决定；透传不需要调用此方法
    pub async fn collect(mut self) -> Result<Vec<u8>, PluginFault> {
        let mut bytes = Vec::new();
        while let Some(chunk) = self.read().await? {
            bytes.extend(chunk);
        }
        Ok(bytes)
    }
}
pub struct WebSocketMessage {
    pub kind: WebSocketKind,
    pub payload: WebSocketPayload,
}
impl WebSocketMessage {
    #[must_use]
    pub fn new(kind: WebSocketKind, bytes: Vec<u8>) -> Self {
        Self {
            kind,
            payload: WebSocketPayload::from_bytes(bytes),
        }
    }
    pub(crate) fn decode(
        message: wire::Message,
        bytes: Vec<u8>,
        host: HostClient,
    ) -> Result<Self, PluginFault> {
        if matches!(message.payload, wire::Payload::Handle { .. }) && !bytes.is_empty() {
            return Err(invalid_input());
        }
        Ok(Self {
            kind: message.kind,
            payload: WebSocketPayload {
                source: message.payload,
                bytes,
                host: Some(host),
                read: PendingRead::default(),
            },
        })
    }
    fn into_wire(self) -> Result<(wire::Message, Vec<u8>), PluginFault> {
        // 尚在读取的消息不能直接归还句柄，否则宿主稍后返回的数据会被丢弃
        if self.payload.read.is_pending() {
            return Err(PluginFault::new(
                ErrorCode::InvalidInput,
                "a pending WebSocket payload read must be resumed or closed before transfer",
            ));
        }
        Ok((
            wire::Message {
                kind: self.kind,
                payload: self.payload.source,
            },
            self.payload.bytes,
        ))
    }
}
pub struct WebSocketNext {
    host: HostClient,
}
impl WebSocketNext {
    pub async fn run(
        self,
        message: WebSocketMessage,
    ) -> Result<Option<WebSocketMessage>, PluginFault> {
        let (head, bytes) = message.into_wire()?;
        let reply = self
            .host
            .call(
                NEXT_METHOD,
                serde_json::to_value(head).map_err(|_| invalid_input())?,
                bytes,
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let result: Option<wire::Message> =
            serde_json::from_value(reply.result).map_err(|_| invalid_input())?;
        match result {
            Some(message) => WebSocketMessage::decode(message, reply.payload, self.host).map(Some),
            None if reply.payload.is_empty() => Ok(None),
            None => Err(invalid_input()),
        }
    }
}
#[derive(Clone)]
pub struct WebSocketSender {
    host: HostClient,
}
impl WebSocketSender {
    pub(crate) fn new(host: HostClient) -> Self {
        Self { host }
    }
    /// 在当前连接直接发送消息并等待实际写入，不伪装成默认协议的输出
    pub async fn send(&self, message: WebSocketMessage) -> Result<(), PluginFault> {
        let (head, bytes) = message.into_wire()?;
        let reply = self
            .host
            .call(
                wire::SEND_METHOD,
                serde_json::to_value(head).map_err(|_| invalid_input())?,
                bytes,
            )
            .await
            .map_err(SessionError::into_plugin_fault)?;
        if reply.result != serde_json::json!({}) || !reply.payload.is_empty() {
            return Err(invalid_input());
        }
        Ok(())
    }
}
pub struct WebSocketCall {
    pub context: CallContext,
    pub connection_id: String,
    pub direction: WebSocketDirection,
    pub headers: Vec<MiddlewareHeader>,
    pub message: WebSocketMessage,
    pub next: WebSocketNext,
    pub sender: WebSocketSender,
    pub host: HostClient,
    pub cancellation: CallCancellation,
}
impl MiddlewareInput for WebSocketCall {
    type Output = Option<WebSocketMessage>;
    fn accepts(stage: Stage) -> bool {
        stage == Stage::WebSocket
    }
    fn decode(call: PluginCall) -> Result<Self, PluginFault> {
        if call.method != HANDLE_METHOD || !Self::accepts(call.context.stage) {
            return Err(invalid_input());
        }
        let input: wire::Call = serde_json::from_value(call.params).map_err(|_| invalid_input())?;
        Ok(Self {
            context: call.context,
            connection_id: input.connection_id,
            direction: input.direction,
            headers: input.headers,
            message: WebSocketMessage::decode(input.message, call.payload, call.host.clone())?,
            next: WebSocketNext {
                host: call.host.clone(),
            },
            sender: WebSocketSender {
                host: call.host.clone(),
            },
            host: call.host,
            cancellation: call.cancellation,
        })
    }
}
impl MiddlewareOutput for Option<WebSocketMessage> {
    fn encode(self) -> Result<CallReply, PluginFault> {
        let (head, payload) = match self {
            Some(message) => {
                let (head, payload) = message.into_wire()?;
                (Some(head), payload)
            }
            None => (None, Vec::new()),
        };
        Ok(CallReply::unary(
            serde_json::to_value(head).map_err(|_| invalid_input())?,
            payload,
        ))
    }
}

/// 由 HTTP 中间件接管的连接；接收权独占，发送端可以复制后并发使用
pub struct WebSocketSession {
    pub sender: WebSocketSender,
    pub cancellation: CallCancellation,
    host: HostClient,
    receive: PendingRead<Result<HostReply, SessionError>>,
}

impl WebSocketSession {
    pub(crate) fn new(host: HostClient, cancellation: CallCancellation) -> Self {
        Self {
            sender: WebSocketSender::new(host.clone()),
            cancellation,
            host,
            receive: PendingRead::default(),
        }
    }
    pub async fn receive(&mut self) -> Result<Option<WebSocketMessage>, PluginFault> {
        let reply = self
            .receive
            .run(|| {
                let host = self.host.clone();
                async move {
                    host.call(
                        crate::call::middleware::http::RECEIVE_METHOD,
                        serde_json::json!({}),
                        Vec::new(),
                    )
                    .await
                }
            })
            .await
            .map_err(SessionError::into_plugin_fault)?;
        let message: Option<wire::Message> =
            serde_json::from_value(reply.result).map_err(|_| invalid_input())?;
        match message {
            Some(message) => {
                WebSocketMessage::decode(message, reply.payload, self.host.clone()).map(Some)
            }
            None if reply.payload.is_empty() => Ok(None),
            None => Err(invalid_input()),
        }
    }
    pub async fn close(self, code: Option<u16>, reason: String) -> Result<(), PluginFault> {
        self.sender
            .send(WebSocketMessage::new(
                WebSocketKind::Close { code },
                reason.into_bytes(),
            ))
            .await
    }
}
