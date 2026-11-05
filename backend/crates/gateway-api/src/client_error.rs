//! 数据面与公共 HTTP 中间件共用的客户端错误合同和重试投影

use axum::{
    Json,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use gateway_core::error::{GatewayError, GatewayErrorKind};
use serde::Serialize;
use serde_json::{Value, json};

/// 认证边界提供的稳定拒绝原因，不携带认证输入
#[derive(Clone)]
pub(crate) struct AccessRejection(pub &'static str);

/// 执行已交给 Core 观测，HTTP 入口只补充交付边界日志
#[derive(Clone)]
pub(crate) struct ExecutionObserved;

/// OpenAI 风格的协议错误 JSON
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtocolErrorBody {
    /// 错误对象
    pub error: ProtocolError,
}

/// 不包含请求正文或 prompt 的错误对象
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtocolError {
    /// OpenAI 错误类别
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// 稳定机器码
    pub code: &'static str,
    /// 安全的可读信息
    pub message: String,
    /// 出错字段；不包含字段值
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
}

impl ProtocolErrorBody {
    /// 序列化为 JSON value
    #[must_use]
    pub fn into_value(self) -> Value {
        serde_json::json!({ "error": self.error })
    }
}

/// OpenAI 风格错误响应
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

/// 无重试提示的容量拒绝及旧版 Codex 限流错误需要可重试投影
/// 仅在交付边界转换，内部仍保留真实上游事实
pub(crate) fn client_error_code(code: &str) -> &str {
    match code {
        "server_is_overloaded" | "slow_down" => "server_error",
        _ => code,
    }
}

/// 为 HTTP 错误或流内失败保留原有字段，仅投影客户端需要的重试信号
pub(crate) fn capacity_error_for_client(data: &Value) -> Option<Value> {
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
    // WS 包装错误按 HTTP 状态分类；保留 400/429 会使客户端终止重试
    for field in ["status", "status_code"] {
        if let Some(status) = projected.get_mut(field)
            && status.is_number()
        {
            *status = json!(503);
        }
    }
    Some(projected)
}

/// Gateway 错误的 OpenAI HTTP 表达
pub fn gateway_error_response(error: &GatewayError) -> Response {
    let (status, default_type, default_code) = gateway_error_contract(error.kind());
    let mut response = openai_error_response(
        status,
        error.client_message(),
        error.client_error_type().unwrap_or(default_type),
        error.client_error_code().unwrap_or(default_code),
    )
    .into_response();
    response.extensions_mut().insert(error.clone());
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

/// Gateway 错误稳定映射，供 HTTP、SSE 和 WebSocket 共用
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
        GatewayErrorKind::MiddlewareCapacityUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "server_error",
            "middleware_capacity_unavailable",
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
        GatewayErrorKind::BudgetExhausted => (
            StatusCode::TOO_MANY_REQUESTS,
            "usage_limit_reached",
            "usage_limit_reached",
        ),
        GatewayErrorKind::UpstreamUnavailable => (
            StatusCode::BAD_GATEWAY,
            "server_error",
            "upstream_unavailable",
        ),
        GatewayErrorKind::MessageTooBig => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request_error",
            "message_too_big",
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
