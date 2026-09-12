//! Responses WebSocket 入站请求与下行事件的纯协议映射。

use axum::http::{HeaderName, HeaderValue, StatusCode};
use gateway_core::engine::EngineError;
use gateway_core::event::ProviderResponseHeader;
use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::openai::error::{gateway_error_contract, gateway_error_from_engine};

use super::super::{
    DecodedResponsesRequest, ProtocolErrorBody, RequestDecodeError,
    request::{OpenAiRequestHeaders, RequestDecodeSource},
};

/// 使用连接级请求头和 Provider 上下文解码官方 `response.create` 文本帧。
///
/// 缺省 `stream` 等价于 WebSocket 固有的流式语义；显式 `false` 会被拒绝。
///
/// # Errors
///
/// 帧不是合法 JSON object、消息类型错误、显式关闭 stream，或 Responses 请求
/// 无法映射到 canonical operation 时返回不包含正文内容的稳定错误。
pub fn decode_response_create_with_context(
    payload: &str,
    request_headers: &OpenAiRequestHeaders,
) -> Result<DecodedResponsesRequest, ResponseCreateFrameError> {
    decode_response_create_inner(payload, request_headers)
}

fn decode_response_create_inner(
    payload: &str,
    request_headers: &OpenAiRequestHeaders,
) -> Result<DecodedResponsesRequest, ResponseCreateFrameError> {
    let Value::Object(mut body) = serde_json::from_str::<Value>(payload)
        .map_err(|_| ResponseCreateFrameError::InvalidJson)?
    else {
        return Err(ResponseCreateFrameError::ExpectedObject);
    };
    match body.remove("type") {
        Some(Value::String(message_type)) if message_type == "response.create" => {}
        _ => return Err(ResponseCreateFrameError::UnsupportedType),
    }
    if matches!(body.get("stream"), Some(value) if value.as_bool() != Some(true)) {
        return Err(ResponseCreateFrameError::StreamingRequired);
    }
    super::super::request::decode_request_object(
        body,
        request_headers,
        RequestDecodeSource::WebSocketFrame,
    )
    .map_err(ResponseCreateFrameError::Request)
}

/// `response.create` 帧的稳定安全错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResponseCreateFrameError {
    /// 文本不是合法 JSON。
    #[error("response.create frame must be valid JSON")]
    InvalidJson,
    /// 顶层不是 object。
    #[error("response.create frame must be a JSON object")]
    ExpectedObject,
    /// `type` 缺失或不是 `response.create`。
    #[error("unsupported Responses WebSocket message type")]
    UnsupportedType,
    /// WebSocket 请求显式声明 `stream=false`。
    #[error("Responses WebSocket requests require stream=true")]
    StreamingRequired,
    /// 内层 Responses 请求无法映射到 canonical operation。
    #[error(transparent)]
    Request(RequestDecodeError),
}

impl ResponseCreateFrameError {
    pub(super) fn protocol_body(&self) -> ProtocolErrorBody {
        match self {
            Self::Request(error) => error.protocol_body(),
            Self::InvalidJson => RequestDecodeError::MalformedJson.protocol_body(),
            Self::ExpectedObject => RequestDecodeError::ExpectedObject.protocol_body(),
            Self::UnsupportedType => RequestDecodeError::InvalidValue {
                field: "type".to_owned(),
            }
            .protocol_body(),
            Self::StreamingRequired => RequestDecodeError::InvalidValue {
                field: "stream".to_owned(),
            }
            .protocol_body(),
        }
    }
}

pub(super) fn response_metadata_event(
    request_id: &str,
    response_headers: &[ProviderResponseHeader],
) -> String {
    let mut headers = response_event_headers(response_headers);
    headers
        .entry("x-request-id".to_owned())
        .or_insert_with(|| Value::String(request_id.to_owned()));
    json!({
        "type": "response.metadata",
        "headers": headers,
    })
    .to_string()
}

fn response_event_headers(response_headers: &[ProviderResponseHeader]) -> Map<String, Value> {
    let connection_options = super::super::response_connection_options(response_headers);
    let mut headers = Map::new();
    for header in response_headers {
        if !super::super::response_header_is_forwardable(header.name(), &connection_options) {
            continue;
        }
        let Ok(name) = HeaderName::from_bytes(header.name().as_bytes()) else {
            continue;
        };
        let Ok(http_value) = HeaderValue::from_bytes(header.value()) else {
            continue;
        };
        let Ok(value) = std::str::from_utf8(header.value()) else {
            continue;
        };
        if matches!(name.as_str(), "x-request-id" | "x-oai-request-id")
            && (value.trim().is_empty() || http_value.to_str().is_err())
        {
            continue;
        }
        // 官方 WS 事件使用 string map，无法表达同名多值；后值保持既有覆盖语义。
        headers.insert(name.as_str().to_owned(), Value::String(value.to_owned()));
    }
    headers
}

pub(super) fn initial_engine_error_event(
    error: &EngineError,
    request_id: &str,
    response_headers: &[ProviderResponseHeader],
) -> String {
    let gateway = gateway_error_from_engine(error);
    let (default_status, default_type, default_code) = gateway_error_contract(gateway.kind());
    let provider = match error {
        EngineError::Provider(provider) => Some(provider),
        _ => None,
    };
    let upstream = provider.and_then(|error| error.client_visible_upstream_response());
    // 最终失败响应优先于会话快照，避免混入先前 attempt 或 opening 的关联头。
    let mut headers =
        response_event_headers(upstream.map_or(response_headers, |response| response.headers()));
    if upstream.is_none() {
        // 会话 opening 身份不证明当前失败属于该请求；仅保留其他允许的会话头。
        headers.remove("x-request-id");
        headers.remove("x-oai-request-id");
    }
    let status = upstream
        .map(|response| response.status())
        .or_else(|| provider.and_then(|error| error.upstream_status()))
        .and_then(|value| StatusCode::from_u16(value).ok())
        .filter(|value| !value.is_success() && !value.is_informational())
        .unwrap_or(default_status);
    // 无原响应关联头时使用 Provider 的失败事实；不能让旧会话 ID 遮住当前失败。
    if !has_request_id(&headers)
        && let Some(request_id) = provider.and_then(|error| error.upstream_request_id())
        && let Ok(value) = HeaderValue::from_str(request_id.as_str())
        && let Ok(value) = value.to_str()
        && !value.trim().is_empty()
    {
        headers.insert("x-request-id".to_owned(), Value::String(value.to_owned()));
    }
    // 只桥接 Provider 已提取的结构化错误；原始 HTML/文本正文不成为安全 fallback 的 message。
    error_event(
        status,
        gateway.client_error_type().unwrap_or(default_type),
        gateway.client_error_code().unwrap_or(default_code),
        gateway.client_message(),
        None,
        Some(request_id),
        headers,
    )
}

fn has_request_id(headers: &Map<String, Value>) -> bool {
    headers.contains_key("x-request-id") || headers.contains_key("x-oai-request-id")
}

pub(super) fn error_event(
    status: StatusCode,
    error_type: &str,
    code: &str,
    message: &str,
    param: Option<&str>,
    request_id: Option<&str>,
    mut headers: Map<String, Value>,
) -> String {
    let mut error = Map::new();
    error.insert("type".to_owned(), Value::String(error_type.to_owned()));
    error.insert("code".to_owned(), Value::String(code.to_owned()));
    error.insert("message".to_owned(), Value::String(message.to_owned()));
    if let Some(param) = param {
        error.insert("param".to_owned(), Value::String(param.to_owned()));
    }
    let mut event = Map::new();
    event.insert("type".to_owned(), Value::String("error".to_owned()));
    event.insert("status".to_owned(), Value::Number(status.as_u16().into()));
    event.insert("error".to_owned(), Value::Object(error));
    if let Some(request_id) = request_id {
        // Codex 从本 error 事件的 headers 读 ID，独立 metadata 和顶层 request_id 不能替代。
        // 网关身份独立保存；有上游关联头时不冒充或覆盖上游身份。
        if !has_request_id(&headers) {
            headers.insert(
                "x-request-id".to_owned(),
                Value::String(request_id.to_owned()),
            );
        }
        headers.insert(
            "x-gateway-request-id".to_owned(),
            Value::String(request_id.to_owned()),
        );
        event.insert(
            "request_id".to_owned(),
            Value::String(request_id.to_owned()),
        );
    }
    if !headers.is_empty() {
        event.insert("headers".to_owned(), Value::Object(headers));
    }
    Value::Object(event).to_string()
}

pub(super) fn connection_limit_event() -> String {
    error_event(
        StatusCode::BAD_REQUEST,
        "invalid_request_error",
        "websocket_connection_limit_reached",
        "Responses websocket connection limit reached (60 minutes). Create a new websocket connection to continue.",
        None,
        None,
        Map::new(),
    )
}
