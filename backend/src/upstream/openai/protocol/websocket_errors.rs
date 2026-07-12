//! Responses WebSocket 错误帧解析。

use serde_json::{Map, Value};

use super::json_value_as_string;

/// 从 WebSocket 错误帧推导出的上游错误分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedWebSocketError {
    /// 应映射给 HTTP 层的状态码。
    pub status_code: u16,
    /// 上游错误类型。
    pub error_type: Option<String>,
    /// 上游错误码。
    pub code: Option<String>,
    /// 上游错误消息。
    pub message: Option<String>,
    /// 上游错误参数。
    pub param: Option<String>,
    /// 错误帧携带的安全响应头。
    pub headers: Vec<(String, String)>,
}

/// 将 WebSocket 错误帧映射为结构化上游错误。
pub fn classify_websocket_error_frame(raw: &str) -> Option<ClassifiedWebSocketError> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    let event_type = value.get("type").and_then(Value::as_str)?;
    if event_type != "error" && event_type != "response.failed" {
        return None;
    }

    let error = websocket_error_value(&value);
    let error_type = error
        .and_then(|error| error.get("type"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let code = error
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let param = error
        .and_then(|error| error.get("param"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let explicit_status =
        explicit_error_status_code(&value).filter(|status_code| !(200..=299).contains(status_code));
    if explicit_error_status_code(&value).is_some() && explicit_status.is_none() {
        return None;
    }
    let normalized_code = code.as_deref().map(str::to_ascii_lowercase);
    let normalized_type = error_type.as_deref().map(str::to_ascii_lowercase);
    let status_code = explicit_status
        .or_else(|| normalized_code.as_deref().and_then(error_status_code))
        .or_else(|| normalized_type.as_deref().and_then(error_type_status_code))?;

    Some(ClassifiedWebSocketError {
        status_code,
        error_type,
        code,
        message,
        param,
        headers: websocket_error_headers(&value),
    })
}

fn websocket_error_value(value: &Value) -> Option<&Map<String, Value>> {
    value
        .pointer("/response/error")
        .or_else(|| value.get("error"))
        .and_then(Value::as_object)
}

fn explicit_error_status_code(value: &Value) -> Option<u16> {
    value
        .get("status")
        .or_else(|| value.get("status_code"))
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
}

fn error_status_code(code: &str) -> Option<u16> {
    match code {
        "usage_limit_reached" | "rate_limit_exceeded" | "rate_limit_reached" => Some(429),
        "quota_exhausted" | "payment_required" => Some(402),
        "unauthorized" | "token_invalid" | "token_expired" | "account_deactivated" => Some(401),
        "forbidden" | "account_banned" | "banned" => Some(403),
        "previous_response_not_found"
        | "invalid_encrypted_content"
        | "missing_tool_output"
        | "no_tool_output" => Some(400),
        "websocket_connection_limit_reached" => Some(503),
        _ => None,
    }
}

fn error_type_status_code(error_type: &str) -> Option<u16> {
    match error_type {
        "invalid_request_error" => Some(400),
        "authentication_error" => Some(401),
        "permission_error" => Some(403),
        "rate_limit_error" => Some(429),
        "server_error" | "api_error" => Some(500),
        _ => None,
    }
}

fn websocket_error_headers(value: &Value) -> Vec<(String, String)> {
    value
        .get("headers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| json_value_as_string(value).map(|value| (name.clone(), value)))
        .collect()
}

/// 从包裹在 WebSocket `error.headers` 中的 retry-after 值提取秒数。
pub fn retry_after_seconds_from_wrapped_error_headers(raw: &str) -> Option<u64> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    let header_retry_after = value
        .get("headers")
        .and_then(Value::as_object)
        .and_then(|headers| {
            headers.iter().find_map(|(name, value)| {
                if name.eq_ignore_ascii_case("retry-after") {
                    retry_after_seconds_from_json_header_value(value)
                } else {
                    None
                }
            })
        });
    header_retry_after.or_else(|| {
        value
            .pointer("/error/retry_after_seconds")
            .or_else(|| value.get("retry_after_seconds"))
            .and_then(Value::as_u64)
    })
}

fn retry_after_seconds_from_json_header_value(value: &Value) -> Option<u64> {
    let seconds = match value {
        Value::String(value) => value.trim().parse::<u64>().ok()?,
        Value::Number(value) => value.as_u64()?,
        Value::Array(values) => values
            .first()
            .and_then(retry_after_seconds_from_json_header_value)?,
        _ => return None,
    };
    (seconds > 0).then_some(seconds)
}
