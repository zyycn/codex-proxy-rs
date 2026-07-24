//! Provider OpenAI Responses wire 到客户端 transport 的透明转发边界。

use bytes::Bytes;
use gateway_core::event::{GatewayEvent, ProtocolWireEvent, ProviderEvent};
use gateway_protocol::openai::sse::encode_sse_event_with_metadata;
use serde_json::Value;

use super::error::ResponseEncodeError;

const OPENAI_PROTOCOL: &str = "openai";

/// OpenAI Responses wire 的转发器。
///
/// OpenAI Provider 与 xAI adapter 都必须交付 OpenAI wire。这里仅保留同一
/// 响应的身份和终态校验，不再从 canonical facts 重建客户端 JSON；因此
/// OpenAI 上游的未知字段、字段顺序和事件语义不会被 API 层二次解释。
#[derive(Debug, Default)]
pub struct OpenAiResponsesEncoder {
    response_id: Option<String>,
    wire_terminal: Option<Value>,
    wire_failure: bool,
    canonical_completed: bool,
}

impl OpenAiResponsesEncoder {
    /// 创建响应 wire 转发器。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            response_id: None,
            wire_terminal: None,
            wire_failure: false,
            canonical_completed: false,
        }
    }

    /// 消费一个 Provider event，并返回 SSE frames。
    ///
    /// # Errors
    ///
    /// 同一响应身份变化时返回错误。
    pub fn push_sse(&mut self, event: &ProviderEvent) -> Result<Vec<Bytes>, ResponseEncodeError> {
        self.observe_identity(event)?;
        let Some(wire) = openai_wire(event) else {
            return Ok(Vec::new());
        };
        self.observe_wire_terminal(wire);
        if let Some(raw_sse_frame) = wire.raw_sse_frame() {
            return Ok(vec![raw_sse_frame.clone()]);
        }
        Ok(vec![Bytes::from(encode_sse_event_with_metadata(
            wire.event_type().unwrap_or_default(),
            &wire.data().to_string(),
            wire.sse_id(),
            wire.sse_retry(),
        ))])
    }

    /// 消费一个 Provider event，并返回 WebSocket JSON messages。
    ///
    /// # Errors
    ///
    /// 同一响应身份变化时返回错误。
    pub fn push_websocket(
        &mut self,
        event: &ProviderEvent,
    ) -> Result<Vec<String>, ResponseEncodeError> {
        self.observe_identity(event)?;
        let Some(wire) = openai_wire(event).filter(|wire| wire.has_json_data()) else {
            return Ok(Vec::new());
        };
        self.observe_wire_terminal(wire);
        Ok(vec![wire.data().to_string()])
    }

    /// 返回是否已经看到同一响应的 canonical 与 wire 终态。
    #[must_use]
    pub fn is_completed(&self) -> bool {
        self.wire_terminal.is_some() && self.canonical_completed
    }

    /// 返回是否已把 Provider 原生失败 event 交付给客户端。
    #[must_use]
    pub const fn has_wire_failure(&self) -> bool {
        self.wire_failure
    }

    /// 返回 Core 已观察到的客户端可见 Provider 原生响应 ID。
    #[must_use]
    pub fn response_id(&self) -> Option<&str> {
        self.response_id.as_deref()
    }

    /// 校验完整响应并返回原生终态 response object。
    ///
    /// # Errors
    ///
    /// 缺少 wire 或 canonical 终态时返回错误。
    pub fn finish(self) -> Result<Value, ResponseEncodeError> {
        if self.canonical_completed {
            self.wire_terminal
                .ok_or(ResponseEncodeError::MissingWireTerminal)
        } else {
            Err(ResponseEncodeError::MissingWireTerminal)
        }
    }

    fn observe_identity(&mut self, event: &ProviderEvent) -> Result<(), ResponseEncodeError> {
        for fact in event.canonical_facts() {
            let metadata = match fact {
                GatewayEvent::Started(metadata) => metadata,
                GatewayEvent::Completed(metadata) => {
                    self.canonical_completed = true;
                    metadata
                }
                _ => continue,
            };
            if self
                .response_id
                .as_deref()
                .is_some_and(|current| current != metadata.response_id())
            {
                return Err(ResponseEncodeError::WireIdentityChanged);
            }
            self.response_id = Some(metadata.response_id().to_owned());
        }
        Ok(())
    }

    fn observe_wire_terminal(&mut self, wire: &ProtocolWireEvent) {
        let effective_type = wire
            .event_type()
            .or_else(|| wire.data().get("type").and_then(Value::as_str));
        if matches!(
            effective_type,
            Some("response.completed" | "response.incomplete")
        ) {
            self.wire_terminal = wire.data().get("response").cloned();
        } else if matches!(effective_type, Some("response.failed" | "error")) {
            self.wire_failure = true;
        }
    }
}

fn openai_wire(event: &ProviderEvent) -> Option<&ProtocolWireEvent> {
    event
        .wire_event()
        .filter(|wire| wire.protocol() == OPENAI_PROTOCOL)
}
