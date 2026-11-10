//! Messages 原生响应保留 wire，旁路产生生命周期、用量及计费事实

use std::collections::BTreeMap;

use gateway_core::{
    event::{ContentItem, ContentKind, ReasoningDelta, TextDelta, ToolCallDelta},
    metering::Usage,
};
use gateway_protocol::{
    anthropic::MessageUsage,
    openai::sse::{SseEventDecoder, SseFrame},
};

use super::*;
use crate::provider::observation::insert_first_timing;
use crate::transport::usage::{OpenAiBillingUsage, configured_token_billing_breakdown};

// 非流式上游需要完整解码后才能交付；流式交付不受此总长限制
const MAX_MESSAGES_JSON_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct ColdMessages {
    pub client: CodexBackendClient,
    pub body: Bytes,
    pub passthrough_headers: reqwest::header::HeaderMap,
    pub context: AttemptContext,
    pub lease: Arc<CodexCredentialLease>,
    pub selector: Arc<CodexCredentialSelector>,
    pub quota: Arc<CodexCredentialQuotaService>,
    pub model: UpstreamModelId,
    pub response_origin: Url,
    pub count_protocol: Option<&'static str>,
    pub fast_requested: bool,
}

pub(super) fn cold_messages_stream(mut request: ColdMessages) -> EventStream {
    Box::pin(async_stream::try_stream! {
        let failure_context = OpenAiFailureContext {
            client: &request.client, selector: &request.selector, quota: &request.quota,
            response_origin: &request.response_origin, cyber_policy_scope: None,
            allows_account_state_mutation: request.lease.allows_account_state_mutation(),
            image_endpoint: false, upstream_model: Some(&request.model),
        };
        let authorization = request.lease.authentication().authorization_header().map_err(|_| invalid_request())?;
        let trace = request.context.trace().clone();
        let upstream_context = CodexRequestContext::auxiliary(authorization.expose_secret(), None,
            request.context.request_id().as_str(), None).with_trace(&trace);
        let endpoint = if request.count_protocol.is_some() { "/messages/count_tokens" } else { "/messages" };
        let response = tokio::select! {
            biased;
            _ = request.context.cancellation().cancelled() => Err(provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::NotSent)),
            _ = request.context.deadline().wait() => Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::NotSent)),
            response = request.client.post_messages(endpoint, std::mem::take(&mut request.body), &request.passthrough_headers, upstream_context) => {
                match response {
                    Ok(response) => Ok(response),
                    Err(error) => {
                        let failure = map_client_error(error, UpstreamSendState::Sent, None);
                        apply_failure(&failure_context, request.lease.account(), &failure).await;
                        Err(failure.error)
                    }
                }
            }
        }?;
        let transport = if response.streaming { CodexBackendTransport::HttpSse } else { CodexBackendTransport::HttpJson };
        let observation = |timings| codex_response_observation(transport, &response.diagnostics,
            &response.response_metadata, &response.transport_metrics, None, timings);
        let mut timings = openai_response_timings(&response.transport_metrics, &response.response_metadata);
        if let Some(snapshot) = observation(timings) {
            yield ProviderEvent::observation(snapshot);
        }
        let streaming = response.streaming;
        let mut body = response.body;
        let mut decoder = SseEventDecoder::default();
        let mut json = Vec::new();
        let mut state = MessageState::default();
        loop {
            let next = tokio::select! {
                biased;
                _ = request.context.cancellation().cancelled() => Err(provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::Sent)),
                _ = request.context.deadline().wait() => Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::Sent)),
                chunk = body.next() => match chunk {
                    Some(Ok(bytes)) => Ok(Some(bytes)),
                    Some(Err(error)) => Err(map_stream_error(error).error),
                    None => Ok(None),
                },
            }?;
            let Some(bytes) = next else { break; };
            trace.capture("upstream.response.body", &bytes);
            if !bytes.is_empty() && insert_first_timing(&mut timings.first_event_ms, request.context.timing_started_at())
                && let Some(snapshot) = observation(timings) {
                yield ProviderEvent::observation(snapshot);
            }
            if streaming {
                for frame in decoder.push_frames(&bytes) {
                    let event = observed_frame(&mut state, frame, &request).await?;
                    if observe_content_timings(&event, &mut timings, request.context.timing_started_at())
                        && let Some(snapshot) = observation(timings) {
                        yield ProviderEvent::observation(snapshot);
                    }
                    yield event;
                    if state.completed { break; }
                }
                if state.completed { break; }
            } else {
                if bytes.len() > MAX_MESSAGES_JSON_BYTES - json.len() { Err(protocol_error())?; }
                json.extend_from_slice(&bytes);
            }
        }
        if streaming {
            if !state.completed {
                for frame in decoder.finish_frames() {
                    let event = observed_frame(&mut state, frame, &request).await?;
                    if observe_content_timings(&event, &mut timings, request.context.timing_started_at())
                        && let Some(snapshot) = observation(timings) {
                        yield ProviderEvent::observation(snapshot);
                    }
                    yield event;
                    if state.completed { break; }
                }
            }
            if !state.completed { Err(protocol_error())?; }
        } else {
            let value: Value = serde_json::from_slice(&json).map_err(|_| protocol_error())?;
            if value.get("type").and_then(Value::as_str) == Some("error") {
                let error = message_error(&value);
                record_message_failure(&request, &error).await;
                Err(error)?;
            }
            let meta = if request.count_protocol.is_some() {
                if value.get("input_tokens").and_then(Value::as_u64).is_none() { Err(protocol_error())?; }
                ResponseMeta::new(request.context.request_id().as_str(), request.model.as_str())
            } else {
                message_meta(&value)?
            };
            let wire = ProtocolWireEvent::raw_json(
                request.count_protocol.unwrap_or(ANTHROPIC_PROTOCOL), json.into(),
            ).map_err(|_| protocol_error())?;
            // 原生出口只交付 wire，开始事实必须附在可交付正文上，避免提前提交空批次
            let mut event = ProviderEvent::wire(wire).with_fact(GatewayEvent::Started(meta.clone()));
            if !request.count_protocol.is_some() {
                state.usage.merge(&value["usage"]);
                event = with_usage(event, state.usage);
                let unpriced = unpriced_usage(&value["usage"]) || value.get("content").and_then(Value::as_array).is_some_and(|blocks| {
                    blocks.iter().any(|block| block.get("type").and_then(Value::as_str) == Some("server_tool_use"))
                });
                event = with_cost(event, state.usage, unpriced, &request);
                if let Some(blocks) = value.get("content").and_then(Value::as_array) {
                    for block in blocks {
                        let target = match block.get("type").and_then(Value::as_str) {
                            Some("text") if block.get("text").and_then(Value::as_str).is_some_and(|text| !text.is_empty()) => Some(&mut timings.first_text_ms),
                            Some("thinking" | "redacted_thinking") => Some(&mut timings.first_reasoning_ms),
                            Some("tool_use" | "server_tool_use") => Some(&mut timings.first_token_ms),
                            _ => None,
                        };
                        if let Some(target) = target {
                            insert_first_timing(target, request.context.timing_started_at());
                            insert_first_timing(&mut timings.first_token_ms, request.context.timing_started_at());
                        }
                    }
                }
                if let Some(snapshot) = observation(timings) {
                    event.attach_observation(snapshot);
                }
            }
            yield event.with_fact(GatewayEvent::Completed(meta.with_finish_reason(finish_reason(value.get("stop_reason").and_then(Value::as_str)))));
        }
    })
}

// Messages 的首字从可消费内容开始，ping、消息元数据和空增量不计入输出延迟
fn observe_content_timings(
    event: &ProviderEvent,
    timings: &mut ProviderResponseTimings,
    started: Instant,
) -> bool {
    let mut changed = false;
    for fact in event.canonical_facts() {
        let target = match fact {
            GatewayEvent::TextDelta(delta) if !delta.text.is_empty() => {
                Some(&mut timings.first_text_ms)
            }
            GatewayEvent::ReasoningDelta(delta) if !delta.text.is_empty() => {
                Some(&mut timings.first_reasoning_ms)
            }
            GatewayEvent::ToolCallDelta(delta)
                if delta.name.is_some() || !delta.arguments_delta.is_empty() =>
            {
                Some(&mut timings.first_token_ms)
            }
            _ => None,
        };
        if let Some(target) = target {
            changed |= insert_first_timing(target, started);
            changed |= insert_first_timing(&mut timings.first_token_ms, started);
        }
    }
    changed
}

#[derive(Default)]
struct MessageState {
    meta: Option<ResponseMeta>,
    usage: MessageUsage,
    reason: Option<String>,
    tools: BTreeMap<u32, String>,
    completed: bool,
    unpriced_usage: bool,
}

impl MessageState {
    fn frame(
        &mut self,
        frame: SseFrame,
        request: &ColdMessages,
    ) -> Result<ProviderEvent, ProviderError> {
        let (raw, events) = frame.into_parts();
        let Some(sse) = events.into_iter().next() else {
            return ProtocolWireEvent::raw_sse(ANTHROPIC_PROTOCOL, raw.into())
                .map(ProviderEvent::wire)
                .map_err(|_| protocol_error());
        };
        let Ok(value) = serde_json::from_str::<Value>(&sse.data) else {
            return ProtocolWireEvent::raw_sse(ANTHROPIC_PROTOCOL, raw.into())
                .map(ProviderEvent::wire)
                .map_err(|_| protocol_error());
        };
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .or(sse.event.as_deref())
            .map(str::to_owned);
        if kind.as_deref() == Some("error") {
            return Err(message_error(&value));
        }
        let wire = ProtocolWireEvent::json_with_raw_sse_metadata(
            ANTHROPIC_PROTOCOL,
            sse.event,
            value.clone(),
            raw.into(),
            sse.id,
            sse.retry,
        )
        .map_err(|_| protocol_error())?;
        let mut event = ProviderEvent::wire(wire);
        let index = value
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_default();
        match kind.as_deref() {
            Some("message_start") => {
                if self.meta.is_some() {
                    return Err(protocol_error());
                }
                let meta = message_meta(&value["message"])?;
                self.meta = Some(meta.clone());
                self.usage.merge(&value["message"]["usage"]);
                self.unpriced_usage |= unpriced_usage(&value["message"]["usage"]);
                event = with_usage(event.with_fact(GatewayEvent::Started(meta)), self.usage);
            }
            Some("content_block_start") => {
                let block = &value["content_block"];
                self.unpriced_usage |=
                    block.get("type").and_then(Value::as_str) == Some("server_tool_use");
                let kind = match block.get("type").and_then(Value::as_str) {
                    Some("thinking" | "redacted_thinking") => ContentKind::Reasoning,
                    Some("tool_use" | "server_tool_use") => ContentKind::ToolCall,
                    _ => ContentKind::Text,
                };
                event = event.with_fact(GatewayEvent::ContentAdded(ContentItem::new(index, kind)));
                if kind == ContentKind::ToolCall {
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    self.tools.insert(index, id.clone());
                    event = event.with_fact(GatewayEvent::ToolCallDelta(ToolCallDelta {
                        content_index: index,
                        call_id: id,
                        name: block.get("name").and_then(Value::as_str).map(str::to_owned),
                        arguments_delta: String::new(),
                    }));
                }
            }
            Some("content_block_delta") => {
                let delta = &value["delta"];
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        event = event.with_fact(GatewayEvent::TextDelta(TextDelta {
                            content_index: index,
                            text: delta["text"].as_str().unwrap_or_default().to_owned(),
                        }))
                    }
                    Some("thinking_delta") => {
                        event = event.with_fact(GatewayEvent::ReasoningDelta(ReasoningDelta {
                            content_index: index,
                            text: delta["thinking"].as_str().unwrap_or_default().to_owned(),
                        }))
                    }
                    Some("input_json_delta") => {
                        event = event.with_fact(GatewayEvent::ToolCallDelta(ToolCallDelta {
                            content_index: index,
                            call_id: self.tools.get(&index).cloned().unwrap_or_default(),
                            name: None,
                            arguments_delta: delta["partial_json"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned(),
                        }))
                    }
                    _ => {}
                }
            }
            Some("message_delta") => {
                self.usage.merge(&value["usage"]);
                self.unpriced_usage |= unpriced_usage(&value["usage"]);
                self.reason = value
                    .pointer("/delta/stop_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or(self.reason.take());
                // 结束用量先于 message_stop 到达，费用必须同时记录，避免随后截断或取消丢账
                event = with_cost(
                    with_usage(event, self.usage),
                    self.usage,
                    self.unpriced_usage,
                    request,
                );
            }
            Some("message_stop") => {
                let meta = self.meta.clone().ok_or_else(protocol_error)?;
                self.completed = true;
                event = with_cost(event, self.usage, self.unpriced_usage, request).with_fact(
                    GatewayEvent::Completed(
                        meta.with_finish_reason(finish_reason(self.reason.as_deref())),
                    ),
                );
            }
            _ => {}
        }
        Ok(event)
    }
}

fn message_meta(value: &Value) -> Result<ResponseMeta, ProviderError> {
    Ok(ResponseMeta::new(
        value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(protocol_error)?,
        value
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(protocol_error)?,
    ))
}

fn finish_reason(value: Option<&str>) -> FinishReason {
    match value {
        Some("max_tokens" | "model_context_window_exceeded") => FinishReason::Length,
        Some("tool_use") => FinishReason::ToolCall,
        Some("refusal") => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

fn with_usage(event: ProviderEvent, usage: MessageUsage) -> ProviderEvent {
    event.with_fact(GatewayEvent::Usage(Usage {
        input_tokens: usage.total_input_tokens(),
        output_tokens: usage.output_tokens,
        cached_input_tokens: usage.cache_read_input_tokens,
        cache_write_input_tokens: usage.cache_creation_input_tokens,
        ..Usage::new()
    }))
}

fn with_cost(
    event: ProviderEvent,
    usage: MessageUsage,
    unpriced: bool,
    request: &ColdMessages,
) -> ProviderEvent {
    if unpriced || request.fast_requested {
        return event;
    }
    // Messages 的计价只能来自显式覆盖；模型别名不能隐式借用 OpenAI 内置价格
    let Some(pricing) = request
        .context
        .pricing()
        .get(PROVIDER_NAME)
        .and_then(|models| models.get(request.model.as_str()))
    else {
        return event;
    };
    let (Some(input_tokens), Some(output_tokens)) =
        (usage.total_input_tokens(), usage.output_tokens)
    else {
        return event;
    };
    let usage = OpenAiBillingUsage::from(gateway_protocol::openai::events::TokenUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens: usage.cache_read_input_tokens.unwrap_or_default(),
        cache_write_input_tokens: usage.cache_creation_input_tokens.unwrap_or_default(),
        ..Default::default()
    });
    match configured_token_billing_breakdown(usage, pricing) {
        Some(cost) => event.with_fact(GatewayEvent::CalculatedCost(cost.calculated_cost())),
        None => event,
    }
}

fn unpriced_usage(usage: &Value) -> bool {
    usage.get("speed").and_then(Value::as_str) == Some("fast")
        || usage
            .pointer("/cache_creation/ephemeral_1h_input_tokens")
            .and_then(Value::as_u64)
            .is_some_and(|tokens| tokens > 0)
        || usage
            .get("server_tool_use")
            .and_then(Value::as_object)
            .is_some_and(|tools| {
                tools
                    .values()
                    .any(|value| value.as_u64().is_some_and(|count| count > 0))
            })
}

fn message_error(value: &Value) -> ProviderError {
    let error = &value["error"];
    let error_type = error.get("type").and_then(Value::as_str);
    let kind = match error_type {
        Some("authentication_error") => ProviderErrorKind::Unauthorized,
        Some("rate_limit_error") => ProviderErrorKind::RateLimited,
        Some("invalid_request_error") => ProviderErrorKind::InvalidRequest,
        Some("overloaded_error") => ProviderErrorKind::UpstreamCapacityUnavailable,
        Some("api_error") => ProviderErrorKind::Unavailable,
        _ => ProviderErrorKind::Protocol,
    };
    provider_error(kind, UpstreamSendState::Sent).with_client_visible_upstream_error(
        ClientVisibleUpstreamError::new(
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("The Messages upstream returned an error."),
            None,
            error_type.map(str::to_owned),
        ),
    )
}

async fn observed_frame(
    state: &mut MessageState,
    frame: SseFrame,
    request: &ColdMessages,
) -> Result<ProviderEvent, ProviderError> {
    match state.frame(frame, request) {
        Ok(event) => Ok(event),
        Err(error) => {
            record_message_failure(request, &error).await;
            Err(error)
        }
    }
}

async fn record_message_failure(request: &ColdMessages, error: &ProviderError) {
    if !request.lease.allows_account_state_mutation() {
        return;
    }
    let failure = match error.kind() {
        ProviderErrorKind::Unauthorized => Some(CodexAccountFailure::CredentialExpired),
        ProviderErrorKind::RateLimited => Some(CodexAccountFailure::RateLimited {
            retry_after: error.retry_after(),
        }),
        _ => None,
    };
    if let Some(failure) = failure {
        // 账号写回复用 selector 的凭据 revision 校验，流内错误不能污染已轮换的凭据
        if let Err(error) = request
            .selector
            .record_failure(request.lease.account(), failure, None)
            .await
        {
            tracing::warn!(account_id = %request.lease.account_id(), error = %error, "Failed to record Messages account failure");
        }
    }
}
