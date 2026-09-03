//! Responses WebSocket 入站请求与下行事件的纯协议映射。

use axum::http::{HeaderName, StatusCode};
use gateway_core::event::ProviderResponseHeader;
use serde_json::{Map, Value, json};
use thiserror::Error;

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
    let connection_options = super::super::response_connection_options(response_headers);
    let mut headers = Map::new();
    for header in response_headers {
        if !super::super::response_header_is_forwardable(header.name(), &connection_options) {
            continue;
        }
        let Ok(name) = HeaderName::from_bytes(header.name().as_bytes()) else {
            continue;
        };
        let Ok(value) = std::str::from_utf8(header.value()) else {
            continue;
        };
        // 官方 response.metadata 使用 string map，无法表达同名多值；后值保持既有覆盖语义。
        headers.insert(name.as_str().to_owned(), Value::String(value.to_owned()));
    }
    headers
        .entry("x-request-id".to_owned())
        .or_insert_with(|| Value::String(request_id.to_owned()));
    json!({
        "type": "response.metadata",
        "headers": headers,
    })
    .to_string()
}

pub(super) fn error_event(
    status: StatusCode,
    error_type: &str,
    code: &str,
    message: &str,
    param: Option<&str>,
    request_id: Option<&str>,
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
        event.insert(
            "request_id".to_owned(),
            Value::String(request_id.to_owned()),
        );
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
    )
}
