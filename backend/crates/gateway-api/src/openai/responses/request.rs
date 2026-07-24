//! OpenAI Responses JSON 到 Core 路由事实与不透明 wire payload 的单一解码边界。

use std::{fmt, net::IpAddr};

use axum::http::HeaderMap;
use gateway_core::operation::{
    CompactConversationRequest, ContentPart, ContinuationMode, Feature, GenerateRequest,
    ImageSource, JsonSchemaFormat, Message, MessageRole, Operation, OutputFormat, ProtocolPayload,
    ProviderSessionState, ResponsePersistence, ToolDefinition,
};
use gateway_core::routing::ProviderKind;
use gateway_protocol::openai::{
    X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER, X_OPENAI_MEMGEN_REQUEST_HEADER,
};
use serde_json::{Map, Value};

use super::error::RequestDecodeError;

const OPENAI_PROTOCOL: &str = "openai";
const XAI_PROVIDER: &str = "xai";
const REVIEW_SUBAGENT: &str = "review";
const OPENAI_SUBAGENT_KEY: &str = "x-openai-subagent";

#[derive(Clone, Default)]
pub struct OpenAiRequestHeaders {
    turn_state: Option<String>,
    turn_metadata: Option<String>,
    beta_features: Option<String>,
    version: Option<String>,
    include_timing_metrics: Option<String>,
    codex_window_id: Option<String>,
    parent_thread_id: Option<String>,

    conversation_id: Option<String>,
    session_id: Option<String>,
    thread_id: Option<String>,
    client_request_id: Option<String>,
    turn_id: Option<String>,

    responses_lite: Option<String>,
    memgen_request: Option<String>,
    subagent: Option<String>,
}

impl OpenAiRequestHeaders {
    /// 提取 OpenAI/Codex 连接级请求头上下文。
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Self {
        Self {
            turn_state: header_string(headers, "x-codex-turn-state"),
            turn_metadata: header_string(headers, "x-codex-turn-metadata"),
            beta_features: header_string(headers, "x-codex-beta-features"),
            version: header_string(headers, "version"),
            include_timing_metrics: header_string(headers, "x-responsesapi-include-timing-metrics"),
            codex_window_id: header_string(headers, "x-codex-window-id"),
            parent_thread_id: header_string(headers, "x-codex-parent-thread-id"),
            conversation_id: header_string(headers, "conversation-id")
                .or_else(|| header_string(headers, "conversation_id")),
            session_id: header_string(headers, "session-id")
                .or_else(|| header_string(headers, "session_id")),
            thread_id: header_string(headers, "thread-id"),
            client_request_id: header_string(headers, "x-client-request-id"),
            turn_id: header_string(headers, "x-codex-turn-id"),
            responses_lite: header_string(headers, X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER),
            memgen_request: header_string(headers, X_OPENAI_MEMGEN_REQUEST_HEADER),
            subagent: header_string(headers, OPENAI_SUBAGENT_KEY).filter(|value| {
                matches!(
                    value.as_str(),
                    "review" | "compact" | "memory_consolidation" | "collab_spawn"
                )
            }),
        }
    }

    fn apply_subagent(&self, body: &mut Map<String, Value>, forced_subagent: Option<&str>) {
        if let Some(subagent) = forced_subagent.or(self.subagent.as_deref()) {
            inject_subagent_metadata(body, subagent);
        }
    }

    fn protocol_context(&self, use_websocket: Option<bool>) -> Map<String, Value> {
        let mut context = Map::new();
        insert_protocol_context(&mut context, "turn_state", self.turn_state.as_ref());
        insert_protocol_context(&mut context, "turn_metadata", self.turn_metadata.as_ref());
        insert_protocol_context(&mut context, "beta_features", self.beta_features.as_ref());
        insert_protocol_context(&mut context, "version", self.version.as_ref());
        insert_protocol_context(
            &mut context,
            "include_timing_metrics",
            self.include_timing_metrics.as_ref(),
        );
        insert_protocol_context(
            &mut context,
            "codex_window_id",
            self.codex_window_id.as_ref(),
        );
        insert_protocol_context(
            &mut context,
            "parent_thread_id",
            self.parent_thread_id.as_ref(),
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
        insert_protocol_context(&mut context, "turn_id", self.turn_id.as_ref());
        insert_protocol_context(&mut context, "responses_lite", self.responses_lite.as_ref());
        insert_protocol_context(&mut context, "memgen_request", self.memgen_request.as_ref());
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

/// 客户端声明的 continuation 意图。
#[derive(Clone, PartialEq, Eq)]
pub enum ContinuationIntent {
    /// 不使用先前响应。
    None,
    /// 使用当前调用方可见的 OpenAI response ID。
    PreviousResponseId(String),
}

impl ContinuationIntent {
    /// 返回是否请求了对话延续。
    #[must_use]
    pub const fn is_continuation(&self) -> bool {
        matches!(self, Self::PreviousResponseId(_))
    }

    /// 返回待 history owner 解析的 response ID。
    #[must_use]
    pub fn previous_response_id(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::PreviousResponseId(value) => Some(value),
        }
    }
}

impl fmt::Debug for ContinuationIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::PreviousResponseId(_) => formatter.write_str("PreviousResponseId(<redacted>)"),
        }
    }
}

/// Handler、Router 和 history owner 使用的请求元数据。
#[derive(Clone, PartialEq, Eq)]
pub struct ResponsesRequestMetadata {
    requested_model: String,
    public_model: String,
    stream: bool,
    store: bool,
    continuation: ContinuationIntent,
    client_ip: Option<IpAddr>,
    user_agent: Option<String>,
}

impl ResponsesRequestMetadata {
    /// 返回客户端原始模型名。
    #[must_use]
    pub fn requested_model(&self) -> &str {
        &self.requested_model
    }

    /// 返回协议层公开模型候选。
    #[must_use]
    pub fn public_model(&self) -> &str {
        &self.public_model
    }

    /// 返回客户端是否请求 SSE。
    #[must_use]
    pub const fn stream(&self) -> bool {
        self.stream
    }

    /// 返回客户端 storage intent。
    #[must_use]
    pub const fn store(&self) -> bool {
        self.store
    }

    /// 返回 continuation intent。
    #[must_use]
    pub const fn continuation(&self) -> &ContinuationIntent {
        &self.continuation
    }

    /// 返回从 HTTP 连接边界解析出的客户端地址。
    #[must_use]
    pub const fn client_ip(&self) -> Option<IpAddr> {
        self.client_ip
    }

    /// 返回经过 UTF-8 校验和空白归一化的 User-Agent。
    #[must_use]
    pub fn user_agent(&self) -> Option<&str> {
        self.user_agent.as_deref()
    }
}

impl fmt::Debug for ResponsesRequestMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponsesRequestMetadata")
            .field("requested_model", &self.requested_model)
            .field("public_model", &self.public_model)
            .field("stream", &self.stream)
            .field("store", &self.store)
            .field("continuation", &self.continuation)
            .field("client_ip", &self.client_ip)
            .field("has_user_agent", &self.user_agent.is_some())
            .finish()
    }
}

/// 一次成功解码的 Responses 请求。
#[derive(Clone, PartialEq)]
pub struct DecodedResponsesRequest {
    operation: Operation,
    metadata: ResponsesRequestMetadata,
}

impl DecodedResponsesRequest {
    /// 附着当前 WebSocket 连接保存的 Provider 私有上一轮状态。
    #[must_use]
    pub fn with_provider_session_state(mut self, state: ProviderSessionState) -> Self {
        self.operation = self.operation.with_provider_session_state(state);
        self
    }

    /// 返回协议无关 operation。
    #[must_use]
    pub const fn operation(&self) -> &Operation {
        &self.operation
    }

    /// 返回 handler 元数据。
    #[must_use]
    pub const fn metadata(&self) -> &ResponsesRequestMetadata {
        &self.metadata
    }

    /// 拆分为 operation 与元数据。
    #[must_use]
    pub fn into_parts(self) -> (Operation, ResponsesRequestMetadata) {
        (self.operation, self.metadata)
    }

    /// 附着只由 HTTP/WebSocket 连接边界提供的诊断事实。
    #[must_use]
    pub fn with_client_context(
        mut self,
        client_ip: Option<IpAddr>,
        user_agent: Option<String>,
    ) -> Self {
        self.metadata.client_ip = client_ip;
        self.metadata.user_agent = user_agent;
        self
    }
}

impl fmt::Debug for DecodedResponsesRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedResponsesRequest")
            .field("operation", &self.operation)
            .field("metadata", &self.metadata)
            .finish()
    }
}

/// 使用下游 OpenAI/Codex 请求头解码 `POST /v1/responses`。
///
/// # Errors
///
/// JSON 非法、顶层不是 object，或网关必须解释的路由字段无效时返回安全错误。
pub fn decode_request_with_headers(
    body: &[u8],
    headers: &HeaderMap,
    provider_kind: &ProviderKind,
) -> Result<DecodedResponsesRequest, RequestDecodeError> {
    decode_request_inner(
        body,
        false,
        &OpenAiRequestHeaders::from_headers(headers),
        Some(provider_kind),
    )
}

/// 解码 `POST /v1/responses/review` 请求并冻结 review subagent 语义。
pub(super) fn decode_review_request_with_headers(
    body: &[u8],
    headers: &HeaderMap,
    provider_kind: &ProviderKind,
) -> Result<DecodedResponsesRequest, RequestDecodeError> {
    decode_request_inner(
        body,
        true,
        &OpenAiRequestHeaders::from_headers(headers),
        Some(provider_kind),
    )
}

pub(super) fn decode_request_inner(
    body: &[u8],
    review: bool,
    request_headers: &OpenAiRequestHeaders,
    provider_kind: Option<&ProviderKind>,
) -> Result<DecodedResponsesRequest, RequestDecodeError> {
    let value =
        serde_json::from_slice::<Value>(body).map_err(|_| RequestDecodeError::MalformedJson)?;
    let Value::Object(mut object) = value else {
        return Err(RequestDecodeError::ExpectedObject);
    };
    // `use_websocket` 只影响本地 transport。无论值是否可识别，都不得进入上游 body。
    let use_websocket = object
        .remove("use_websocket")
        .and_then(|value| value.as_bool());

    let model = required_non_empty_string(&object, "model", "model")?.trim();
    if model.is_empty() {
        return Err(RequestDecodeError::EmptyField {
            field: "model".to_owned(),
        });
    }
    let model = model.to_owned();
    let compact_conversation = provider_kind
        .is_some_and(|provider| provider.as_str() == XAI_PROVIDER)
        && consume_compaction_trigger(&mut object);

    let stream = object
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let store = object
        .get("store")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let continuation = object
        .get("previous_response_id")
        .and_then(Value::as_str)
        .filter(|response_id| !response_id.is_empty())
        .map(|response_id| ContinuationIntent::PreviousResponseId(response_id.to_owned()))
        .unwrap_or(ContinuationIntent::None);

    request_headers.apply_subagent(&mut object, review.then_some(REVIEW_SUBAGENT));
    let protocol_context = request_headers.protocol_context(use_websocket);

    let messages = canonical_messages(&object);
    let tools = canonical_function_tools(&object);
    let tools_requested = object
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    let image_generation_requested =
        object
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| {
                tools.iter().any(|tool| {
                    tool.get("type").and_then(Value::as_str) == Some("image_generation")
                })
            });
    let vision_requested = contains_type(object.get("input"), "input_image");
    let reasoning_requested = object.get("reasoning").is_some_and(Value::is_object);
    let output_format = canonical_output_format(&object);
    let json_schema_requested = object
        .get("text")
        .and_then(|text| text.get("format"))
        .and_then(|format| format.get("type"))
        .and_then(Value::as_str)
        == Some("json_schema");
    let max_output_tokens = object
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .filter(|tokens| *tokens > 0);
    let prompt_cache_key = object
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .filter(|key| !key.trim().is_empty())
        .map(ToOwned::to_owned);
    let payload = ProtocolPayload::json_object(OPENAI_PROTOCOL, object)
        .map_err(|_| RequestDecodeError::CanonicalContract {
            field: "request".to_owned(),
        })?
        .with_context(protocol_context);
    let mut request = GenerateRequest::from_protocol_payload(messages, payload)
        .with_image_generation_requested(image_generation_requested)
        .with_response_persistence(if store {
            ResponsePersistence::Store
        } else {
            ResponsePersistence::DoNotStore
        });

    if tools_requested {
        request = request.require_feature(Feature::Tools);
    }
    if !tools.is_empty() {
        request = request.with_tools(tools);
    }
    if vision_requested {
        request = request.require_feature(Feature::Vision);
    }
    if reasoning_requested {
        request = request.require_feature(Feature::Reasoning);
    }
    if json_schema_requested {
        request = request.require_feature(Feature::JsonSchema);
    }
    if let Some(format) = output_format {
        request = request.with_output_format(format);
    }
    if let Some(tokens) = max_output_tokens {
        request = request.with_max_output_tokens(tokens);
    }
    if let Some(key) = prompt_cache_key {
        request = request.with_prompt_cache_key(key);
    }
    if continuation.is_continuation() {
        request = request.with_continuation(ContinuationMode::Native);
    }

    let operation = if compact_conversation {
        Operation::CompactConversation(CompactConversationRequest::new(request))
    } else {
        Operation::Generate(request)
    };
    Ok(DecodedResponsesRequest {
        operation,
        metadata: ResponsesRequestMetadata {
            requested_model: model.clone(),
            public_model: model,
            stream,
            store,
            continuation,
            client_ip: None,
            user_agent: None,
        },
    })
}

fn inject_subagent_metadata(body: &mut Map<String, Value>, subagent: &str) {
    let metadata = body
        .entry("client_metadata".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !metadata.is_object() {
        *metadata = Value::Object(Map::new());
    }
    if let Some(metadata) = metadata.as_object_mut() {
        metadata.insert(
            OPENAI_SUBAGENT_KEY.to_owned(),
            Value::String(subagent.to_owned()),
        );
    }
}

fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn canonical_messages(body: &Map<String, Value>) -> Vec<Message> {
    let mut messages = Vec::new();
    if let Some(instructions) = body
        .get("instructions")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        && let Some(message) = message(
            MessageRole::Developer,
            vec![ContentPart::Text(instructions.to_owned())],
        )
    {
        messages.push(message);
    }
    match body.get("input") {
        Some(Value::String(input)) if !input.is_empty() => {
            if let Some(message) =
                message(MessageRole::User, vec![ContentPart::Text(input.clone())])
            {
                messages.push(message);
            }
        }
        Some(Value::Array(input)) => {
            messages.extend(input.iter().filter_map(canonical_input_item));
        }
        _ => {}
    }
    messages
}

fn canonical_input_item(value: &Value) -> Option<Message> {
    let object = value.as_object()?;
    match object.get("type").and_then(Value::as_str) {
        Some("function_call_output") => message(
            MessageRole::User,
            vec![ContentPart::ToolResult {
                call_id: object.get("call_id")?.as_str()?.to_owned(),
                output: object.get("output")?.as_str()?.to_owned(),
            }],
        ),
        Some("function_call") => message(
            MessageRole::Assistant,
            vec![ContentPart::ToolCall {
                call_id: object.get("call_id")?.as_str()?.to_owned(),
                name: object.get("name")?.as_str()?.to_owned(),
                arguments: object.get("arguments")?.as_str()?.to_owned(),
            }],
        ),
        _ => {
            let role = match object.get("role").and_then(Value::as_str)? {
                "system" => MessageRole::System,
                "developer" => MessageRole::Developer,
                "user" => MessageRole::User,
                "assistant" => MessageRole::Assistant,
                _ => return None,
            };
            let content = canonical_content(object.get("content")?, role);
            message(role, content)
        }
    }
}

fn canonical_content(value: &Value, role: MessageRole) -> Vec<ContentPart> {
    match value {
        Value::String(text) if !text.is_empty() => vec![ContentPart::Text(text.clone())],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| canonical_content_part(part, role))
            .collect(),
        _ => Vec::new(),
    }
}

fn canonical_content_part(value: &Value, role: MessageRole) -> Option<ContentPart> {
    let object = value.as_object()?;
    match object.get("type").and_then(Value::as_str)? {
        "input_text" | "output_text" => object
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(|text| ContentPart::Text(text.to_owned())),
        "input_image" if role == MessageRole::User => object
            .get("image_url")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .map(|url| ContentPart::Image(ImageSource::Url(url.to_owned()))),
        _ => None,
    }
}

fn message(role: MessageRole, content: Vec<ContentPart>) -> Option<Message> {
    (!content.is_empty())
        .then(|| Message::new(role, content).ok())
        .flatten()
}

fn canonical_function_tools(body: &Map<String, Value>) -> Vec<ToolDefinition> {
    body.get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let tool = value.as_object()?;
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                return None;
            }
            let name = tool.get("name")?.as_str()?;
            let schema = tool.get("parameters")?.as_object()?.clone();
            ToolDefinition::new(
                name,
                tool.get("description")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                schema,
            )
            .ok()
            .map(|definition| {
                definition.with_strict(tool.get("strict").and_then(Value::as_bool).unwrap_or(false))
            })
        })
        .collect()
}

fn canonical_output_format(body: &Map<String, Value>) -> Option<OutputFormat> {
    let format = body.get("text")?.get("format")?.as_object()?;
    match format.get("type")?.as_str()? {
        "text" => Some(OutputFormat::Text),
        "json_object" => Some(OutputFormat::JsonObject),
        "json_schema" => JsonSchemaFormat::new(
            format.get("name")?.as_str()?,
            format
                .get("description")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            format.get("schema")?.as_object()?.clone(),
            format
                .get("strict")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )
        .ok()
        .map(OutputFormat::JsonSchema),
        _ => None,
    }
}

fn contains_type(value: Option<&Value>, expected: &str) -> bool {
    match value {
        Some(Value::Array(values)) => values
            .iter()
            .any(|value| contains_type(Some(value), expected)),
        Some(Value::Object(object)) => {
            object.get("type").and_then(Value::as_str) == Some(expected)
                || object
                    .values()
                    .any(|value| contains_type(Some(value), expected))
        }
        _ => false,
    }
}

fn consume_compaction_trigger(body: &mut Map<String, Value>) -> bool {
    let Some(Value::Array(input)) = body.get_mut("input") else {
        return false;
    };
    if input
        .last()
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        != Some("compaction_trigger")
    {
        return false;
    }
    input.pop();
    true
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
