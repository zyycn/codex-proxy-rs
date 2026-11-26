//! 观察 Grok Responses 原生 SSE，保留原始帧并提取计量与生命周期事实

use std::collections::{BTreeMap, BTreeSet};

use gateway_core::error::{
    ClientVisibleUpstreamError, OpaqueUpstreamValue, ProviderDiagnostic, ProviderError,
    ProviderErrorKind,
};
use gateway_core::event::{
    ContentItem, ContentKind, FinishReason, GatewayEvent, ProtocolWireEvent, ProviderEvent,
    ReasoningDelta, ResponseMeta, TextDelta, ToolCallDelta,
};
use gateway_core::metering::{CalculatedCost, ProviderReportedCost, Usage};
use gateway_core::upstream::UpstreamSendState;
use gateway_protocol::openai::OPENAI_PROTOCOL;
use gateway_protocol::openai::events::{TokenUsage, billable_usage_is_complete, extract_usage};
use gateway_protocol::openai::sse::{SseEventDecoder, SseFrame};
use serde_json::Value;

use super::GrokInferenceTransportErrorKind;
use super::request::GrokResponsesRequest;
use super::upstream_error::{classify_grok_quota_failure, scrub_account_fingerprints};
use crate::billing::grok_billing_breakdown_with_override;

const CONTENTS_PER_OUTPUT: u32 = 1_024;
/// 单次官方 Grok Responses 尝试的增量解码器
///
/// 每个上游 event 同时保留 OpenAI wire，并在可识别时附加 canonical facts
pub struct GrokCanonicalDecoder {
    pricing: Option<gateway_core::metering::ModelPriceOverride>,
    decoder: SseEventDecoder,
    upstream_model: String,
    response_id: Option<String>,
    started: bool,
    completed: bool,
    content: BTreeMap<u32, ContentKind>,
    tool_arguments_seen: BTreeSet<u32>,
    usage_emitted: bool,
    response_service_tier: Option<String>,
    requires_provider_cost: bool,
}

impl GrokCanonicalDecoder {
    /// 使用路由后最终发往上游的请求模型计价，并在响应缺少模型时用于 canonical 兜底
    pub fn new(upstream_model: impl Into<String>) -> Self {
        Self {
            decoder: SseEventDecoder::default(),
            pricing: None,
            upstream_model: upstream_model.into(),
            response_id: None,
            started: false,
            completed: false,
            content: BTreeMap::new(),
            tool_arguments_seen: BTreeSet::new(),
            usage_emitted: false,
            response_service_tier: None,
            requires_provider_cost: false,
        }
    }

    #[must_use]
    pub fn with_pricing(
        mut self,
        pricing: Option<gateway_core::metering::ModelPriceOverride>,
    ) -> Self {
        self.pricing = pricing;
        self
    }

    /// 使用最终请求中的原生工具声明判断是否需要上游费用事实
    #[must_use]
    pub fn for_request(upstream_model: impl Into<String>, request: &GrokResponsesRequest) -> Self {
        Self {
            requires_provider_cost: request.body().get("tool_choice").and_then(Value::as_str)
                != Some("none")
                && request
                    .body()
                    .get("tools")
                    .and_then(Value::as_array)
                    .is_some_and(|tools| {
                        tools.iter().any(|tool| {
                            !matches!(
                                tool.get("type").and_then(Value::as_str),
                                Some("function" | "custom" | "mcp")
                            )
                        })
                    }),
            ..Self::new(upstream_model)
        }
    }

    /// 上游报告的实际服务档位，供费用与持久观测共用
    #[must_use]
    pub fn response_service_tier(&self) -> Option<&str> {
        self.response_service_tier.as_deref()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<ProviderEvent>, ProviderError> {
        let frames = self.decoder.push_frames(chunk);
        self.decode(frames)
    }

    pub fn finish(&mut self) -> Result<Vec<ProviderEvent>, ProviderError> {
        let frames = self.decoder.finish_frames();
        let events = self.decode(frames)?;
        if !self.completed {
            return Err(protocol_error_marker());
        }
        Ok(events)
    }

    fn decode(&mut self, frames: Vec<SseFrame>) -> Result<Vec<ProviderEvent>, ProviderError> {
        let mut output = Vec::new();
        for frame in frames {
            let (raw, events) = frame.into_parts();
            let raw = bytes::Bytes::from(raw);
            if events.is_empty() {
                output.push(ProviderEvent::wire(
                    ProtocolWireEvent::raw_sse(OPENAI_PROTOCOL, raw).map_err(protocol_error)?,
                ));
                continue;
            }
            for (index, event) in events.into_iter().enumerate() {
                let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
                    if index == 0 {
                        output.push(ProviderEvent::wire(
                            ProtocolWireEvent::raw_sse(OPENAI_PROTOCOL, raw.clone())
                                .map_err(protocol_error)?,
                        ));
                    }
                    continue;
                };
                let event_type = value
                    .get("type")
                    .and_then(Value::as_str)
                    .or(event.event.as_deref())
                    .unwrap_or_default()
                    .to_owned();
                if let Some(response) = value.get("response") {
                    if let Some(tier) = response.get("service_tier").and_then(Value::as_str) {
                        self.response_service_tier = Some(tier.trim().to_owned());
                    }
                    self.requires_provider_cost |= !token_only_response(response);
                }
                if let Some(item) = value.get("item") {
                    self.requires_provider_cost |= !token_only_output(item);
                }
                let mut facts = Vec::new();
                // 已知终态必须包含有效计量；其他原生扩展不依赖观察器是否理解
                let terminal = matches!(
                    event_type.as_str(),
                    "response.completed" | "response.incomplete"
                );
                match self.decode_event(&event_type, &value, &mut facts) {
                    Ok(()) => {}
                    Err(error) if !terminal && error.kind() == ProviderErrorKind::Protocol => {
                        facts.clear()
                    }
                    Err(error) => return Err(error),
                }
                let wire = if index == 0 {
                    ProtocolWireEvent::json_with_raw_sse_metadata(
                        OPENAI_PROTOCOL,
                        event.event,
                        value,
                        raw.clone(),
                        event.id,
                        event.retry,
                    )
                } else {
                    ProtocolWireEvent::json_with_sse_metadata(
                        OPENAI_PROTOCOL,
                        event.event,
                        value,
                        event.id,
                        event.retry,
                    )
                }
                .map_err(protocol_error)?;
                output.push(if facts.is_empty() {
                    ProviderEvent::wire(wire)
                } else {
                    ProviderEvent::canonical_with_wire(facts, wire)
                });
            }
        }
        Ok(output)
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
            "response.queued" => Ok(()),
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
            | "rate_limits.updated" => Ok(()),
            _ => Ok(()),
        }
    }

    fn start(
        &mut self,
        value: &Value,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        if self.started {
            return self.confirm_response_id(value);
        }
        let response = response_object(value).ok_or_else(protocol_error_marker)?;
        let response_id = required_text(response, "id")?;
        let model = response
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
            .unwrap_or(&self.upstream_model)
            .to_owned();
        self.response_id = Some(response_id.clone());
        self.started = true;
        output.push(GatewayEvent::Started(ResponseMeta::new(response_id, model)));
        Ok(())
    }

    fn confirm_response_id(&self, value: &Value) -> Result<(), ProviderError> {
        let response = response_object(value).ok_or_else(protocol_error_marker)?;
        let response_id = required_text(response, "id")?;
        if self.response_id.as_deref() == Some(response_id.as_str()) {
            Ok(())
        } else {
            Err(protocol_error_marker())
        }
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
            Some("reasoning") => self.add_content(
                content_index(output_index, 0)?,
                ContentKind::Reasoning,
                output,
            ),
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
        match item.get("type").and_then(Value::as_str) {
            Some("message" | "reasoning") => return Ok(()),
            Some("function_call" | "custom_tool_call") => {}
            Some(_) | None => return Ok(()),
        }
        let output_index = event_index(value, "output_index")?;
        let index = content_index(output_index, 0)?;
        if self.tool_arguments_seen.contains(&index) {
            return Ok(());
        }
        let Some(arguments) = item
            .get("arguments")
            .or_else(|| item.get("input"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            return Ok(());
        };
        let call_id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(protocol_error_marker)?;
        self.ensure_content(index, ContentKind::ToolCall, output)?;
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
        let part_index = optional_event_index(value, "content_index")?
            .or(optional_event_index(value, "summary_index")?)
            .unwrap_or_default();
        let part = value
            .get("part")
            .or_else(|| value.get("summary_part"))
            .ok_or_else(protocol_error_marker)?;
        let kind = match part.get("type").and_then(Value::as_str) {
            Some("output_text" | "refusal") => ContentKind::Text,
            Some("summary_text" | "reasoning_text") => ContentKind::Reasoning,
            Some(_) | None => return Ok(()),
        };
        self.ensure_content(content_index(output_index, part_index)?, kind, output)
    }

    fn text_delta(
        &mut self,
        value: &Value,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        self.require_started()?;
        let index = event_content_index(value)?;
        self.ensure_content(index, ContentKind::Text, output)?;
        output.push(GatewayEvent::TextDelta(TextDelta {
            content_index: index,
            text: required_text(value, "delta")?,
        }));
        Ok(())
    }

    fn reasoning_delta(
        &mut self,
        value: &Value,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        self.require_started()?;
        let index = event_reasoning_content_index(value)?;
        self.ensure_content(index, ContentKind::Reasoning, output)?;
        output.push(GatewayEvent::ReasoningDelta(ReasoningDelta {
            content_index: index,
            text: required_text(value, "delta")?,
        }));
        Ok(())
    }

    fn tool_delta(
        &mut self,
        value: &Value,
        output: &mut Vec<GatewayEvent>,
    ) -> Result<(), ProviderError> {
        self.require_started()?;
        let index = content_index(event_index(value, "output_index")?, 0)?;
        self.ensure_content(index, ContentKind::ToolCall, output)?;
        let call_id = value
            .get("call_id")
            .or_else(|| value.get("item_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(protocol_error_marker)?;
        self.tool_arguments_seen.insert(index);
        output.push(GatewayEvent::ToolCallDelta(ToolCallDelta {
            content_index: index,
            call_id: call_id.to_owned(),
            name: None,
            arguments_delta: required_text(value, "delta")?,
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
            .unwrap_or(&self.upstream_model)
            .to_owned();
        if let Some(cost) = provider_reported_cost(response)? {
            output.push(GatewayEvent::ProviderCost(cost));
        } else if !self.requires_provider_cost
            && let Some(cost) = usage.and_then(|usage| {
                // 已报告金额仍优先；本地估价只使用实际发送模型，响应模型仅作观测
                calculated_cost(
                    response,
                    &self.upstream_model,
                    usage,
                    self.response_service_tier(),
                    self.pricing.as_ref(),
                )
            })
        {
            output.push(GatewayEvent::CalculatedCost(cost));
        }
        let incomplete = event_type == "response.incomplete"
            || response.get("status").and_then(Value::as_str) == Some("incomplete");
        let finish_reason = if incomplete {
            incomplete_finish_reason(response)
        } else if self
            .content
            .values()
            .any(|kind| *kind == ContentKind::ToolCall)
        {
            FinishReason::ToolCall
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

fn event_reasoning_content_index(value: &Value) -> Result<u32, ProviderError> {
    content_index(
        event_index(value, "output_index")?,
        optional_event_index(value, "content_index")?
            .or(optional_event_index(value, "summary_index")?)
            .unwrap_or_default(),
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
    normalized.cached_input_tokens = Some(usage.cached_input_tokens);
    normalized.cache_write_input_tokens = Some(usage.cache_write_input_tokens);
    normalized.reasoning_output_tokens = Some(usage.reasoning_output_tokens);
    normalized.image_input_tokens = Some(usage.image_input_tokens);
    normalized.image_output_tokens = Some(usage.image_output_tokens);
    normalized.total_tokens = Some(usage.total_tokens);
    normalized
}

fn provider_reported_cost(response: &Value) -> Result<Option<ProviderReportedCost>, ProviderError> {
    let Some(value) = response.pointer("/usage/cost_in_usd_ticks") else {
        return Ok(None);
    };
    let ticks = value.as_u64().ok_or_else(protocol_error_marker)?;
    // 官方 Grok REST 层会把未报告费用回填为 0，只有正数才是已报告账单
    if ticks == 0 {
        return Ok(None);
    }
    ProviderReportedCost::from_usd_ticks(u128::from(ticks))
        .map(Some)
        .map_err(protocol_error)
}

// cost_in_usd_ticks 已包含服务端工具费；缺少该字段时，只有全部按 token
// 计费的响应才能确定总价
fn token_only_output(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some(
            "message"
                | "reasoning"
                | "function_call"
                | "custom_tool_call"
                | "mcp_call"
                | "mcp_list_tools"
                | "mcp_approval_request"
                | "compaction"
        )
    )
}

fn token_only_response(response: &Value) -> bool {
    let empty = |value: &Value| value.as_object().is_some_and(serde_json::Map::is_empty);
    response.get("tool_usage").is_none_or(empty)
        && response
            .pointer("/usage/server_side_tool_usage")
            .is_none_or(empty)
        && response
            .pointer("/usage/num_server_side_tool_calls")
            .is_none_or(|value| value.as_u64() == Some(0))
        && response.get("output").is_none_or(|value| {
            value
                .as_array()
                .is_some_and(|items| items.iter().all(token_only_output))
        })
}

fn calculated_cost(
    response: &Value,
    model: &str,
    usage: TokenUsage,
    service_tier: Option<&str>,
    pricing: Option<&gateway_core::metering::ModelPriceOverride>,
) -> Option<CalculatedCost> {
    if !billable_usage_is_complete(response, usage)
        || usage.image_input_tokens > 0
        || usage.image_output_tokens > 0
    {
        return None;
    }
    let breakdown = grok_billing_breakdown_with_override(
        model,
        usage.input_tokens,
        usage.output_tokens,
        usage.cached_input_tokens,
        usage.cache_write_input_tokens,
        service_tier,
        pricing,
    )?;
    Some(breakdown.calculated_cost())
}

fn incomplete_finish_reason(response: &Value) -> FinishReason {
    match response
        .pointer("/incomplete_details/reason")
        .and_then(Value::as_str)
    {
        Some("max_output_tokens" | "max_tokens" | "max_prompt_tokens" | "max_time_limit") => {
            FinishReason::Length
        }
        Some("content_filter") => FinishReason::ContentFilter,
        _ => FinishReason::Other,
    }
}

fn upstream_event_error(value: &Value) -> ProviderError {
    let code = upstream_error_field(value, "code");
    // 顶层 type 是 SSE 事件名，只有错误对象内的 type 才属于上游错误事实
    let error_type = value
        .pointer("/error/type")
        .or_else(|| value.pointer("/response/error/type"))
        .and_then(Value::as_str);
    let message = upstream_error_field(value, "message");
    let kind = if let Some(quota) = classify_grok_quota_failure(code, error_type, message) {
        GrokInferenceTransportErrorKind::from_quota(quota)
    } else {
        match code {
            Some("invalid_request" | "invalid_prompt") => {
                GrokInferenceTransportErrorKind::InvalidRequest
            }
            Some("unsupported" | "unsupported_feature") => {
                GrokInferenceTransportErrorKind::Unsupported
            }
            Some("unauthorized" | "invalid_token") => GrokInferenceTransportErrorKind::Unauthorized,
            Some("permission_denied") => GrokInferenceTransportErrorKind::PermissionDenied,
            Some("rate_limit_exceeded") => GrokInferenceTransportErrorKind::RateLimited,
            _ => GrokInferenceTransportErrorKind::Unavailable,
        }
    };
    let mut error = ProviderError::new(kind.provider_kind(), UpstreamSendState::Sent)
        .with_diagnostic(ProviderDiagnostic::new(kind.diagnostic_message()))
        .with_raw_upstream_error(gateway_core::error::RawUpstreamError::new(
            value.to_string(),
        ))
        .with_upstream_failure_fields(gateway_core::upstream::UpstreamFailureFields {
            status: value
                .get("status")
                .or_else(|| value.get("status_code"))
                .or_else(|| value.pointer("/error/status"))
                .or_else(|| value.pointer("/response/error/status"))
                .and_then(Value::as_u64)
                .and_then(|value| u16::try_from(value).ok())
                .filter(|value| (400..=599).contains(value)),
            code: code.map(str::to_owned),
            error_type: error_type.map(str::to_owned),
            message: message.map(str::to_owned),
        });
    if let Some(code) = code {
        error = error.with_upstream_code(OpaqueUpstreamValue::new(code.to_owned()));
    }
    // 结构化 message/code/type 供原客户端展示与重试分类；message 先脱去
    // 账号指纹（上游限流文案内嵌 team UUID），非结构化正文不透出
    if let Some(message) = message {
        error = error.with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
            scrub_account_fingerprints(message),
            code.map(str::to_owned),
            error_type.map(str::to_owned),
        ));
    }
    error
}

fn upstream_error_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value
        .pointer(&format!("/error/{field}"))
        .or_else(|| value.pointer(&format!("/response/error/{field}")))
        .or_else(|| value.get(field))
        .and_then(Value::as_str)
}

fn protocol_error(error: impl std::error::Error + Send + Sync + 'static) -> ProviderError {
    protocol_error_marker().with_source(error)
}

fn protocol_error_marker() -> ProviderError {
    ProviderError::new(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
        .redact_sensitive_context("invalid upstream event")
}
