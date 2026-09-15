//! Provider OpenAI Responses wire 到客户端 transport 的透明转发边界。

use bytes::Bytes;
use gateway_core::event::{GatewayEvent, ProtocolWireEvent, ProviderEvent};
use gateway_protocol::openai::sse::{
    encode_sse_event_with_metadata, response_failed_sse_data_from_error_event,
};
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
    response_snapshot: Option<Value>,
    wire_terminal: Option<Value>,
    wire_failure: bool,
}

impl OpenAiResponsesEncoder {
    /// 创建响应 wire 转发器。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            response_id: None,
            response_snapshot: None,
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
        // 当前 Codex 不消费 Responses `error` event，会在 EOF 时丢失失败原因。
        // 在客户端 SSE 边界统一投影成它能识别的 `response.failed`。
        if wire.event_type() == Some("error")
            && let Some(data) = response_failed_sse_data_from_error_event(
                self.response_snapshot.as_ref(),
                self.response_id.as_deref(),
                wire.data(),
            )
        {
            return vec![Bytes::from(encode_sse_event_with_metadata(
                "response.failed",
                &data.to_string(),
                wire.sse_id(),
                wire.sse_retry(),
            ))];
        }
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
        // WS 客户端只对它无法消费的裸 `error` 帧做投影：codex 的 WS 端点
        // 会静默忽略缺少 status 且不含内置可重试错误码的 `error` 帧，客户
        // 端只能空等到 idle 超时。投影成 `response.failed`（消息 JSON 自带
        // type 字段），codex 将其映射为可重试错误并立即重试。可消费形状
        // （带非 2xx status 或特殊错误码）原样透传，保持业务语义。
        if wire.event_type() == Some("error")
            && !ws_client_consumable_error(wire.data())
            && let Some(data) = response_failed_sse_data_from_error_event(
                self.response_snapshot.as_ref(),
                self.response_id.as_deref(),
                wire.data(),
            )
        {
            return vec![data.to_string()];
        }
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
            Some("response.created" | "response.in_progress" | "response.queued")
        ) && let Some(response) = wire
            .data()
            .get("response")
            .filter(|value| value.is_object())
        {
            self.response_snapshot = Some(response.clone());
        }
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

/// 判断 WS 上游的 `error` 帧是否已经是客户端可直接消费的形状。
///
/// codex 的 WS 端点只消费两类 `error` 帧：带非 2xx HTTP status 的包装错误，
/// 以及连接数上限 / previous_response_not_found 这类内置可重试错误码；其余
/// 帧（包括 status 为 2xx 的矛盾形状）会被静默忽略。可消费的帧原样透传，
/// 不可消费的才在 WS 边界投影成 `response.failed`。
fn ws_client_consumable_error(data: &Value) -> bool {
    if data
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .is_some_and(|code| {
            matches!(
                code,
                "websocket_connection_limit_reached" | "previous_response_not_found"
            )
        })
    {
        return true;
    }
    data.get("status")
        .or_else(|| data.get("status_code"))
        .and_then(Value::as_u64)
        .is_some_and(|status| !(200..300).contains(&status))
}
