//! 请求观测 JSON 的类型化编码，只拥有持久化分组，不决定业务状态

use serde::Serialize;
use serde_json::value::RawValue;

use super::{ModelRequestFinalization, NewModelRequest, StoreResult, invalid};

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RequestObservation {
    #[serde(skip_serializing_if = "Option::is_none")]
    request: Option<RequestContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    routing: Option<RoutingSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timings: Option<RequestTimings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scheduling: Option<SchedulingObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<TransportObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<ResponseObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    continuation: Option<ContinuationObservation>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestContext {
    config_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_api_key_name_snapshot: Option<String>,
    protocol: String,
    endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_preset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_kind: Option<String>,
    compact: bool,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct RoutingSnapshot {
    scope: String,
    group_refs: Vec<String>,
    group_names_snapshot: Vec<String>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorObservation {
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_ms: Option<u64>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestTimings {
    #[serde(skip_serializing_if = "Option::is_none")]
    local: Option<LocalTimings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<UpstreamTimings>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalTimings {
    #[serde(skip_serializing_if = "Option::is_none")]
    transport_decision_wait_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connect_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    headers_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_event_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_reasoning_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_text_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_token_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latency_ms: Option<u64>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpstreamTimings {
    #[serde(skip_serializing_if = "Option::is_none")]
    processing_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_overhead_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_iapi_ttft_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_service_ttft_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_iapi_tbt_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_service_tbt_ms: Option<f64>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct SchedulingObservation {
    #[serde(skip_serializing_if = "Option::is_none")]
    admission_decision_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    account_selection_wait_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capacity_used_slots: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capacity_total_slots: Option<u64>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransportObservation {
    #[serde(skip_serializing_if = "Option::is_none")]
    http_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    websocket_pool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connection: Option<ConnectionObservation>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionObservation {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    age_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    idle_ms: Option<u64>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseObservation {
    #[serde(skip_serializing_if = "Option::is_none")]
    response_model: Option<String>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ContinuationObservation {
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable_reason: Option<String>,
}

impl RequestObservation {
    pub(super) fn initial(
        request: &NewModelRequest,
    ) -> StoreResult<sqlx::types::Json<Box<RawValue>>> {
        validate_integers([Some(request.config_revision), request.admission_decision_ms])?;
        let observation = Self {
            request: Some(RequestContext {
                config_revision: request.config_revision,
                client_api_key_name_snapshot: request.client_api_key_name_snapshot.clone(),
                protocol: request.protocol.clone(),
                endpoint: request.endpoint.clone(),
                client_ip: request.client_ip.clone(),
                user_agent: request.user_agent.clone(),
                reasoning_effort: request.reasoning_effort.clone(),
                reasoning_preset: request.reasoning_preset.clone(),
                subagent_kind: request.subagent_kind.clone(),
                compact: request.compact,
            }),
            routing: Some(RoutingSnapshot {
                scope: request.routing_scope.clone(),
                group_refs: request.routing_group_refs.clone(),
                group_names_snapshot: serde_json::from_value(
                    request.routing_group_names_snapshot.clone(),
                )
                .map_err(|source| invalid("invalid routing group names").with_source(source))?,
            }),
            scheduling: Some(SchedulingObservation {
                admission_decision_ms: request.admission_decision_ms,
                ..Default::default()
            }),
            continuation: Some(ContinuationObservation {
                previous_response_id_hash: request.continuation.previous_response_id_hash.clone(),
                ..Default::default()
            }),
            ..Default::default()
        };
        observation.encode()
    }

    pub(super) fn finalized(
        value: &ModelRequestFinalization,
    ) -> StoreResult<sqlx::types::Json<Box<RawValue>>> {
        let timings = &value.timings;
        validate_integers([
            timings.transport_decision_wait_ms,
            timings.connect_ms,
            timings.headers_ms,
            timings.first_event_ms,
            timings.first_reasoning_ms,
            timings.first_text_ms,
            timings.first_token_ms,
            timings.latency_ms,
            timings.provider_processing_ms,
            timings.upstream_response_ms,
            value.upstream_connection_age_ms,
            value.upstream_connection_idle_ms,
            value.retry_after_ms,
        ])?;
        // serde_json 会把非有限浮点数写成 null，先拒绝以免把非法指标伪装成缺失
        for value in [
            timings.upstream_api_overhead_ms,
            timings.upstream_engine_ms,
            timings.upstream_engine_iapi_ttft_ms,
            timings.upstream_engine_service_ttft_ms,
            timings.upstream_engine_iapi_tbt_ms,
            timings.upstream_engine_service_tbt_ms,
        ]
        .into_iter()
        .flatten()
        {
            if !value.is_finite() || value < 0.0 || value >= i64::MAX as f64 {
                return Err(invalid(
                    "upstream timing is outside the supported millisecond range",
                ));
            }
        }
        let observation = Self {
            timings: Some(RequestTimings {
                local: Some(LocalTimings {
                    transport_decision_wait_ms: timings.transport_decision_wait_ms,
                    connect_ms: timings.connect_ms,
                    headers_ms: timings.headers_ms,
                    first_event_ms: timings.first_event_ms,
                    first_reasoning_ms: timings.first_reasoning_ms,
                    first_text_ms: timings.first_text_ms,
                    first_token_ms: timings.first_token_ms,
                    latency_ms: timings.latency_ms,
                }),
                upstream: Some(UpstreamTimings {
                    processing_ms: timings.provider_processing_ms,
                    response_ms: timings.upstream_response_ms,
                    api_overhead_ms: timings.upstream_api_overhead_ms,
                    engine_ms: timings.upstream_engine_ms,
                    engine_iapi_ttft_ms: timings.upstream_engine_iapi_ttft_ms,
                    engine_service_ttft_ms: timings.upstream_engine_service_ttft_ms,
                    engine_iapi_tbt_ms: timings.upstream_engine_iapi_tbt_ms,
                    engine_service_tbt_ms: timings.upstream_engine_service_tbt_ms,
                }),
            }),
            transport: Some(TransportObservation {
                http_version: value.http_version.clone(),
                websocket_pool: value.websocket_pool.clone(),
                connection: value
                    .upstream_connection_id
                    .as_ref()
                    .map(|id| ConnectionObservation {
                        id: Some(id.clone()),
                        exit_reason: value.upstream_connection_exit_reason.clone(),
                        age_ms: value.upstream_connection_age_ms,
                        idle_ms: value.upstream_connection_idle_ms,
                    }),
            }),
            response: Some(ResponseObservation {
                response_model: value.upstream_response_model.clone(),
            }),
            error: Some(ErrorObservation {
                provider_error_code: value.provider_error_code.clone(),
                message: value.error_message.clone(),
                retry_after_ms: value.retry_after_ms,
            }),
            continuation: Some(ContinuationObservation {
                unavailable_reason: value.continuation_unavailable_reason.clone(),
                ..Default::default()
            }),
            ..Default::default()
        };
        observation.encode()
    }

    fn encode(self) -> StoreResult<sqlx::types::Json<Box<RawValue>>> {
        // 类型化事实只编码一次，SQLx 复用原始 JSON 文本，避免 Value 树与第二次长度序列化
        let text = serde_json::to_string(&self)
            .map_err(|source| invalid("encode request observation").with_source(source))?;
        if text.len() > 1024 * 1024 {
            return Err(invalid("request observation exceed 1 MiB"));
        }
        RawValue::from_string(text)
            .map(sqlx::types::Json)
            .map_err(|source| invalid("encode request observation").with_source(source))
    }
}

// 整数指标保持 bigint 边界，官方专项计时允许毫秒小数
fn validate_integers(values: impl IntoIterator<Item = Option<u64>>) -> StoreResult<()> {
    if values
        .into_iter()
        .flatten()
        .any(|value| value > i64::MAX as u64)
    {
        return Err(invalid(
            "request observation number exceeds supported range",
        ));
    }
    Ok(())
}
