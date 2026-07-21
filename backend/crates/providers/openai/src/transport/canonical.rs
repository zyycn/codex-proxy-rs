//! Codex Responses SSE 到核心 canonical event 的单一解码边界。

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use gateway_core::accounting::Usage;
use gateway_core::engine::UpstreamSendState;
use gateway_core::error::{ProviderError, ProviderErrorKind};
use gateway_core::event::{
    ContentItem, ContentKind, FinishReason, GatewayEvent, ProtocolWireEvent, ProviderEvent,
    ReasoningDelta, ResponseMeta, TextDelta, ToolCallDelta,
};
use gateway_protocol::openai::events::{TokenUsage, extract_usage};
use gateway_protocol::openai::sse::{SseEvent, SseEventDecoder};
use serde_json::Value;
use thiserror::Error;

use super::protocol::responses::{ResponsesSseFailure, response_event_signals};
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
    semantic_output_seen: bool,
}

/// 上游 Responses 事件的两类失败：协议损坏，或上游明确报告业务失败。
#[derive(Error)]
pub enum CodexCanonicalError {
    /// SSE/JSON 违反了已知协议不变量。
    #[error("invalid Codex Responses event")]
    Protocol(#[source] ProviderError),
    /// 上游在成功建立流后发送了明确的失败事件。
    #[error("Codex upstream reported a failed response")]
    Upstream(ResponsesSseFailure),
}

impl fmt::Debug for CodexCanonicalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => formatter.debug_tuple("Protocol").field(error).finish(),
            Self::Upstream(_) => formatter.write_str("Upstream(<redacted>)"),
        }
    }
}

impl From<ProviderError> for CodexCanonicalError {
    fn from(error: ProviderError) -> Self {
        Self::Protocol(error)
    }
}

/// 单次增量解码的有序结果。
///
/// 上游失败不是解析异常：它与失败前已经产生的事件一起返回，Provider 因而可以
/// 先保留真实输出，再把类型化失败交给 Core 收敛。
pub enum CodexCanonicalOutcome {
    /// 本批次只包含正常事件。
    Events(Vec<ProviderEvent>),
    /// 本批次在若干正常事件后到达失败边界。
    Failed(CodexCanonicalFailure),
}

/// `response.failed` 或协议错误发生时的单一 typed outcome。
pub struct CodexCanonicalFailure {
    events: Vec<ProviderEvent>,
    error: CodexCanonicalError,
    semantic_output_seen: bool,
}

impl fmt::Debug for CodexCanonicalFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCanonicalFailure")
            .field("events", &self.events)
            .field("error", &self.error)
            .field("semantic_output_seen", &self.semantic_output_seen)
            .finish()
    }
}

impl CodexCanonicalFailure {
    /// 失败前按上游顺序产生的事件。
    pub fn events(&self) -> &[ProviderEvent] {
        &self.events
    }

    /// 类型化失败；其 Debug/Display 不包含上游正文。
    pub const fn error(&self) -> &CodexCanonicalError {
        &self.error
    }

    /// 失败前是否已经出现客户端可消费的真实输出。
    pub const fn semantic_output_seen(&self) -> bool {
        self.semantic_output_seen
    }

    pub(crate) fn into_parts(self) -> (Vec<ProviderEvent>, CodexCanonicalError, bool) {
        (self.events, self.error, self.semantic_output_seen)
    }
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
            semantic_output_seen: false,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> CodexCanonicalOutcome {
        let events = match self.decoder.push(chunk) {
            Ok(events) => events,
            Err(error) => {
                return self.failure(
                    Vec::new(),
                    CodexCanonicalError::Protocol(protocol_error(error)),
                );
            }
        };
        self.decode(events)
    }

    pub fn finish(&mut self) -> CodexCanonicalOutcome {
        let events = match self.decoder.finish() {
            Ok(events) => events,
            Err(error) => {
                return self.failure(
                    Vec::new(),
                    CodexCanonicalError::Protocol(protocol_error(error)),
                );
            }
        };
        self.decode(events)
    }

    fn decode(&mut self, events: Vec<SseEvent>) -> CodexCanonicalOutcome {
        let mut output = Vec::new();
        for event in events {
            if event.data.trim() == "[DONE]" {
                if !self.completed {
                    return self.failure(
                        output,
                        CodexCanonicalError::Protocol(protocol_error_marker()),
                    );
                }
                continue;
            }
            let value = match serde_json::from_str::<Value>(&event.data) {
                Ok(value) => value,
                Err(error) => {
                    return self
                        .failure(output, CodexCanonicalError::Protocol(protocol_error(error)));
                }
            };
            let Some(event_type) = event
                .event
                .as_deref()
                .or_else(|| value.get("type").and_then(Value::as_str))
            else {
                return self.failure(
                    output,
                    CodexCanonicalError::Protocol(protocol_error_marker()),
                );
            };
            if matches!(event_type, "response.failed" | "error") {
                return self.failure(
                    output,
                    CodexCanonicalError::Upstream(ResponsesSseFailure::from_event(
                        event_type, &value,
                    )),
                );
            }
            let mut canonical = Vec::new();
            if let Err(error) = self.decode_event(event_type, &value, &mut canonical) {
                return self.failure(output, CodexCanonicalError::Protocol(error));
            }
            let semantic_output = response_event_signals(Some(event_type), &value).semantic_output;
            let wire = match ProtocolWireEvent::json_with_sse_metadata(
                "openai",
                event.event,
                value,
                event.id,
                event.retry,
            ) {
                Ok(wire) => wire,
                Err(_) => {
                    return self.failure(
                        output,
                        CodexCanonicalError::Protocol(protocol_error_marker()),
                    );
                }
            };
            output.push(if canonical.is_empty() {
                ProviderEvent::wire(wire)
            } else {
                ProviderEvent::canonical_with_wire(canonical, wire)
            });
            self.semantic_output_seen |= semantic_output;
        }
        CodexCanonicalOutcome::Events(output)
    }

    fn failure(
        &self,
        events: Vec<ProviderEvent>,
        error: CodexCanonicalError,
    ) -> CodexCanonicalOutcome {
        CodexCanonicalOutcome::Failed(CodexCanonicalFailure {
            events,
            error,
            semantic_output_seen: self.semantic_output_seen,
        })
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
            "response.failed" | "error" => Err(protocol_error_marker()),
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
            _ => Ok(()),
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
            Some("image_generation_call" | "computer_call" | "web_search_call") => Ok(()),
            _ => Ok(()),
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
            _ => return Ok(()),
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

fn protocol_error(_error: impl std::fmt::Debug) -> ProviderError {
    protocol_error_marker()
}

fn protocol_error_marker() -> ProviderError {
    ProviderError::new(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
        .redact_sensitive_context("invalid upstream event")
}
