//! 合并适配器的原始上游观测，并记录宿主实际收到规范化输出的时间

use gateway_core::event::{
    GatewayEvent, ProviderResponseHeader, ProviderResponseMetadata, ProviderResponseObservation,
    ProviderResponseTimings, UpstreamHttpVersion, WebSocketPoolKind,
};
use gateway_core::upstream::{OpaqueUpstreamValue, UpstreamSendState};
use gateway_plugin_sdk::call::model::facts::ResponseObservation;

use super::failure::invalid;

pub(super) fn merge(
    mut current: ProviderResponseObservation,
    update: ResponseObservation,
    sent: UpstreamSendState,
) -> Result<ProviderResponseObservation, gateway_core::error::ProviderError> {
    // 传输类型由已选适配器固定，不能让不一致的报告绕过 Core 的观测归属
    if update.transport != current.transport().as_str() {
        return Err(invalid(sent));
    }
    if let Some(value) = update.http_version {
        let version = if value == "unknown" {
            UpstreamHttpVersion::Unknown
        } else {
            UpstreamHttpVersion::parse(&value).ok_or_else(|| invalid(sent))?
        };
        current = current.with_http_version(version);
    }
    if let Some(value) = update.websocket_pool {
        current = current.with_websocket_pool(match value.as_str() {
            "new" => WebSocketPoolKind::New,
            "reuse" => WebSocketPoolKind::Reuse,
            _ => return Err(invalid(sent)),
        });
    }
    if let Some(value) = update.status_code {
        if !(100..=599).contains(&value) {
            return Err(invalid(sent));
        }
        current = current.with_status_code(value);
    }
    if let Some(value) = update.request_id {
        current = current.with_request_id(OpaqueUpstreamValue::new(value));
    }
    if let Some(value) = update.service_tier {
        current = current
            .try_with_service_tier(value)
            .map_err(|source| invalid(sent).with_source(source))?;
    }
    if let Some(value) = update.upstream_response_model {
        current = current.with_upstream_response_model_if_valid(&value);
        if current.upstream_response_model() != Some(value.trim()) {
            return Err(invalid(sent));
        }
    }
    if !update.client_headers.is_empty() {
        current = current.with_client_headers(
            update
                .client_headers
                .into_iter()
                .map(|header| ProviderResponseHeader::new(header.name, header.value.into()))
                .collect(),
        );
    }
    if let Some(value) = update.provider_metadata {
        current = current.with_provider_metadata(
            ProviderResponseMetadata::new(value).ok_or_else(|| invalid(sent))?,
        );
    }
    let previous = current.timings();
    let update = update.timings;
    let timings = ProviderResponseTimings {
        transport_decision_wait_ms: update
            .transport_decision_wait_ms
            .or(previous.transport_decision_wait_ms),
        connect_ms: update.connect_ms.or(previous.connect_ms),
        headers_ms: update.headers_ms.or(previous.headers_ms),
        first_event_ms: update.first_event_ms.or(previous.first_event_ms),
        first_reasoning_ms: update.first_reasoning_ms.or(previous.first_reasoning_ms),
        first_text_ms: update.first_text_ms.or(previous.first_text_ms),
        first_token_ms: update.first_token_ms.or(previous.first_token_ms),
        provider_processing_ms: update
            .provider_processing_ms
            .or(previous.provider_processing_ms),
        upstream_response_ms: update
            .upstream_response_ms
            .or(previous.upstream_response_ms),
        upstream_api_overhead_ms: update
            .upstream_api_overhead_ms
            .or(previous.upstream_api_overhead_ms),
        upstream_engine_ms: update.upstream_engine_ms.or(previous.upstream_engine_ms),
    };
    Ok(current.with_timings(timings))
}

pub(super) fn observe_output(
    current: ProviderResponseObservation,
    facts: &[GatewayEvent],
    has_wire: bool,
    started_at: std::time::Instant,
) -> ProviderResponseObservation {
    let elapsed = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut timings = current.timings();
    if has_wire || !facts.is_empty() {
        timings.first_event_ms.get_or_insert(elapsed);
    }
    // 内容边界来自插件报告的 canonical 事实，不按上游私有事件名猜测
    for fact in facts {
        let has_output = match fact {
            GatewayEvent::TextDelta(delta) if !delta.text.is_empty() => {
                timings.first_text_ms.get_or_insert(elapsed);
                true
            }
            GatewayEvent::ReasoningDelta(delta) if !delta.text.is_empty() => {
                timings.first_reasoning_ms.get_or_insert(elapsed);
                true
            }
            GatewayEvent::ToolCallDelta(delta) => {
                !delta.arguments_delta.is_empty()
                    || delta.name.as_ref().is_some_and(|name| !name.is_empty())
            }
            _ => false,
        };
        if has_output {
            timings.first_token_ms.get_or_insert(elapsed);
        }
    }
    current.with_timings(timings)
}
