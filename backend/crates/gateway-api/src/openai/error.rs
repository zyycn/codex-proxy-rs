//! OpenAI 客户端协议的稳定错误响应。

use axum::{
    Json,
    body::Body,
    http::{HeaderName, HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use gateway_core::{
    engine::EngineError,
    error::{GatewayError, GatewayErrorKind, ProviderErrorKind},
    policy::{ClientVersionRejection, CodexClientKind},
};
use serde_json::{Value, json};

use super::responses::ProtocolErrorBody;

/// OpenAI 风格错误响应。
pub fn openai_error_response(
    status: StatusCode,
    message: &str,
    error_type: &str,
    code: &str,
) -> (StatusCode, Json<Value>) {
    let body = json!({
        "error": {"message": message, "type": error_type, "code": code}
    });
    match capacity_error_for_client(&body) {
        Some(body) => (StatusCode::SERVICE_UNAVAILABLE, Json(body)),
        None => (status, Json(body)),
    }
}

/// Codex 将这两个容量码视为不可重试；仅在交付边界转换，内部仍保留真实上游事实。
pub(super) fn client_error_code(code: &str) -> &str {
    match code {
        "server_is_overloaded" | "slow_down" => "server_error",
        _ => code,
    }
}

/// 为 HTTP 错误或流内失败保留原有字段，仅投影客户端需要的重试信号。
pub(super) fn capacity_error_for_client(data: &Value) -> Option<Value> {
    const CODE_PATHS: [&str; 2] = ["/error/code", "/response/error/code"];
    let capacity_code = |value: &Value| {
        value
            .as_str()
            .is_some_and(|code| client_error_code(code) != code)
    };
    if !CODE_PATHS
        .iter()
        .any(|path| data.pointer(path).is_some_and(capacity_code))
    {
        return None;
    }
    let mut projected = data.clone();
    for path in CODE_PATHS {
        if let Some(code) = projected.pointer_mut(path)
            && capacity_code(code)
        {
            *code = Value::String("server_error".to_owned());
        }
    }
    // WS 包装错误按 HTTP 状态分类；保留 400/429 会使客户端终止重试。
    for field in ["status", "status_code"] {
        if let Some(status) = projected.get_mut(field)
            && status.is_number()
        {
            *status = json!(503);
        }
    }
    Some(projected)
}

/// 严格协议 decoder/encoder 返回的安全错误 body。
pub fn protocol_error_response(status: StatusCode, body: ProtocolErrorBody) -> Response {
    (status, Json(body.into_value())).into_response()
}

/// 下游 Client API Key 无效。
pub fn missing_client_api_key_response() -> (StatusCode, Json<Value>) {
    openai_error_response(
        StatusCode::UNAUTHORIZED,
        "Missing or invalid API key",
        "invalid_request_error",
        "invalid_api_key",
    )
}

/// RuntimeSnapshot 当前不允许接收新的配置依赖请求。
pub fn runtime_unavailable_response() -> (StatusCode, Json<Value>) {
    openai_error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "Gateway runtime configuration is temporarily unavailable",
        "server_error",
        "runtime_configuration_unavailable",
    )
}

/// 已识别 Codex 客户端不满足最低版本要求。
pub fn client_version_rejection_response(
    rejection: &ClientVersionRejection,
) -> (StatusCode, Json<Value>) {
    let client_label = match rejection.kind() {
        CodexClientKind::Desktop => "Codex Desktop",
        CodexClientKind::Cli => "Codex CLI",
    };
    let (message, code, current_version) = match rejection.current() {
        Some(current) => (
            format!(
                "{client_label} {current} is below the minimum required version {}. Upgrade {client_label} and retry.",
                rejection.min()
            ),
            "client_version_too_old",
            Some(current.to_string()),
        ),
        None => (
            format!(
                "A valid {client_label} version is required. Upgrade to version {} or newer and retry.",
                rejection.min()
            ),
            "client_version_unavailable",
            None,
        ),
    };
    (
        StatusCode::UPGRADE_REQUIRED,
        Json(json!({
            "error": {
                "message": message,
                "type": "invalid_request_error",
                "code": code,
                "client": rejection.kind().as_str(),
                "current_version": current_version,
                "min_version": rejection.min().to_string()
            }
        })),
    )
}

/// 对外模型不存在。
pub fn model_not_found_response() -> (StatusCode, Json<Value>) {
    openai_error_response(
        StatusCode::NOT_FOUND,
        "The requested model does not exist or is not available",
        "invalid_request_error",
        "model_not_found",
    )
}

/// 将 core Engine 错误收敛为稳定 Gateway 分类。
#[must_use]
pub fn gateway_error_from_engine(error: &EngineError) -> GatewayError {
    match error {
        EngineError::Provider(error) => GatewayError::from_provider(error),
        EngineError::Cancelled => {
            GatewayError::new(GatewayErrorKind::Cancelled, "request was cancelled")
        }
        EngineError::Deadline => {
            GatewayError::new(GatewayErrorKind::Timeout, "gateway request timed out")
        }
        EngineError::ProviderNotRegistered { .. } | EngineError::EmptyRoutingPlan => {
            GatewayError::new(
                GatewayErrorKind::NoAvailableProvider,
                "no upstream provider is currently available for this request",
            )
        }
        EngineError::Store(_)
        | EngineError::ProviderMetadataMismatch
        | EngineError::ContinuationPinMismatch
        | EngineError::RequiredAccountMismatch
        | EngineError::AccountOutsideClientScope
        | EngineError::DownstreamCommitRequired
        | EngineError::NoActiveAttempt
        | EngineError::InvalidDeliveryState => {
            GatewayError::new(GatewayErrorKind::Internal, "gateway execution failed")
        }
    }
}

/// Gateway 错误的 OpenAI HTTP 表达。
pub fn gateway_error_response(error: &GatewayError) -> Response {
    let (status, default_type, default_code) = gateway_error_contract(error.kind());
    let mut response = openai_error_response(
        status,
        error.client_message(),
        error.client_error_type().unwrap_or(default_type),
        error.client_error_code().unwrap_or(default_code),
    )
    .into_response();
    if let Some(delay) = error.retry_after()
        && let Ok(value) = HeaderValue::from_str(
            &delay
                .as_secs()
                .saturating_add(u64::from(delay.subsec_nanos() > 0))
                .max(1)
                .to_string(),
        )
    {
        response
            .headers_mut()
            .insert(axum::http::header::RETRY_AFTER, value);
    }
    response
}

/// 在下游尚未提交时交付 Provider 的 HTTP 失败响应，并应用客户端容量恢复合同。
pub fn engine_error_response(error: &EngineError) -> Response {
    let capacity_unavailable = matches!(error, EngineError::Provider(error)
        if error.kind() == ProviderErrorKind::UpstreamCapacityUnavailable);
    if let EngineError::Provider(error) = error
        && let Some(upstream) = error.client_visible_upstream_response()
        && let Ok(status) = StatusCode::from_u16(upstream.status())
    {
        let projected = serde_json::from_slice::<Value>(upstream.body())
            .ok()
            .and_then(|body| capacity_error_for_client(&body));
        let mut response = match projected {
            Some(body) => {
                let mut response = Response::new(Body::from(body.to_string()));
                *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                response
            }
            None => {
                let mut response = Response::new(Body::from(upstream.body().clone()));
                *response.status_mut() = if capacity_unavailable {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    status
                };
                response
            }
        };
        if let Some(content_type) = upstream.content_type()
            && let Ok(content_type) = HeaderValue::from_bytes(content_type)
        {
            response.headers_mut().insert(CONTENT_TYPE, content_type);
        }
        for header in upstream.headers() {
            let Ok(name) = HeaderName::from_bytes(header.name().as_bytes()) else {
                continue;
            };
            let Ok(value) = HeaderValue::from_bytes(header.value()) else {
                continue;
            };
            response.headers_mut().append(name, value);
        }
        return response;
    }
    let mut response = gateway_error_response(&gateway_error_from_engine(error));
    if capacity_unavailable {
        *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    }
    // 正文未完整取得时不能透传残缺响应，但已确认的上游关联 ID 仍应交付。
    if let EngineError::Provider(error) = error
        && let Some(request_id) = error.upstream_request_id()
        && let Ok(value) = HeaderValue::from_str(request_id.as_str())
    {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

/// Gateway 错误稳定映射，供 HTTP、SSE 和 WebSocket 共用。
#[must_use]
pub const fn gateway_error_contract(
    kind: GatewayErrorKind,
) -> (StatusCode, &'static str, &'static str) {
    match kind {
        GatewayErrorKind::InvalidRequest => (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "invalid_request",
        ),
        GatewayErrorKind::Unsupported => (
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "unsupported_capability",
        ),
        GatewayErrorKind::Unauthorized => (
            StatusCode::UNAUTHORIZED,
            "invalid_request_error",
            "invalid_api_key",
        ),
        GatewayErrorKind::PolicyDenied => (
            StatusCode::FORBIDDEN,
            "invalid_request_error",
            "policy_denied",
        ),
        GatewayErrorKind::ModelNotFound => (
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            "model_not_found",
        ),
        GatewayErrorKind::NoAvailableProvider => (
            StatusCode::SERVICE_UNAVAILABLE,
            "server_error",
            "no_available_provider",
        ),
        GatewayErrorKind::AccountCapacityUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "server_error",
            "account_capacity_unavailable",
        ),
        GatewayErrorKind::ProviderInfrastructureUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "server_error",
            "provider_infrastructure_unavailable",
        ),
        GatewayErrorKind::ConcurrencyQueueFull => (
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "concurrency_queue_full",
        ),
        GatewayErrorKind::ConcurrencyQueueTimeout => (
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "concurrency_queue_timeout",
        ),
        GatewayErrorKind::RateLimited => (
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "rate_limit_exceeded",
        ),
        GatewayErrorKind::UpstreamUnavailable => (
            StatusCode::BAD_GATEWAY,
            "server_error",
            "upstream_unavailable",
        ),
        GatewayErrorKind::Timeout => (
            StatusCode::GATEWAY_TIMEOUT,
            "server_error",
            "request_timeout",
        ),
        GatewayErrorKind::Cancelled => (
            StatusCode::REQUEST_TIMEOUT,
            "server_error",
            "request_cancelled",
        ),
        GatewayErrorKind::Internal => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "internal_error",
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "internal_error",
        ),
    }
}
