//! OpenAI Responses JSON 到 Core 路由事实与不透明 wire payload 的单一解码边界

use crate::data_plane::body::decompress_request_body;
use crate::data_plane::codex::CodexRequestHeaders;
use crate::data_plane::headers::{header_string, passthrough_headers};

use axum::http::HeaderMap;
use bytes::Bytes;
use gateway_core::operation::{GenerateRequest, Operation, ProtocolPayload};
use gateway_protocol::openai::OPENAI_PROTOCOL;
use serde_json::{Map, Value};

use crate::data_plane::body::RequestDecodeError;
use crate::data_plane::generation::{
    ContinuationIntent, DecodedGenerationRequest, GenerationRequestMetadata,
};

const PASSTHROUGH_HEADERS_CONTEXT_KEY: &str = "opaque_request_headers";
const DOWNSTREAM_WEBSOCKET_CONNECTION_ID_CONTEXT_KEY: &str = "downstream_websocket_connection_id";

/// Responses 请求进入共享解码内核时的下游传输来源
#[derive(Clone, Copy)]
pub(super) enum RequestDecodeSource {
    /// 单次 HTTP 请求，请求头与正文属于同一请求
    Http,
    /// 复用连接上的 WebSocket `response.create` 帧
    WebSocketFrame,
}

#[derive(Clone, Default)]
pub struct OpenAiRequestHeaders {
    codex: CodexRequestHeaders,
    downstream_websocket_connection_id: Option<String>,

    conversation_id: Option<String>,
    session_id: Option<String>,
    thread_id: Option<String>,
    client_request_id: Option<String>,

    passthrough_headers: Vec<Value>,
}

impl OpenAiRequestHeaders {
    /// 提取 OpenAI/Codex 连接级请求头上下文
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            codex: CodexRequestHeaders::from_headers(headers),
            downstream_websocket_connection_id: None,
            conversation_id: header_string(headers, "conversation-id")
                .or_else(|| header_string(headers, "conversation_id")),
            session_id: header_string(headers, "session-id")
                .or_else(|| header_string(headers, "session_id")),
            thread_id: header_string(headers, "thread-id"),
            client_request_id: header_string(headers, "x-client-request-id"),
            passthrough_headers: passthrough_headers(headers),
        }
    }

    /// 绑定代理为当前下游 WebSocket 分配的连接身份
    #[must_use]
    pub(super) fn with_downstream_websocket_connection_id(
        mut self,
        connection_id: impl Into<String>,
    ) -> Self {
        self.downstream_websocket_connection_id = Some(connection_id.into());
        self
    }

    fn insert_passthrough_headers(&self, context: &mut Map<String, Value>) {
        if !self.passthrough_headers.is_empty() {
            context.insert(
                PASSTHROUGH_HEADERS_CONTEXT_KEY.to_owned(),
                Value::Array(self.passthrough_headers.clone()),
            );
        }
    }

    fn protocol_context(
        &self,
        use_websocket: Option<bool>,
        source: RequestDecodeSource,
    ) -> Map<String, Value> {
        let mut context = Map::new();
        self.codex.append_context(&mut context);
        if matches!(source, RequestDecodeSource::WebSocketFrame) {
            context.insert(
                gateway_protocol::openai::WEBSOCKET_FRAME_CONTEXT_KEY.to_owned(),
                Value::Bool(true),
            );
        }
        insert_protocol_context(
            &mut context,
            DOWNSTREAM_WEBSOCKET_CONNECTION_ID_CONTEXT_KEY,
            self.downstream_websocket_connection_id.as_ref(),
        );
        insert_protocol_context(
            &mut context,
            "conversation_id",
            self.conversation_id.as_ref(),
        );
        insert_protocol_context(&mut context, "session_id", self.session_id.as_ref());
        insert_protocol_context(&mut context, "thread_id", self.thread_id.as_ref());
        insert_protocol_context(
            &mut context,
            "client_request_id",
            self.client_request_id.as_ref(),
        );
        self.insert_passthrough_headers(&mut context);
        if let Some(use_websocket) = use_websocket {
            context.insert("use_websocket".to_owned(), Value::Bool(use_websocket));
        }
        context
    }
}

fn insert_protocol_context(context: &mut Map<String, Value>, field: &str, value: Option<&String>) {
    if let Some(value) = value {
        context.insert(field.to_owned(), Value::String(value.clone()));
    }
}

/// 使用下游 OpenAI/Codex 请求头解码 `POST /v1/responses`
///
/// # Errors
///
/// 编码不支持、压缩正文损坏或超限、JSON 非法，或路由字段无效时返回安全错误
pub fn decode_request_with_headers(
    body: &[u8],
    headers: &HeaderMap,
    max_decompressed_bytes: usize,
) -> Result<DecodedGenerationRequest, RequestDecodeError> {
    let body = decompress_request_body(body, headers, max_decompressed_bytes)?;
    decode_request_inner(&body, &OpenAiRequestHeaders::from_headers(headers))
}

/// 解码请求并返回已经解除 HTTP content encoding 的中间件正文
///
/// 外层中间件只接收有界的协议正文；terminal 在正文或请求头改写后重新解码，
/// 因而不能把压缩字节与已经移除的传输编码语义混用
pub(crate) fn decode_request_with_body(
    body: &[u8],
    headers: &HeaderMap,
    max_decompressed_bytes: usize,
) -> Result<(DecodedGenerationRequest, Bytes), RequestDecodeError> {
    let body = decompress_request_body(body, headers, max_decompressed_bytes)?;
    let decoded = decode_request_inner(&body, &OpenAiRequestHeaders::from_headers(headers))?;
    Ok((decoded, Bytes::copy_from_slice(&body)))
}

pub(super) fn decode_request_inner(
    body: &[u8],
    request_headers: &OpenAiRequestHeaders,
) -> Result<DecodedGenerationRequest, RequestDecodeError> {
    let value =
        serde_json::from_slice::<Value>(body).map_err(|_| RequestDecodeError::MalformedJson)?;
    let Value::Object(object) = value else {
        return Err(RequestDecodeError::ExpectedObject);
    };
    decode_request_object(object, request_headers, RequestDecodeSource::Http)
}

/// 解码已解析的顶层 object；按下游传输来源恢复连接级协议上下文
pub(super) fn decode_request_object(
    mut object: Map<String, Value>,
    request_headers: &OpenAiRequestHeaders,
    source: RequestDecodeSource,
) -> Result<DecodedGenerationRequest, RequestDecodeError> {
    // 仅消费已识别的本地 transport 开关；未知同名值保留给未来上游协议
    let use_websocket = object.get("use_websocket").and_then(Value::as_bool);
    if use_websocket.is_some() {
        object.remove("use_websocket");
    }

    let model = required_non_empty_string(&object, "model", "model")?;
    if model.trim().is_empty() {
        return Err(RequestDecodeError::EmptyField {
            field: "model".to_owned(),
        });
    }
    let model = model.to_owned();
    // 缺省值只决定下游交付方式，不补写正文或改变 Provider 的上游流式执行
    let stream = match object.get("stream") {
        Some(value) => value.as_bool().unwrap_or(true),
        None => matches!(source, RequestDecodeSource::WebSocketFrame),
    };
    let continuation = object
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(|response_id| ContinuationIntent::PreviousResponseId(response_id.to_owned()))
        .unwrap_or(ContinuationIntent::None);

    let mut protocol_context = request_headers.protocol_context(use_websocket, source);
    let client_turn = request_headers.codex.apply(
        &mut object,
        &mut protocol_context,
        matches!(source, RequestDecodeSource::WebSocketFrame),
    );

    let payload = ProtocolPayload::json_object(OPENAI_PROTOCOL, object)
        .map_err(|_| RequestDecodeError::CanonicalContract {
            field: "request".to_owned(),
        })?
        .with_context(protocol_context);
    let operation = Operation::Generate(
        GenerateRequest::from_protocol_payload(payload).with_client_turn(client_turn),
    );
    Ok(DecodedGenerationRequest {
        operation,
        metadata: GenerationRequestMetadata {
            requested_model: model,
            stream,
            continuation,
            client_ip: None,
            user_agent: None,
        },
    })
}

fn required_non_empty_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<&'a str, RequestDecodeError> {
    let value = object
        .get(key)
        .ok_or_else(|| RequestDecodeError::MissingField {
            field: field.to_owned(),
        })?
        .as_str()
        .ok_or_else(|| RequestDecodeError::InvalidType {
            field: field.to_owned(),
            expected: "a string",
        })?;
    if value.is_empty() {
        return Err(RequestDecodeError::EmptyField {
            field: field.to_owned(),
        });
    }
    Ok(value)
}
