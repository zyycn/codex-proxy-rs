//! 校验并解码插件上游事件，转换为宿主执行事件与错误

use std::time::Duration;

use bytes::Bytes;
use gateway_core::{
    engine::upstream_adapter::UpstreamAccountConnection,
    error::{ClientVisibleUpstreamError, ProviderError, ProviderErrorKind},
    event::{
        ContentItem, ContentKind, EventSequenceValidator, FinishReason, GatewayEvent,
        ProtocolWireEvent, ProviderEvent, ProviderResponseObservation, ReasoningDelta,
        ResponseMeta, TextDelta, ToolCallDelta,
    },
    metering::Usage,
    upstream::{OpaqueUpstreamValue, UpstreamSendState, UpstreamTransport},
};
use gateway_plugin_sdk::call::{
    model::{CanonicalEvent, WireEvent, WirePayload},
    upstream_adapter::{
        UpstreamAdapterEvent, UpstreamContinuation, UpstreamFailure, UpstreamFailureKind,
    },
};
use gateway_protocol::openai::sse::SseEventDecoder;

#[derive(Default)]
pub(super) struct EventDecoder {
    sequence: EventSequenceValidator,
    response_id: Option<String>,
    service_tier: Option<String>,
    usage: Usage,
    contents: usize,
    pub(super) completed: bool,
}

pub(super) struct DecodedEvent {
    pub(super) event: ProviderEvent,
    pub(super) continuation: Option<UpstreamContinuation>,
}

pub(super) enum EventDecodeError {
    Invalid(ProviderError),
    Upstream(ProviderError),
}

impl From<ProviderError> for EventDecodeError {
    fn from(error: ProviderError) -> Self {
        Self::Invalid(error)
    }
}

impl EventDecoder {
    pub(super) fn decode(
        &mut self,
        payload: &[u8],
        protocol: &str,
        transport: &str,
        account: &dyn UpstreamAccountConnection,
        sent: UpstreamSendState,
    ) -> Result<DecodedEvent, EventDecodeError> {
        let message = UpstreamAdapterEvent::decode(payload).map_err(|_| invalid(sent))?;
        if self.completed || message.event.facts.len() > 64 {
            return Err(invalid(sent).into());
        }
        let wire = message
            .event
            .wire
            .map(|wire| decode_wire(wire, protocol, sent))
            .transpose()?;
        if let Some(failure) = message.failure {
            if !message.event.facts.is_empty() || message.continuation.is_some() {
                return Err(invalid(sent).into());
            }
            let mut error = failure_error(failure, sent)?;
            if let Some(wire) = wire {
                error = error.with_atomic_client_events(vec![ProviderEvent::wire(wire)]);
            }
            return Err(EventDecodeError::Upstream(error));
        }
        if let Some(tier) = message.service_tier {
            if tier.is_empty() || tier.len() > 64 || tier.chars().any(char::is_control) {
                return Err(invalid(sent).into());
            }
            // 上游可能在完成时才把 auto 解析为实际档位；与原生观测一样保留最新值
            self.service_tier = Some(tier);
        }
        let mut facts = Vec::new();
        for fact in message.event.facts {
            let fact = match fact {
                CanonicalEvent::Started { id, model } => {
                    validate_id(&id, sent)?;
                    validate_model(model.as_deref(), sent)?;
                    self.response_id = Some(id.clone());
                    GatewayEvent::Started(meta(id, model))
                }
                CanonicalEvent::ContentAdded { index, kind } => {
                    self.contents += 1;
                    if self.contents > 4096 {
                        return Err(invalid(sent).into());
                    }
                    GatewayEvent::ContentAdded(ContentItem::new(
                        index,
                        match kind {
                            gateway_plugin_sdk::call::model::ContentKind::Text => ContentKind::Text,
                            gateway_plugin_sdk::call::model::ContentKind::Reasoning => {
                                ContentKind::Reasoning
                            }
                            gateway_plugin_sdk::call::model::ContentKind::ToolCall => {
                                ContentKind::ToolCall
                            }
                            gateway_plugin_sdk::call::model::ContentKind::Image => {
                                ContentKind::Image
                            }
                            gateway_plugin_sdk::call::model::ContentKind::Audio => {
                                ContentKind::Audio
                            }
                        },
                    ))
                }
                CanonicalEvent::TextDelta { index, text } => GatewayEvent::TextDelta(TextDelta {
                    content_index: index,
                    text,
                }),
                CanonicalEvent::ReasoningDelta { index, text } => {
                    GatewayEvent::ReasoningDelta(ReasoningDelta {
                        content_index: index,
                        text,
                    })
                }
                CanonicalEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments,
                } => {
                    validate_id(&id, sent)?;
                    if name
                        .as_ref()
                        .is_some_and(|name| name.len() > 512 || name.chars().any(char::is_control))
                    {
                        return Err(invalid(sent).into());
                    }
                    GatewayEvent::ToolCallDelta(ToolCallDelta {
                        content_index: index,
                        call_id: id,
                        name,
                        arguments_delta: arguments,
                    })
                }
                CanonicalEvent::Usage { usage } => {
                    if [
                        usage.input_tokens,
                        usage.output_tokens,
                        usage.cached_tokens,
                        usage.cache_write_tokens,
                        usage.reasoning_tokens,
                        usage.image_input_tokens,
                        usage.image_output_tokens,
                        usage.total_tokens,
                    ]
                    .into_iter()
                    .flatten()
                    .any(|count| count > i64::MAX as u64)
                    {
                        return Err(invalid(sent).into());
                    }
                    let usage = Usage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cached_tokens: usage.cached_tokens,
                        cache_write_tokens: usage.cache_write_tokens,
                        reasoning_tokens: usage.reasoning_tokens,
                        image_input_tokens: usage.image_input_tokens,
                        image_output_tokens: usage.image_output_tokens,
                        total_tokens: usage.total_tokens,
                    };
                    self.usage.merge(&usage);
                    GatewayEvent::Usage(usage)
                }
                CanonicalEvent::Completed { id, model, reason } => {
                    validate_model(model.as_deref(), sent)?;
                    if self.response_id.as_deref() != Some(id.as_str()) {
                        return Err(invalid(sent).into());
                    }
                    if let Some(cost) =
                        account.calculate_cost(self.service_tier.as_deref(), &self.usage)
                    {
                        let cost = GatewayEvent::CalculatedCost(cost);
                        self.sequence.observe(&cost).map_err(|_| invalid(sent))?;
                        facts.push(cost);
                    }
                    self.completed = true;
                    GatewayEvent::Completed(meta(id, model).with_finish_reason(match reason {
                        gateway_plugin_sdk::call::model::FinishReason::Stop => FinishReason::Stop,
                        gateway_plugin_sdk::call::model::FinishReason::Length => {
                            FinishReason::Length
                        }
                        gateway_plugin_sdk::call::model::FinishReason::ToolCall => {
                            FinishReason::ToolCall
                        }
                        gateway_plugin_sdk::call::model::FinishReason::ContentFilter => {
                            FinishReason::ContentFilter
                        }
                        gateway_plugin_sdk::call::model::FinishReason::Other => FinishReason::Other,
                    }))
                }
            };
            self.sequence.observe(&fact).map_err(|_| invalid(sent))?;
            facts.push(fact);
        }
        if message.continuation.is_some() && !self.completed {
            return Err(invalid(sent).into());
        }
        let mut event = if let Some(wire) = wire {
            ProviderEvent::canonical_with_wire(facts, wire)
        } else {
            let mut facts = facts.into_iter();
            let first = facts.next().ok_or_else(|| invalid(sent))?;
            facts.fold(ProviderEvent::canonical(first), ProviderEvent::with_fact)
        };
        if let Some(tier) = &self.service_tier {
            let observation = ProviderResponseObservation::new(
                UpstreamTransport::new(transport).map_err(|_| invalid(sent))?,
            )
            .try_with_service_tier(tier.clone())
            .map_err(|_| invalid(sent))?;
            event.attach_observation(observation);
        }
        Ok(DecodedEvent {
            event,
            continuation: message.continuation,
        })
    }

    pub(super) fn finish(&self, sent: UpstreamSendState) -> Result<(), ProviderError> {
        self.sequence.finish().map_err(|_| invalid(sent))
    }
}

fn meta(id: String, model: Option<String>) -> ResponseMeta {
    model.map_or_else(
        || ResponseMeta::for_provider_endpoint(id.clone()),
        |model| ResponseMeta::new(id.clone(), model),
    )
}

fn validate_id(id: &str, sent: UpstreamSendState) -> Result<(), ProviderError> {
    if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
        return Err(invalid(sent));
    }
    Ok(())
}

fn validate_model(model: Option<&str>, sent: UpstreamSendState) -> Result<(), ProviderError> {
    if model.is_some_and(|model| {
        model.is_empty() || model.len() > 256 || model.chars().any(char::is_control)
    }) {
        return Err(invalid(sent));
    }
    Ok(())
}

fn decode_wire(
    wire: WireEvent,
    protocol: &str,
    sent: UpstreamSendState,
) -> Result<ProtocolWireEvent, ProviderError> {
    if wire.protocol != protocol {
        return Err(invalid(sent));
    }
    let result = match wire.payload {
        WirePayload::Json {
            event,
            data,
            id,
            retry,
            raw_sse,
        } => {
            if let Some(raw) = raw_sse {
                let frame = single_sse(&raw, sent)?;
                let [parsed] = frame.events() else {
                    return Err(invalid(sent));
                };
                if parsed.event != event
                    || parsed.id != id
                    || parsed.retry != retry
                    || serde_json::from_str::<serde_json::Value>(&parsed.data)
                        .ok()
                        .as_ref()
                        != Some(&data)
                {
                    return Err(invalid(sent));
                }
                ProtocolWireEvent::json_with_raw_sse_metadata(
                    protocol,
                    event,
                    data,
                    Bytes::from(raw),
                    id,
                    retry,
                )
            } else {
                ProtocolWireEvent::json_with_sse_metadata(protocol, event, data, id, retry)
            }
        }
        WirePayload::RawSse { frame } => {
            single_sse(&frame, sent)?;
            ProtocolWireEvent::raw_sse(protocol, Bytes::from(frame))
        }
        WirePayload::RawJson { body } => {
            serde_json::from_slice::<serde_json::Value>(&body).map_err(|_| invalid(sent))?;
            ProtocolWireEvent::raw_json(protocol, Bytes::from(body))
        }
        WirePayload::RawBody { body } => {
            ProtocolWireEvent::raw_http_body(protocol, Bytes::from(body))
        }
    };
    result.map_err(|_| invalid(sent))
}

fn single_sse(
    bytes: &[u8],
    sent: UpstreamSendState,
) -> Result<gateway_protocol::openai::sse::SseFrame, ProviderError> {
    let mut frames = SseEventDecoder::default().push_frames(bytes);
    if frames.len() != 1 || frames[0].raw() != bytes || std::str::from_utf8(bytes).is_err() {
        return Err(invalid(sent));
    }
    frames.pop().ok_or_else(|| invalid(sent))
}

fn failure_error(
    failure: UpstreamFailure,
    sent: UpstreamSendState,
) -> Result<ProviderError, ProviderError> {
    if failure.message.len() > 64 * 1024
        || failure.code.as_ref().is_some_and(|code| code.len() > 256)
        || failure
            .status
            .is_some_and(|status| !(400..=599).contains(&status))
        || failure
            .retry_after_ms
            .is_some_and(|delay| delay > 24 * 60 * 60 * 1000)
    {
        return Err(invalid(sent));
    }
    let kind = match failure.kind {
        UpstreamFailureKind::InvalidRequest => ProviderErrorKind::InvalidRequest,
        UpstreamFailureKind::Unsupported => ProviderErrorKind::Unsupported,
        UpstreamFailureKind::Unauthorized => ProviderErrorKind::Unauthorized,
        UpstreamFailureKind::PermissionDenied => ProviderErrorKind::PermissionDenied,
        UpstreamFailureKind::RateLimited => ProviderErrorKind::RateLimited,
        UpstreamFailureKind::QuotaExhausted => ProviderErrorKind::QuotaExhausted,
        UpstreamFailureKind::Timeout => ProviderErrorKind::Timeout,
        UpstreamFailureKind::Unavailable => ProviderErrorKind::Unavailable,
        UpstreamFailureKind::Protocol => ProviderErrorKind::Protocol,
    };
    let mut error = ProviderError::new(kind, sent);
    if let Some(code) = &failure.code {
        error = error.with_upstream_code(OpaqueUpstreamValue::new(code.clone()));
    }
    error = error.with_client_visible_upstream_error(ClientVisibleUpstreamError::new(
        failure.message,
        failure.code,
        None,
    ));
    if let Some(status) = failure.status {
        error = error.with_status(status);
    }
    if let Some(delay) = failure.retry_after_ms {
        error = error.with_retry_after(Duration::from_millis(delay));
    }
    Ok(error)
}

pub(super) fn invalid(sent: UpstreamSendState) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Protocol, sent)
}
