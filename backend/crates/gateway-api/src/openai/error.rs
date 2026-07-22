//! OpenAI 客户端协议的稳定错误响应。

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use gateway_core::{
    engine::{EngineError, UpstreamSendState},
    error::{GatewayError, GatewayErrorKind, ProviderErrorKind},
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
    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": error_type,
                "code": code
            }
        })),
    )
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
        EngineError::Provider(error)
            if error.kind() == ProviderErrorKind::Unavailable
                && error.send_state() == UpstreamSendState::NotSent =>
        {
            GatewayError::new(
                GatewayErrorKind::NoAvailableProvider,
                "no upstream provider is currently available for this request",
            )
        }
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
        | EngineError::DownstreamCommitRequired
        | EngineError::InvalidDeliveryState => {
            GatewayError::new(GatewayErrorKind::Internal, "gateway execution failed")
        }
    }
}

/// Gateway 错误的 OpenAI HTTP 表达。
pub fn gateway_error_response(error: &GatewayError) -> Response {
    let (status, error_type, code) = gateway_error_contract(error.kind());
    openai_error_response(status, error.safe_message(), error_type, code).into_response()
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
