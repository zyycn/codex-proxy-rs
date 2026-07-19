//! Official Grok Responses SSE to gateway canonical events.

use std::collections::{BTreeMap, BTreeSet};

use gateway_core::accounting::{
    CalculatedCost, CalculatedCostBreakdown, CurrencyCode, Decimal, Money, ProviderReportedCost,
    Usage,
};
use gateway_core::engine::UpstreamSendState;
use gateway_core::error::{ProviderError, ProviderErrorKind};
use gateway_core::event::{
    ContentItem, ContentKind, FinishReason, GatewayEvent, ReasoningDelta, ResponseMeta, TextDelta,
    ToolCallDelta,
};
use gateway_protocol::openai::events::{TokenUsage, extract_usage};
use gateway_protocol::openai::sse::{SseEvent, SseEventDecoder};
use serde_json::Value;

const CONTENTS_PER_OUTPUT: u32 = 1_024;
const LONG_CONTEXT_THRESHOLD: u64 = 200_000;

#[derive(Clone, Copy)]
struct TokenRates {
    input_ticks: u128,
    cached_input_ticks: u128,
    output_ticks: u128,
}

#[derive(Clone, Copy)]
struct ModelPricing {
    short: TokenRates,
    long: TokenRates,
}

const GROK_45_PRICING: ModelPricing = ModelPricing {
    short: TokenRates {
        input_ticks: 20_000,
        cached_input_ticks: 3_000,
        output_ticks: 60_000,
    },
    long: TokenRates {
        input_ticks: 40_000,
        cached_input_ticks: 6_000,
        output_ticks: 120_000,
    },
};

const GROK_BUILD_PRICING: ModelPricing = ModelPricing {
    short: TokenRates {
        input_ticks: 10_000,
        cached_input_ticks: 2_000,
        output_ticks: 20_000,
    },
    long: TokenRates {
        input_ticks: 20_000,
        cached_input_ticks: 4_000,
        output_ticks: 40_000,
    },
};

const GROK_43_PRICING: ModelPricing = ModelPricing {
    short: TokenRates {
        input_ticks: 12_500,
        cached_input_ticks: 2_000,
        output_ticks: 25_000,
    },
    long: TokenRates {
        input_ticks: 25_000,
        cached_input_ticks: 4_000,
        output_ticks: 50_000,
    },
};

/// 按 xAI Provider 当前受控价格规则计算费用明细。
#[must_use]
pub fn grok_billing_breakdown(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
) -> Option<CalculatedCostBreakdown> {
    let pricing = model_pricing(model)?;
    let rates = if input_tokens >= LONG_CONTEXT_THRESHOLD {
        pricing.long
    } else {
        pricing.short
    };
    let uncached_tokens = input_tokens.checked_sub(cached_tokens)?;
    let input_amount_ticks = u128::from(uncached_tokens).checked_mul(rates.input_ticks)?;
    let cache_read_amount_ticks =
        u128::from(cached_tokens).checked_mul(rates.cached_input_ticks)?;
    let output_amount_ticks = u128::from(output_tokens).checked_mul(rates.output_ticks)?;
    let standard_amount_ticks = input_amount_ticks
        .checked_add(cache_read_amount_ticks)?
        .checked_add(output_amount_ticks)?;
    Some(CalculatedCostBreakdown::new(
        usd_money(input_amount_ticks)?,
        usd_money(output_amount_ticks)?,
        usd_money(cache_read_amount_ticks)?,
        usd_money(0)?,
        usd_money(standard_amount_ticks)?,
        usd_money(standard_amount_ticks)?,
        usd_price_per_million(rates.input_ticks)?,
        usd_price_per_million(rates.output_ticks)?,
        usd_price_per_million(rates.cached_input_ticks)?,
        usd_money(0)?,
        Some("default".to_owned()),
        100,
    ))
}

fn usd_money(ticks: u128) -> Option<Money> {
    Some(Money::new(
        Decimal::from_scaled(ticks).ok()?,
        CurrencyCode::new("USD").ok()?,
    ))
}

fn usd_price_per_million(per_token_ticks: u128) -> Option<Money> {
    usd_money(per_token_ticks.checked_mul(1_000_000)?)
}

/// Incremental decoder for one official Grok Responses attempt.
///
/// Unknown output kinds and events fail closed because gateway-core does not
/// currently have canonical representations for Grok backend tools, images,
/// searches, or MCP activity. No raw upstream data is retained in errors.
pub struct GrokCanonicalDecoder {
    decoder: SseEventDecoder,
    fallback_model: String,
    response_id: Option<String>,
    started: bool,
    completed: bool,
    content: BTreeMap<u32, ContentKind>,
    tool_arguments_seen: BTreeSet<u32>,
    usage_emitted: bool,
}

impl GrokCanonicalDecoder {
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
        let canonical = self.decode(events)?;
        if !self.completed {
            return Err(protocol_error_marker());
        }
        Ok(canonical)
    }

    fn decode(&mut self, events: Vec<SseEvent>) -> Result<Vec<GatewayEvent>, ProviderError> {
        let mut canonical = Vec::new();
        for event in events {
            let value = serde_json::from_str::<Value>(&event.data).map_err(protocol_error)?;
            let body_type = value.get("type").and_then(Value::as_str);
            let event_type = match (event.event.as_deref(), body_type) {
                (Some(header), Some(body)) if header != body => {
                    return Err(protocol_error_marker());
                }
                (Some(header), _) => header,
                (None, Some(body)) => body,
                (None, None) => return Err(protocol_error_marker()),
            };
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
            "response.function_call_arguments.delta" => self.tool_delta(value, output),
            "response.completed" | "response.incomplete" => {
                self.complete(event_type, value, output)
            }
            "response.failed" | "error" => Err(upstream_event_error(value)),
            "response.output_text.done"
            | "response.refusal.done"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.done"
            | "response.function_call_arguments.done"
            | "response.content_part.done"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_part.done"
            | "response.rate_limits.updated"
            | "rate_limits.updated" => Ok(()),
            _ => Err(unsupported_event_error()),
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
            .unwrap_or(&self.fallback_model)
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
            Some("function_call") => {
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
            Some("message") => Ok(()),
            Some(_) | None => Err(unsupported_event_error()),
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
            Some("function_call") => {}
            Some(_) | None => return Err(unsupported_event_error()),
        }
        let output_index = event_index(value, "output_index")?;
        let index = content_index(output_index, 0)?;
        if self.tool_arguments_seen.contains(&index) {
            return Ok(());
        }
        let Some(arguments) = item
            .get("arguments")
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
            Some(_) | None => return Err(unsupported_event_error()),
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
            .unwrap_or(&self.fallback_model)
            .to_owned();
        if let Some(cost) = provider_reported_cost(response)? {
            output.push(GatewayEvent::ProviderCost(cost));
        } else if let Some(cost) = usage.and_then(|usage| calculated_cost(response, &model, usage))
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
    normalized.cached_tokens = Some(usage.cached_tokens);
    normalized.cache_write_tokens = Some(usage.cache_write_tokens);
    normalized.reasoning_tokens = Some(usage.reasoning_tokens);
    normalized.total_tokens = Some(usage.total_tokens);
    normalized
}

fn provider_reported_cost(response: &Value) -> Result<Option<ProviderReportedCost>, ProviderError> {
    let Some(value) = response.pointer("/usage/cost_in_usd_ticks") else {
        return Ok(None);
    };
    let ticks = value.as_u64().ok_or_else(protocol_error_marker)?;
    if ticks == 0 {
        return Ok(None);
    }
    ProviderReportedCost::from_usd_ticks(u128::from(ticks))
        .map(Some)
        .map_err(protocol_error)
}

fn calculated_cost(response: &Value, model: &str, usage: TokenUsage) -> Option<CalculatedCost> {
    if !billable_usage_is_complete(response, usage) {
        return None;
    }
    let breakdown = grok_billing_breakdown(
        model,
        usage.input_tokens,
        usage.output_tokens,
        usage.cached_tokens,
    )?;
    Some(breakdown.calculated_cost())
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

fn model_pricing(model: &str) -> Option<ModelPricing> {
    match model {
        "grok-4.5" | "grok-4.5-latest" | "grok-4.5-build-free" | "grok-build-latest" => {
            Some(GROK_45_PRICING)
        }
        "grok-build-0.1" | "grok-code-fast-1" | "grok-code-fast" | "grok-code-fast-1-0825" => {
            Some(GROK_BUILD_PRICING)
        }
        "grok-4.3"
        | "grok-4.3-latest"
        | "grok-latest"
        | "grok-4.20-multi-agent-0309"
        | "grok-4.20-multi-agent"
        | "grok-4.20-multi-agent-latest"
        | "grok-4.20-multi-agent-beta-latest"
        | "grok-4.20-multi-agent-experimental-beta-0304"
        | "grok-4.20-multi-agent-experimental-beta-latest"
        | "grok-4.20-multi-agent-beta-0309"
        | "grok-4.20-0309-reasoning"
        | "grok-4.20-reasoning-latest"
        | "grok-4.20"
        | "grok-4.20-reasoning"
        | "grok-4.20-0309"
        | "grok-4.20-beta-0309-reasoning"
        | "grok-4.20-beta"
        | "grok-4.20-beta-0309"
        | "grok-4.20-beta-latest"
        | "grok-4.20-beta-latest-reasoning"
        | "grok-4.20-beta-reasoning"
        | "grok-4.20-experimental-beta-0304-reasoning"
        | "grok-4.20-experimental-beta-0304"
        | "grok-4.20-experimental-beta-reasoning-latest"
        | "grok-4.20-experimental-beta-latest"
        | "grok-4.20-reasoning-gv2"
        | "grok-4.20-0309-non-reasoning"
        | "grok-4.20-non-reasoning"
        | "grok-4.20-non-reasoning-latest"
        | "grok-4.20-beta-non-reasoning"
        | "grok-4.20-beta-latest-non-reasoning"
        | "grok-4.20-experimental-beta-0304-non-reasoning"
        | "grok-4.20-experimental-beta-non-reasoning-latest"
        | "grok-4.20-beta-0309-non-reasoning"
        | "grok-4.20-non-reasoning-gv2" => Some(GROK_43_PRICING),
        _ => None,
    }
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
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
    {
        Some("invalid_request" | "invalid_prompt") => ProviderErrorKind::InvalidRequest,
        Some("unsupported" | "unsupported_feature") => ProviderErrorKind::Unsupported,
        Some("unauthorized" | "invalid_token") => ProviderErrorKind::Unauthorized,
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
