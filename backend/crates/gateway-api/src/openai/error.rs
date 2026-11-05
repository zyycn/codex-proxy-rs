//! OpenAI 客户端协议的稳定错误响应

use axum::{
    Json,
    body::Body,
    http::{HeaderName, HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use gateway_core::{
    engine::EngineError,
    error::ProviderErrorKind,
    policy::{ClientVersionRejection, CodexClientKind},
};
use serde_json::{Value, json};

pub use gateway_core::engine::execution::gateway_error_from_engine;

use crate::client_error::{ProtocolErrorBody, capacity_error_for_client};

pub use crate::client_error::{
    gateway_error_contract, gateway_error_response, openai_error_response,
};

/// 严格协议 decoder/encoder 返回的安全错误 body
pub fn protocol_error_response(status: StatusCode, body: ProtocolErrorBody) -> Response {
    let mut response = (status, Json(body.clone().into_value())).into_response();
    response.extensions_mut().insert(body);
    response
}

/// 下游 Client API Key 无效
pub fn missing_client_api_key_response() -> (StatusCode, Json<Value>) {
    openai_error_response(
        StatusCode::UNAUTHORIZED,
        "Missing or invalid API key.",
        "invalid_request_error",
        "invalid_api_key",
    )
}

/// RuntimeSnapshot 当前不允许接收新的配置依赖请求
pub fn runtime_unavailable_response() -> (StatusCode, Json<Value>) {
    openai_error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "Gateway configuration is temporarily unavailable.",
        "server_error",
        "runtime_configuration_unavailable",
    )
}

/// 已识别 Codex 客户端不满足最低版本要求
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
                "{client_label} {current} is outdated. Upgrade to {} or later.",
                rejection.min()
            ),
            "client_version_too_old",
            Some(current.to_string()),
        ),
        None => (
            format!(
                "Cannot read the {client_label} version. Check the User-Agent and version headers."
            ),
            "client_version_unavailable",
            None,
        ),
    };
    // Codex 将 400 视为不可重试的请求错误，426 则会触发 WS 回退和 HTTP 重试
    (
        StatusCode::BAD_REQUEST,
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

/// 对外模型不存在
pub fn model_not_found_response() -> (StatusCode, Json<Value>) {
    openai_error_response(
        StatusCode::NOT_FOUND,
        "The requested model does not exist or is not available.",
        "invalid_request_error",
        "model_not_found",
    )
}

/// 在下游尚未提交时交付 Provider 的 HTTP 失败响应，并应用客户端容量恢复合同
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
    // 正文未完整取得时不能透传残缺响应，但已确认的上游关联 ID 仍应交付
    if let EngineError::Provider(error) = error
        && let Some(request_id) = error.upstream_request_id()
        && let Ok(value) = HeaderValue::from_str(request_id.as_str())
    {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}
