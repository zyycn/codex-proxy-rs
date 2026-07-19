//! Codex Responses SSE 到核心 canonical event 的单一解码边界。

use std::collections::{BTreeMap, BTreeSet};

use gateway_core::accounting::Usage;
use gateway_core::engine::UpstreamSendState;
use gateway_core::error::{ProviderError, ProviderErrorKind};
use gateway_core::event::{
    ContentItem, ContentKind, FinishReason, GatewayEvent, ReasoningDelta, ResponseMeta, TextDelta,
    ToolCallDelta,
};
use gateway_protocol::openai::events::{TokenUsage, extract_usage};
use gateway_protocol::openai::sse::{SseEvent, SseEventDecoder};
use serde_json::Value;

use super::usage::openai_billing_breakdown;

const CONTENTS_PER_OUTPUT: u32 = 1_024;

/// 单 attempt 的增量 Responses decoder。
///
/// 该类型不保留原始正文，也不会把无法识别的事件静默丢弃。Provider transport
/// 越过 send barrier 后才调用它，因此所有解码错误都按 `sent` 处理。
pub struct CodexCanonicalDecoder {
    decoder: SseEventDecoder,
    fallback_model: String,
    response_id: Option<String>,
    started: bool,
    completed: bool,
    content: BTreeMap<u32, ContentKind>,
    tool_arguments_seen: BTreeSet<u32>,
    usage_emitted: bool,
}

impl CodexCanonicalDecoder {
    pub fn new(fallback_model: impl Into<String>) -> Self {
        Self {
            decoder: SseEventDecoder::default(),
            fallback_model: fallback_model.into(),
            response_id: None,
            started: false,
            completed: false,
            content: BTreeMap::new(),
            tool_arguments_seen: BTreeSet::new(),
            usage_emitted: false,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<GatewayEvent>, ProviderError> {
        let events = self.decoder.push(chunk).map_err(protocol_error)?;
        self.decode(events)
    }

    pub fn finish(&mut self) -> Result<Vec<GatewayEvent>, ProviderError> {
        let events = self.decoder.finish().map_err(protocol_error)?;
        self.decode(events)
    }

    fn decode(&mut self, events: Vec<SseEvent>) -> Result<Vec<GatewayEvent>, ProviderError> {
        let mut canonical = Vec::new();
        for event in events {
            if event.data.trim() == "[DONE]" {
                if !self.completed {
                    return Err(protocol_error_marker());
                }
                continue;
            }
            let value = serde_json::from_str::<Value>(&event.data).map_err(protocol_error)?;
            let event_type = event
                .event
                .as_deref()
                .or_else(|| value.get("type").and_then(Value::as_str))
                .ok_or_else(protocol_error_marker)?;
            self.decode_event(event_type, &value, &mut canonical)?;
        }
        Ok(canonical)
    }

    fn decode_event(
        &mut self,
        event_type: &str,
        value: &Value,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        if self.completed {
            return Err(protocol_error_marker());
        }

        match event_type {
            "response.created" | "response.in_progress" => self.start(value, output),
            "response.output_item.added" => self.output_item_added(value, output),
            "response.output_item.done" => self.output_item_done(value, output),
            "response.content_part.added"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_part.added" => self.content_part_added(value, output),
            "response.output_text.delta" | "response.refusal.delta" => {
                self.text_delta(value, output)
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.reasoning_delta(value, output)
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                self.tool_delta(value, output)
            }
            "response.completed" | "response.incomplete" => {
                self.complete(event_type, value, output)
            }
            "response.failed" | "error" => Err(upstream_event_error(value)),
            "response.output_text.done"
            | "response.refusal.done"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.done"
            | "response.function_call_arguments.done"
            | "response.custom_tool_call_input.done"
            | "response.content_part.done"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_part.done"
            | "response.rate_limits.updated"
            | "codex.rate_limits"
            | "response.metadata" => Ok(()),
            _ => Err(unsupported_event_error()),
        }
    }

    fn start(
        &mut self,
        value: &Value,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        if self.started {
            // `response.in_progress` 是 created 后的结构事件，不重复发 Started。
            return Ok(());
        }
        let response = response_object(value).ok_or_else(protocol_error_marker)?;
        let response_id = required_text(response, "id")?;
        let model = response
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
            .unwrap_or(&self.fallback_model)
            .to_owned();
        self.response_id = Some(response_id.clone());
        self.started = true;
        output.push(GatewayEvent::Started(ResponseMeta::new(response_id, model)));
        Ok(())
    }

    fn output_item_added(
        &mut self,
        value: &Value,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        self.require_started()?;
        let item = value.get("item").ok_or_else(protocol_error_marker)?;
        let output_index = event_index(value, "output_index")?;
        match item.get("type").and_then(Value::as_str) {
            Some("function_call" | "custom_tool_call") => {
                let index = content_index(output_index, 0)?;
                self.add_content(index, ContentKind::ToolCall, output)?;
                let call_id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(protocol_error_marker)?;
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned);
                output.push(GatewayEvent::ToolCallDelta(ToolCallDelta {
                    content_index: index,
                    call_id: call_id.to_owned(),
                    name,
                    arguments_delta: String::new(),
                }));
                Ok(())
            }
            Some("reasoning") => {
                let index = content_index(output_index, 0)?;
                self.add_content(index, ContentKind::Reasoning, output)
            }
            Some("message") => Ok(()),
            Some("image_generation_call" | "computer_call" | "web_search_call") => {
                Err(unsupported_event_error())
            }
            _ => Err(unsupported_event_error()),
        }
    }

    fn output_item_done(
        &mut self,
        value: &Value,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        self.require_started()?;
        let item = value.get("item").ok_or_else(protocol_error_marker)?;
        if !matches!(
            item.get("type").and_then(Value::as_str),
            Some("function_call" | "custom_tool_call")
        ) {
            return Ok(());
        }
        let output_index = event_index(value, "output_index")?;
        let index = content_index(output_index, 0)?;
        if self.tool_arguments_seen.contains(&index) {
            return Ok(());
        }
        let arguments = item
            .get("arguments")
            .or_else(|| item.get("input"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let Some(arguments) = arguments else {
            return Ok(());
        };
        let call_id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(protocol_error_marker)?;
        if !self.content.contains_key(&index) {
            self.add_content(index, ContentKind::ToolCall, output)?;
        }
        self.tool_arguments_seen.insert(index);
        output.push(GatewayEvent::ToolCallDelta(ToolCallDelta {
            content_index: index,
            call_id: call_id.to_owned(),
            name: item
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            arguments_delta: arguments.to_owned(),
        }));
        Ok(())
    }

    fn content_part_added(
        &mut self,
        value: &Value,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        self.require_started()?;
        let output_index = event_index(value, "output_index")?;
        let part_index = optional_event_index(value, "content_index")?.unwrap_or_default();
        let part = value
            .get("part")
            .or_else(|| value.get("summary_part"))
            .ok_or_else(protocol_error_marker)?;
        let kind = match part.get("type").and_then(Value::as_str) {
            Some("output_text" | "refusal") => ContentKind::Text,
            Some("summary_text" | "reasoning_text") => ContentKind::Reasoning,
            _ => return Err(unsupported_event_error()),
        };
        self.add_content(content_index(output_index, part_index)?, kind, output)
    }

    fn text_delta(
        &mut self,
        value: &Value,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        self.require_started()?;
        let index = event_content_index(value)?;
        self.ensure_content(index, ContentKind::Text, output)?;
        let text = required_text(value, "delta")?;
        output.push(GatewayEvent::TextDelta(TextDelta {
            content_index: index,
            text,
        }));
        Ok(())
    }

    fn reasoning_delta(
        &mut self,
        value: &Value,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        self.require_started()?;
        let index = event_content_index(value)?;
        self.ensure_content(index, ContentKind::Reasoning, output)?;
        let text = required_text(value, "delta")?;
        output.push(GatewayEvent::ReasoningDelta(ReasoningDelta {
            content_index: index,
            text,
        }));
        Ok(())
    }

    fn tool_delta(
        &mut self,
        value: &Value,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        self.require_started()?;
        let output_index = event_index(value, "output_index")?;
        let index = content_index(output_index, 0)?;
        self.ensure_content(index, ContentKind::ToolCall, output)?;
        let call_id = value
            .get("call_id")
            .or_else(|| value.get("item_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(protocol_error_marker)?;
        let arguments_delta = required_text(value, "delta")?;
        self.tool_arguments_seen.insert(index);
        output.push(GatewayEvent::ToolCallDelta(ToolCallDelta {
            content_index: index,
            call_id: call_id.to_owned(),
            name: None,
            arguments_delta,
        }));
        Ok(())
    }

    fn complete(
        &mut self,
        event_type: &str,
        value: &Value,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        self.require_started()?;
        let response = response_object(value).ok_or_else(protocol_error_marker)?;
        let response_id = required_text(response, "id")?;
        if self.response_id.as_deref() != Some(response_id.as_str()) {
            return Err(protocol_error_marker());
        }
        let usage = extract_usage(response);
        if !self.usage_emitted
            && let Some(usage) = usage
        {
            output.push(GatewayEvent::Usage(core_usage(usage)));
            self.usage_emitted = true;
        }
        let model = response
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
            .unwrap_or(&self.fallback_model)
            .to_owned();
        if let Some(breakdown) = usage
            .filter(|usage| billable_usage_is_complete(response, *usage))
            .and_then(|usage| {
                openai_billing_breakdown(
                    &model,
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cached_tokens,
                    usage.cache_write_tokens,
                    response.get("service_tier").and_then(Value::as_str),
                )
            })
        {
            output.push(GatewayEvent::CalculatedCost(breakdown.calculated_cost()));
        }
        let finish_reason = if event_type == "response.incomplete"
            || response.get("status").and_then(Value::as_str) == Some("incomplete")
        {
            incomplete_finish_reason(response)
        } else {
            FinishReason::Stop
        };
        output.push(GatewayEvent::Completed(
            ResponseMeta::new(response_id, model).with_finish_reason(finish_reason),
        ));
        self.completed = true;
        Ok(())
    }

    fn add_content(
        &mut self,
        index: u32,
        kind: ContentKind,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        if self.content.insert(index, kind).is_some() {
            return Err(protocol_error_marker());
        }
        output.push(GatewayEvent::ContentAdded(ContentItem::new(index, kind)));
        Ok(())
    }

    fn ensure_content(
        &mut self,
        index: u32,
        kind: ContentKind,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        match self.content.get(&index) {
            Some(current) if *current == kind => Ok(()),
            Some(_) => Err(protocol_error_marker()),
            None => self.add_content(index, kind, output),
        }
    }

    fn require_started(&self) -> Result<(), ProviderError> {
        if self.started {
            Ok(())
        } else {
            Err(protocol_error_marker())
        }
    }
}

fn response_object(value: &Value) -> Option<&Value> {
    value
        .get("response")
        .filter(|response| response.is_object())
}

fn required_text(value: &Value, field: &str) -> Result<String, ProviderError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(protocol_error_marker)
}

fn optional_event_index(value: &Value, field: &str) -> Result<Option<u32>, ProviderError> {
    value
        .get(field)
        .map(|index| {
            index
                .as_u64()
                .and_then(|index| u32::try_from(index).ok())
                .ok_or_else(protocol_error_marker)
        })
        .transpose()
}

fn event_index(value: &Value, field: &str) -> Result<u32, ProviderError> {
    optional_event_index(value, field)?.ok_or_else(protocol_error_marker)
}

fn event_content_index(value: &Value) -> Result<u32, ProviderError> {
    content_index(
        event_index(value, "output_index")?,
        optional_event_index(value, "content_index")?.unwrap_or_default(),
    )
}

fn content_index(output_index: u32, part_index: u32) -> Result<u32, ProviderError> {
    if part_index >= CONTENTS_PER_OUTPUT {
        return Err(protocol_error_marker());
    }
    output_index
        .checked_mul(CONTENTS_PER_OUTPUT)
        .and_then(|base| base.checked_add(part_index))
        .ok_or_else(protocol_error_marker)
}

fn core_usage(usage: TokenUsage) -> Usage {
    let mut normalized = Usage::new();
    normalized.input_tokens = Some(usage.input_tokens);
    normalized.output_tokens = Some(usage.output_tokens);
    normalized.cached_tokens = Some(usage.cached_tokens);
    normalized.cache_write_tokens = Some(usage.cache_write_tokens);
    normalized.reasoning_tokens = Some(usage.reasoning_tokens);
    normalized.total_tokens = Some(usage.total_tokens);
    normalized
}

fn billable_usage_is_complete(response: &Value, usage: TokenUsage) -> bool {
    let Some(raw) = response.get("usage").filter(|value| value.is_object()) else {
        return false;
    };
    let input = raw
        .get("input_tokens")
        .or_else(|| raw.get("prompt_tokens"))
        .and_then(Value::as_u64);
    let output = raw
        .get("output_tokens")
        .or_else(|| raw.get("completion_tokens"))
        .and_then(Value::as_u64);
    let cached = raw
        .pointer("/input_tokens_details/cached_tokens")
        .or_else(|| raw.pointer("/prompt_tokens_details/cached_tokens"))
        .or_else(|| raw.get("cached_tokens"));
    let cached = match cached {
        Some(value) => value.as_u64(),
        None => Some(0),
    };

    input == Some(usage.input_tokens)
        && output == Some(usage.output_tokens)
        && cached == Some(usage.cached_tokens)
        && usage.cached_tokens <= usage.input_tokens
}

fn incomplete_finish_reason(response: &Value) -> FinishReason {
    match response
        .pointer("/incomplete_details/reason")
        .and_then(Value::as_str)
    {
        Some("max_output_tokens" | "max_tokens") => FinishReason::Length,
        Some("content_filter") => FinishReason::ContentFilter,
        _ => FinishReason::Other,
    }
}

fn upstream_event_error(value: &Value) -> ProviderError {
    let kind = match value
        .pointer("/error/code")
        .or_else(|| value.pointer("/response/error/code"))
        .and_then(Value::as_str)
    {
        Some("invalid_request" | "invalid_prompt" | "cyber_policy") => {
            ProviderErrorKind::InvalidRequest
        }
        Some("unsupported" | "unsupported_feature" | "model_not_supported") => {
            ProviderErrorKind::Unsupported
        }
        Some(
            "unauthorized"
            | "invalid_api_key"
            | "token_invalid"
            | "token_invalidated"
            | "access_token_expired",
        ) => ProviderErrorKind::Unauthorized,
        Some("permission_denied") => ProviderErrorKind::PermissionDenied,
        Some("rate_limit_exceeded") => ProviderErrorKind::RateLimited,
        Some("quota_exceeded" | "insufficient_quota") => ProviderErrorKind::QuotaExhausted,
        _ => ProviderErrorKind::Unavailable,
    };
    ProviderError::new(kind, UpstreamSendState::Sent).redact_sensitive_context("upstream event")
}

fn protocol_error(_error: impl std::fmt::Debug) -> ProviderError {
    protocol_error_marker()
}

fn protocol_error_marker() -> ProviderError {
    ProviderError::new(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
        .redact_sensitive_context("invalid upstream event")
}

fn unsupported_event_error() -> ProviderError {
    ProviderError::new(ProviderErrorKind::Unsupported, UpstreamSendState::Sent)
        .redact_sensitive_context("unsupported upstream event")
}
