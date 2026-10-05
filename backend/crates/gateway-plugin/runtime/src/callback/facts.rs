//! 将宿主执行、会话、费用与观测事实投影为插件线协议值

use gateway_core::{
    engine::provider::ProviderCallMetadata,
    event::{
        ContentKind, FinishReason, GatewayEvent, ProtocolWireEvent, ProviderEvent,
        ProviderResponseObservation,
    },
    metering::{CalculatedCostBreakdown, Money},
    operation::ProviderSessionState,
};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        middleware::MiddlewareHeader,
        model::{
            CanonicalEvent, ContentKind as WireContentKind, ExecutionEvent,
            FinishReason as WireFinishReason, Usage as WireUsage, WireEvent, WirePayload,
            facts::{self, ExecutionCost, ExecutionFacts},
        },
        observation::{RequestMoney, RequestTimings},
    },
};

pub(crate) fn execution(event: &ProviderEvent) -> Result<ExecutionFacts, PluginFault> {
    Ok(ExecutionFacts {
        costs: event
            .canonical_facts()
            .iter()
            .filter_map(|fact| match fact {
                GatewayEvent::CalculatedCost(cost) => {
                    let estimate = cost.clone().into_estimate();
                    Some(ExecutionCost::Calculated {
                        total: money(cost.total()),
                        breakdown: estimate.breakdown().map(breakdown).map(Box::new),
                    })
                }
                GatewayEvent::ProviderCost(cost) => Some(ExecutionCost::ProviderReported {
                    total: money(cost.total()),
                }),
                _ => None,
            })
            .collect(),
        observation: event.response_observation().map(observation),
        session_update: event.session_update().map(session),
        middleware_transformed: event.middleware_transformed(),
        middleware_origin_wire: event
            .middleware_origin_wire()
            .map(project_wire)
            .transpose()?,
    })
}

pub(crate) fn metadata(value: &ProviderCallMetadata) -> facts::ProviderCallMetadata {
    facts::ProviderCallMetadata {
        provider: value.provider().as_str().to_owned(),
        upstream_model: value
            .upstream_model()
            .map(|value| value.as_str().to_owned()),
        provider_account_id: value.provider_account_id().as_str().to_owned(),
        upstream_request_id: value
            .upstream_request_id()
            .map(|value| value.as_str().to_owned()),
        transport: value.transport().as_str().to_owned(),
        selection_observation: value.selection_observation().map(|value| {
            facts::SelectionObservation {
                account_selection_wait_ms: value.account_selection_wait_ms(),
                capacity: value.capacity().map(|value| facts::AccountCapacity {
                    used_slots: value.used_slots(),
                    total_slots: value.total_slots(),
                }),
            }
        }),
    }
}

fn money(value: Money) -> RequestMoney {
    RequestMoney {
        amount: value.amount().to_string(),
        currency: value.currency().as_str().to_owned(),
    }
}

fn breakdown(value: &CalculatedCostBreakdown) -> facts::CostBreakdown {
    facts::CostBreakdown {
        long_context_billing_applied: value.long_context_billing_applied(),
        image: value.image().map(|value| facts::ImageCostBreakdown {
            input_tokens: value.input_tokens,
            cached_tokens: value.cached_tokens,
            input_amount: money(value.input_amount),
            cache_read_amount: money(value.cache_read_amount),
            input_price_per_million: money(value.input_price_per_million),
            cache_read_price_per_million: money(value.cache_read_price_per_million),
        }),
        input_amount: money(value.input_amount()),
        output_amount: money(value.output_amount()),
        cache_read_amount: money(value.cache_read_amount()),
        cache_write_amount: money(value.cache_write_amount()),
        standard_amount: money(value.standard_amount()),
        total_amount: money(value.total_amount()),
        input_price_per_million: money(value.input_price_per_million()),
        output_price_per_million: money(value.output_price_per_million()),
        cache_read_price_per_million: money(value.cache_read_price_per_million()),
        cache_write_price_per_million: money(value.cache_write_price_per_million()),
        service_tier: value.service_tier().map(str::to_owned),
        multiplier_percent: value.multiplier_percent(),
        custom_multiplier_bps: value.custom_multiplier_bps(),
    }
}

fn observation(value: &ProviderResponseObservation) -> facts::ResponseObservation {
    let timings = value.timings();
    facts::ResponseObservation {
        transport: value.transport().as_str().to_owned(),
        http_version: value.http_version().map(|value| value.as_str().to_owned()),
        websocket_pool: value
            .websocket_pool()
            .map(|value| value.as_str().to_owned()),
        status_code: value.status_code(),
        request_id: value.request_id().map(|value| value.as_str().to_owned()),
        service_tier: value.service_tier().map(str::to_owned),
        upstream_response_model: value.upstream_response_model().map(str::to_owned),
        timings: RequestTimings {
            transport_decision_wait_ms: timings.transport_decision_wait_ms,
            connect_ms: timings.connect_ms,
            headers_ms: timings.headers_ms,
            first_event_ms: timings.first_event_ms,
            first_reasoning_ms: timings.first_reasoning_ms,
            first_text_ms: timings.first_text_ms,
            first_token_ms: timings.first_token_ms,
            provider_processing_ms: timings.provider_processing_ms,
            latency_ms: None,
        },
        client_headers: value
            .client_headers()
            .iter()
            .map(|value| MiddlewareHeader {
                name: value.name().to_owned(),
                value: value.value().to_vec(),
            })
            .collect(),
        provider_metadata: value
            .provider_metadata()
            .map(|value| value.as_json().to_owned()),
    }
}

fn session(value: &ProviderSessionState) -> facts::SessionState {
    facts::SessionState {
        provider: value.provider().to_owned(),
        payload: value.payload().clone(),
        extension_owner: value.extension_owner().map(|value| facts::SessionOwner {
            instance_id: value.instance_id.clone(),
            contribution_id: value.contribution_id.clone(),
            adapter_id: value.adapter_id.clone(),
            generation: value.generation,
            incarnation: value.incarnation.clone(),
            connection_local: value.connection_local,
        }),
    }
}

pub(crate) fn event(event: ProviderEvent) -> Result<ExecutionEvent, PluginFault> {
    let host = Some(Box::new(execution(&event)?));
    let (facts, wire) = event.into_parts();
    Ok(ExecutionEvent {
        facts: facts
            .into_iter()
            .filter_map(project_fact)
            .collect::<Result<Vec<_>, _>>()?,
        wire: wire.as_ref().map(project_wire).transpose()?,
        host,
    })
}

pub(crate) fn snapshot(event: &ProviderEvent) -> Result<ExecutionEvent, PluginFault> {
    let facts = event
        .canonical_facts()
        .iter()
        .cloned()
        .filter_map(project_fact)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ExecutionEvent {
        facts,
        wire: event.wire_event().map(project_wire).transpose()?,
        host: Some(Box::new(execution(event)?)),
    })
}

fn project_fact(event: GatewayEvent) -> Option<Result<CanonicalEvent, PluginFault>> {
    Some(Ok(match event {
        GatewayEvent::Started(meta) => CanonicalEvent::Started {
            id: meta.response_id().to_owned(),
            model: meta.model().map(str::to_owned),
        },
        GatewayEvent::ContentAdded(item) => CanonicalEvent::ContentAdded {
            index: item.index(),
            kind: match item.kind() {
                ContentKind::Text => WireContentKind::Text,
                ContentKind::Reasoning => WireContentKind::Reasoning,
                ContentKind::ToolCall => WireContentKind::ToolCall,
                ContentKind::Image => WireContentKind::Image,
                ContentKind::Audio => WireContentKind::Audio,
                _ => {
                    return Some(Err(PluginFault::new(
                        ErrorCode::Unsupported,
                        "unknown content kind",
                    )));
                }
            },
        },
        GatewayEvent::TextDelta(delta) => CanonicalEvent::TextDelta {
            index: delta.content_index,
            text: delta.text,
        },
        GatewayEvent::ReasoningDelta(delta) => CanonicalEvent::ReasoningDelta {
            index: delta.content_index,
            text: delta.text,
        },
        GatewayEvent::ToolCallDelta(delta) => CanonicalEvent::ToolCallDelta {
            index: delta.content_index,
            id: delta.call_id,
            name: delta.name,
            arguments: delta.arguments_delta,
        },
        GatewayEvent::Usage(usage) => CanonicalEvent::Usage {
            usage: WireUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cached_tokens: usage.cached_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                reasoning_tokens: usage.reasoning_tokens,
                image_input_tokens: usage.image_input_tokens,
                image_output_tokens: usage.image_output_tokens,
                total_tokens: usage.total_tokens,
            },
        },
        // 费用随 host 快照完整开放，结算仍由原执行完成
        GatewayEvent::CalculatedCost(_) | GatewayEvent::ProviderCost(_) => return None,
        GatewayEvent::Completed(meta) => CanonicalEvent::Completed {
            id: meta.response_id().to_owned(),
            model: meta.model().map(str::to_owned),
            reason: match meta.finish_reason().unwrap_or(FinishReason::Other) {
                FinishReason::Stop => WireFinishReason::Stop,
                FinishReason::Length => WireFinishReason::Length,
                FinishReason::ToolCall => WireFinishReason::ToolCall,
                FinishReason::ContentFilter => WireFinishReason::ContentFilter,
                FinishReason::Other => WireFinishReason::Other,
                _ => WireFinishReason::Other,
            },
        },
        _ => {
            return Some(Err(PluginFault::new(
                ErrorCode::Unsupported,
                "unknown model event",
            )));
        }
    }))
}

fn project_wire(wire: &ProtocolWireEvent) -> Result<WireEvent, PluginFault> {
    let protocol = wire.protocol().to_owned();
    let payload = if let Some(body) = wire.raw_http_body_bytes() {
        WirePayload::RawBody {
            body: body.to_vec(),
        }
    } else if let Some(body) = wire.raw_json_body() {
        WirePayload::RawJson {
            body: body.to_vec(),
        }
    } else if wire.has_json_data() {
        WirePayload::Json {
            event: wire.event_type().map(str::to_owned),
            data: wire.data().clone(),
            id: wire.sse_id().map(str::to_owned),
            retry: wire.sse_retry(),
            raw_sse: wire.raw_sse_frame().map(|frame| frame.to_vec()),
        }
    } else if let Some(frame) = wire.raw_sse_frame() {
        WirePayload::RawSse {
            frame: frame.to_vec(),
        }
    } else {
        return Err(PluginFault::new(
            ErrorCode::Fault,
            "model wire event has no payload",
        ));
    };
    Ok(WireEvent { protocol, payload })
}
