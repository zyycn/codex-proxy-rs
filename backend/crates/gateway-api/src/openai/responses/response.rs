//! Provider OpenAI Responses wire 到客户端 transport 的透明转发边界。

use bytes::Bytes;
use gateway_core::event::{GatewayEvent, ProtocolWireEvent, ProviderEvent};
use gateway_protocol::openai::sse::encode_sse_event_with_metadata;
use serde_json::Value;

use super::error::ResponseEncodeError;

const OPENAI_PROTOCOL: &str = "openai";

/// OpenAI Responses wire 的转发器。
///
/// OpenAI Provider 与 xAI adapter 都必须交付 OpenAI wire。wire 是客户端交付
/// 与终态判断的事实来源；canonical facts 只在 wire 暂未携带 ID 时提供旁路观测，
/// 不得反过来拒绝或改写上游事件。
#[derive(Debug, Default)]
pub struct OpenAiResponsesEncoder {
    response_id: Option<String>,
    wire_terminal: Option<Value>,
    wire_failure: bool,
}

impl OpenAiResponsesEncoder {
    /// 创建响应 wire 转发器。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            response_id: None,
            wire_terminal: None,
            wire_failure: false,
        }
    }

    /// 消费一个 Provider event，并返回 SSE frames。
    pub fn push_sse(&mut self, event: &ProviderEvent) -> Vec<Bytes> {
        self.observe_canonical_identity(event);
        let Some(wire) = openai_wire(event) else {
            return Vec::new();
        };
        self.observe_wire(wire);
        if let Some(raw_sse_frame) = wire.raw_sse_frame() {
            return vec![raw_sse_frame.clone()];
        }
        vec![Bytes::from(encode_sse_event_with_metadata(
            wire.event_type().unwrap_or_default(),
            &wire.data().to_string(),
            wire.sse_id(),
            wire.sse_retry(),
        ))]
    }

    /// 消费一个 Provider event，并返回 WebSocket JSON messages。
    pub fn push_websocket(&mut self, event: &ProviderEvent) -> Vec<String> {
        self.observe_canonical_identity(event);
        let Some(wire) = openai_wire(event).filter(|wire| wire.has_json_data()) else {
            return Vec::new();
        };
        self.observe_wire(wire);
        vec![wire.data().to_string()]
    }

    /// 返回是否已经看到客户端可见的 wire 终态。
    #[must_use]
    pub fn is_completed(&self) -> bool {
        self.wire_terminal.is_some()
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
    /// 缺少可转换为非流式响应的 wire 终态时返回错误。
    pub fn finish(self) -> Result<Value, ResponseEncodeError> {
        self.wire_terminal
            .ok_or(ResponseEncodeError::MissingWireTerminal)
    }

    fn observe_canonical_identity(&mut self, event: &ProviderEvent) {
        for fact in event.canonical_facts() {
            let metadata = match fact {
                GatewayEvent::Started(metadata) | GatewayEvent::Completed(metadata) => metadata,
                _ => continue,
            };
            self.response_id = Some(metadata.response_id().to_owned());
        }
    }

    fn observe_wire(&mut self, wire: &ProtocolWireEvent) {
        if let Some(response_id) = wire
            .data()
            .pointer("/response/id")
            .or_else(|| wire.data().get("response_id"))
            .and_then(Value::as_str)
        {
            self.response_id = Some(response_id.to_owned());
        }
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
