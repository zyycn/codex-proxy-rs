//! OpenAI Responses JSON 到 canonical operation 的严格解码。

use std::{fmt, net::IpAddr};

use gateway_core::operation::{
    ContentPart, ContinuationMode, GenerateRequest, ImageSource, JsonSchemaFormat, Message,
    MessageRole, Operation, OutputFormat, ProviderOptions, ReasoningEffort, ReasoningRequirement,
    ReasoningSummary, ResponsePersistence, ToolDefinition,
};
use serde_json::{Map, Value};

use super::error::RequestDecodeError;

/// Gateway 扩展 `provider_options` 的唯一受支持版本。
pub const PROVIDER_OPTIONS_VERSION: &str = "v1";

const TOP_LEVEL_FIELDS: &[&str] = &[
    "input",
    "instructions",
    "max_output_tokens",
    "model",
    "previous_response_id",
    "provider_options",
    "reasoning",
    "store",
    "stream",
    "text",
    "tools",
];

const KNOWN_UNSUPPORTED_TOP_LEVEL_FIELDS: &[&str] = &[
    "background",
    "conversation",
    "context_management",
    "include",
    "max_tool_calls",
    "metadata",
    "moderation",
    "parallel_tool_calls",
    "prompt",
    "prompt_cache_key",
    "prompt_cache_options",
    "prompt_cache_retention",
    "safety_identifier",
    "service_tier",
    "stream_options",
    "temperature",
    "tool_choice",
    "top_logprobs",
    "top_p",
    "truncation",
    "user",
];

/// 客户端声明的 continuation 意图。
#[derive(Clone, PartialEq, Eq)]
pub enum ContinuationIntent {
    /// 不使用先前响应。
    None,
    /// 使用当前调用方可见的 OpenAI response ID。
    ///
    /// History owner 必须在认证后把它解析为调用方隔离的 binding；不得直接
    /// 作为 Provider handle 使用。
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
    ///
    /// Decoder 阶段它与 `requested_model` 相同；Router 冻结 Route Plan 后必须
    /// 以实际解析出的 public model 构造 logical request 事实。
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

/// 严格解码 OpenAI Responses JSON object。
///
/// # Errors
///
/// JSON 非法、字段类型错误、出现未知字段或出现已知但未实现的语义时返回
/// [`RequestDecodeError`]。错误及其 `Debug` 不保存原始 body 或 prompt。
pub fn decode_request(body: &[u8]) -> Result<DecodedResponsesRequest, RequestDecodeError> {
    // UTF-8 wire bytes 是 Provider 无关且不会低估 byte-level token 数量的保守上界；
    // continuation owner 后续还要把恢复历史的 estimate 加入同一 canonical 事实。
    let input_token_estimate =
        u64::try_from(body.len()).map_err(|_| RequestDecodeError::InvalidValue {
            field: "input".to_owned(),
        })?;
    let value =
        serde_json::from_slice::<Value>(body).map_err(|_| RequestDecodeError::MalformedJson)?;
    let object = value
        .as_object()
        .ok_or(RequestDecodeError::ExpectedObject)?;
    reject_top_level_fields(object)?;

    let model = required_non_empty_string(object, "model", "model")?.to_owned();
    if model.len() > 256 || model.chars().any(char::is_control) {
        return Err(RequestDecodeError::InvalidValue {
            field: "model".to_owned(),
        });
    }

    let mut messages = Vec::new();
    if let Some(instructions) = optional_non_empty_string(object, "instructions", "instructions")? {
        messages.push(canonical_message(
            MessageRole::Developer,
            vec![ContentPart::Text(instructions.to_owned())],
            "instructions",
        )?);
    }
    let input = object
        .get("input")
        .ok_or_else(|| RequestDecodeError::MissingField {
            field: "input".to_owned(),
        })?;
    messages.extend(parse_input(input)?);

    let mut request = GenerateRequest::new(messages)
        .map_err(|_| RequestDecodeError::CanonicalContract {
            field: "input".to_owned(),
        })?
        .with_estimated_context_tokens(input_token_estimate);

    if let Some(tools) = object.get("tools") {
        request = request.with_tools(parse_tools(tools)?);
    }
    if let Some(text) = object.get("text") {
        request = request.with_output_format(parse_text_format(text)?);
    }
    if let Some(reasoning) = object.get("reasoning")
        && let Some(reasoning) = parse_reasoning(reasoning)?
    {
        request = request.with_reasoning(reasoning);
    }
    if let Some(tokens) = optional_u64(object, "max_output_tokens", "max_output_tokens")? {
        if tokens == 0 {
            return Err(RequestDecodeError::InvalidValue {
                field: "max_output_tokens".to_owned(),
            });
        }
        request = request.with_max_output_tokens(tokens);
    }

    let continuation =
        match optional_non_empty_string(object, "previous_response_id", "previous_response_id")? {
            Some(response_id) => {
                validate_response_id(response_id)?;
                request = request.with_continuation(ContinuationMode::Native);
                ContinuationIntent::PreviousResponseId(response_id.to_owned())
            }
            None => ContinuationIntent::None,
        };

    if let Some(options) = object.get("provider_options") {
        request = request.with_provider_options(parse_provider_options(options)?);
    }

    let stream = optional_bool(object, "stream", "stream")?.unwrap_or(false);
    let store = optional_bool(object, "store", "store")?.unwrap_or(true);
    request = request.with_response_persistence(if store {
        ResponsePersistence::Store
    } else {
        ResponsePersistence::DoNotStore
    });
    Ok(DecodedResponsesRequest {
        operation: Operation::Generate(request),
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

fn reject_top_level_fields(object: &Map<String, Value>) -> Result<(), RequestDecodeError> {
    for field in object.keys() {
        if TOP_LEVEL_FIELDS.contains(&field.as_str()) {
            continue;
        }
        if KNOWN_UNSUPPORTED_TOP_LEVEL_FIELDS.contains(&field.as_str()) {
            return Err(RequestDecodeError::UnsupportedField {
                field: field.clone(),
            });
        }
        return Err(RequestDecodeError::UnknownField {
            field: field.clone(),
        });
    }
    Ok(())
}

fn parse_input(value: &Value) -> Result<Vec<Message>, RequestDecodeError> {
    match value {
        Value::String(text) => {
            require_non_empty(text, "input")?;
            Ok(vec![canonical_message(
                MessageRole::User,
                vec![ContentPart::Text(text.clone())],
                "input",
            )?])
        }
        Value::Array(items) if items.is_empty() => Err(RequestDecodeError::EmptyField {
            field: "input".to_owned(),
        }),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| parse_input_item(item, index))
            .collect(),
        _ => Err(RequestDecodeError::InvalidType {
            field: "input".to_owned(),
            expected: "a string or array",
        }),
    }
}

fn parse_input_item(value: &Value, index: usize) -> Result<Message, RequestDecodeError> {
    let path = format!("input[{index}]");
    let object = required_object(value, &path)?;
    match object.get("type").and_then(Value::as_str) {
        Some("function_call_output") => parse_function_call_output(object, &path),
        Some("message") | None => parse_message(object, &path),
        Some(_) => Err(RequestDecodeError::UnsupportedField {
            field: format!("{path}.type"),
        }),
    }
}

fn parse_message(object: &Map<String, Value>, path: &str) -> Result<Message, RequestDecodeError> {
    reject_nested_fields(object, &["content", "role", "type"], &[], path)?;
    if let Some(kind) = object.get("type") {
        require_exact_string(kind, "message", &format!("{path}.type"))?;
    }
    let role_path = format!("{path}.role");
    let role = match required_string_value(object, "role", &role_path)? {
        "system" => MessageRole::System,
        "developer" => MessageRole::Developer,
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        _ => {
            return Err(RequestDecodeError::InvalidValue { field: role_path });
        }
    };
    let content_path = format!("{path}.content");
    let content = object
        .get("content")
        .ok_or_else(|| RequestDecodeError::MissingField {
            field: content_path.clone(),
        })?;
    let parts = parse_message_content(content, role, &content_path)?;
    canonical_message(role, parts, &content_path)
}

fn parse_message_content(
    value: &Value,
    role: MessageRole,
    path: &str,
) -> Result<Vec<ContentPart>, RequestDecodeError> {
    match value {
        Value::String(text) => {
            require_non_empty(text, path)?;
            Ok(vec![ContentPart::Text(text.clone())])
        }
        Value::Array(parts) if parts.is_empty() => Err(RequestDecodeError::EmptyField {
            field: path.to_owned(),
        }),
        Value::Array(parts) => parts
            .iter()
            .enumerate()
            .map(|(index, part)| parse_content_part(part, role, &format!("{path}[{index}]")))
            .collect(),
        _ => Err(RequestDecodeError::InvalidType {
            field: path.to_owned(),
            expected: "a string or array",
        }),
    }
}

fn parse_content_part(
    value: &Value,
    role: MessageRole,
    path: &str,
) -> Result<ContentPart, RequestDecodeError> {
    let object = required_object(value, path)?;
    let type_path = format!("{path}.type");
    match required_string_value(object, "type", &type_path)? {
        "input_text" => {
            reject_nested_fields(object, &["text", "type"], &[], path)?;
            let text = required_non_empty_string(object, "text", &format!("{path}.text"))?;
            Ok(ContentPart::Text(text.to_owned()))
        }
        "output_text" if role == MessageRole::Assistant => {
            reject_nested_fields(
                object,
                &["text", "type"],
                &["annotations", "logprobs"],
                path,
            )?;
            let text = required_non_empty_string(object, "text", &format!("{path}.text"))?;
            Ok(ContentPart::Text(text.to_owned()))
        }
        "output_text" => Err(RequestDecodeError::InvalidValue { field: type_path }),
        "input_image" => parse_input_image(object, path),
        _ => Err(RequestDecodeError::UnsupportedField { field: type_path }),
    }
}

fn parse_input_image(
    object: &Map<String, Value>,
    path: &str,
) -> Result<ContentPart, RequestDecodeError> {
    reject_nested_fields(object, &["image_url", "type"], &["detail", "file_id"], path)?;
    let url_path = format!("{path}.image_url");
    let url = required_non_empty_string(object, "image_url", &url_path)?;
    if url.len() > 8 * 1024 || !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(RequestDecodeError::InvalidValue { field: url_path });
    }
    Ok(ContentPart::Image(ImageSource::Url(url.to_owned())))
}

fn parse_function_call_output(
    object: &Map<String, Value>,
    path: &str,
) -> Result<Message, RequestDecodeError> {
    reject_nested_fields(object, &["call_id", "output", "type"], &[], path)?;
    let call_id = required_non_empty_string(object, "call_id", &format!("{path}.call_id"))?;
    let output = required_string_value(object, "output", &format!("{path}.output"))?;
    canonical_message(
        MessageRole::User,
        vec![ContentPart::ToolResult {
            call_id: call_id.to_owned(),
            output: output.to_owned(),
        }],
        path,
    )
}

fn parse_tools(value: &Value) -> Result<Vec<ToolDefinition>, RequestDecodeError> {
    let tools = value
        .as_array()
        .ok_or_else(|| RequestDecodeError::InvalidType {
            field: "tools".to_owned(),
            expected: "an array",
        })?;
    if tools.is_empty() {
        return Ok(Vec::new());
    }
    tools
        .iter()
        .enumerate()
        .map(|(index, tool)| parse_tool(tool, index))
        .collect()
}

fn parse_tool(value: &Value, index: usize) -> Result<ToolDefinition, RequestDecodeError> {
    let path = format!("tools[{index}]");
    let object = required_object(value, &path)?;
    reject_nested_fields(
        object,
        &["description", "name", "parameters", "strict", "type"],
        &[],
        &path,
    )?;
    require_exact_string(
        object
            .get("type")
            .ok_or_else(|| RequestDecodeError::MissingField {
                field: format!("{path}.type"),
            })?,
        "function",
        &format!("{path}.type"),
    )?;
    let name = required_non_empty_string(object, "name", &format!("{path}.name"))?;
    let description =
        optional_non_empty_string(object, "description", &format!("{path}.description"))?
            .map(ToOwned::to_owned);
    let parameters_path = format!("{path}.parameters");
    let parameters = object
        .get("parameters")
        .ok_or_else(|| RequestDecodeError::MissingField {
            field: parameters_path.clone(),
        })?
        .as_object()
        .ok_or(RequestDecodeError::InvalidType {
            field: parameters_path,
            expected: "a JSON object",
        })?
        .clone();
    let strict = optional_bool(object, "strict", &format!("{path}.strict"))?.unwrap_or(false);
    ToolDefinition::new(name, description, parameters)
        .map(|tool| tool.with_strict(strict))
        .map_err(|_| RequestDecodeError::CanonicalContract {
            field: format!("{path}.name"),
        })
}

fn parse_text_format(value: &Value) -> Result<OutputFormat, RequestDecodeError> {
    let object = required_object(value, "text")?;
    reject_nested_fields(object, &["format"], &["verbosity"], "text")?;
    let Some(format) = object.get("format") else {
        return Ok(OutputFormat::Text);
    };
    let format = required_object(format, "text.format")?;
    let format_type = required_string_value(format, "type", "text.format.type")?;
    match format_type {
        "text" => {
            reject_nested_fields(format, &["type"], &[], "text.format")?;
            Ok(OutputFormat::Text)
        }
        "json_object" => {
            reject_nested_fields(format, &["type"], &[], "text.format")?;
            Ok(OutputFormat::JsonObject)
        }
        "json_schema" => parse_json_schema_format(format),
        _ => Err(RequestDecodeError::UnsupportedField {
            field: "text.format.type".to_owned(),
        }),
    }
}

fn parse_json_schema_format(
    object: &Map<String, Value>,
) -> Result<OutputFormat, RequestDecodeError> {
    reject_nested_fields(
        object,
        &["description", "name", "schema", "strict", "type"],
        &[],
        "text.format",
    )?;
    let name = required_non_empty_string(object, "name", "text.format.name")?;
    let description = optional_non_empty_string(object, "description", "text.format.description")?
        .map(ToOwned::to_owned);
    let schema = object
        .get("schema")
        .ok_or_else(|| RequestDecodeError::MissingField {
            field: "text.format.schema".to_owned(),
        })?
        .as_object()
        .ok_or_else(|| RequestDecodeError::InvalidType {
            field: "text.format.schema".to_owned(),
            expected: "a JSON object",
        })?
        .clone();
    let strict = optional_bool(object, "strict", "text.format.strict")?.unwrap_or(false);
    JsonSchemaFormat::new(name, description, schema, strict)
        .map(OutputFormat::JsonSchema)
        .map_err(|_| RequestDecodeError::CanonicalContract {
            field: "text.format.name".to_owned(),
        })
}

fn parse_reasoning(value: &Value) -> Result<Option<ReasoningRequirement>, RequestDecodeError> {
    let object = required_object(value, "reasoning")?;
    reject_nested_fields(object, &["effort", "summary"], &[], "reasoning")?;
    let effort = match object.get("effort") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(match value.as_str() {
            "minimal" => ReasoningEffort::Minimal,
            "low" => ReasoningEffort::Low,
            "medium" => ReasoningEffort::Medium,
            "high" => ReasoningEffort::High,
            "xhigh" => ReasoningEffort::ExtraHigh,
            _ => {
                return Err(RequestDecodeError::InvalidValue {
                    field: "reasoning.effort".to_owned(),
                });
            }
        }),
        Some(_) => {
            return Err(RequestDecodeError::InvalidType {
                field: "reasoning.effort".to_owned(),
                expected: "a string",
            });
        }
    };
    let summary = match object.get("summary") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(match value.as_str() {
            "auto" => ReasoningSummary::Auto,
            "concise" => ReasoningSummary::Concise,
            "detailed" => ReasoningSummary::Detailed,
            "none" => ReasoningSummary::None,
            _ => {
                return Err(RequestDecodeError::InvalidValue {
                    field: "reasoning.summary".to_owned(),
                });
            }
        }),
        Some(_) => {
            return Err(RequestDecodeError::InvalidType {
                field: "reasoning.summary".to_owned(),
                expected: "a string",
            });
        }
    };
    Ok((effort.is_some() || summary.is_some()).then_some(ReasoningRequirement { effort, summary }))
}

fn parse_provider_options(value: &Value) -> Result<ProviderOptions, RequestDecodeError> {
    let object = required_object(value, "provider_options")?;
    reject_nested_fields(object, &["providers", "version"], &[], "provider_options")?;
    let version = required_string_value(object, "version", "provider_options.version")?;
    if version != PROVIDER_OPTIONS_VERSION {
        return Err(RequestDecodeError::UnsupportedProviderOptionsVersion);
    }
    let providers = object
        .get("providers")
        .ok_or_else(|| RequestDecodeError::MissingField {
            field: "provider_options.providers".to_owned(),
        })?
        .as_object()
        .ok_or_else(|| RequestDecodeError::InvalidType {
            field: "provider_options.providers".to_owned(),
            expected: "a JSON object",
        })?;
    let mut options = ProviderOptions::new();
    for (provider, value) in providers {
        let provider_options =
            value
                .as_object()
                .ok_or_else(|| RequestDecodeError::InvalidType {
                    field: format!("provider_options.providers.{provider}"),
                    expected: "a JSON object",
                })?;
        options
            .insert(provider.clone(), provider_options.clone())
            .map_err(|_| RequestDecodeError::CanonicalContract {
                field: format!("provider_options.providers.{provider}"),
            })?;
    }
    Ok(options)
}

fn validate_response_id(response_id: &str) -> Result<(), RequestDecodeError> {
    if response_id.len() > 256
        || !response_id.starts_with("resp_")
        || response_id.chars().any(char::is_control)
    {
        return Err(RequestDecodeError::InvalidValue {
            field: "previous_response_id".to_owned(),
        });
    }
    Ok(())
}

fn canonical_message(
    role: MessageRole,
    parts: Vec<ContentPart>,
    field: &str,
) -> Result<Message, RequestDecodeError> {
    Message::new(role, parts).map_err(|_| RequestDecodeError::CanonicalContract {
        field: field.to_owned(),
    })
}

fn reject_nested_fields(
    object: &Map<String, Value>,
    supported: &[&str],
    known_unsupported: &[&str],
    path: &str,
) -> Result<(), RequestDecodeError> {
    for field in object.keys() {
        if supported.contains(&field.as_str()) {
            continue;
        }
        let field_path = format!("{path}.{field}");
        if known_unsupported.contains(&field.as_str()) {
            return Err(RequestDecodeError::UnsupportedField { field: field_path });
        }
        return Err(RequestDecodeError::UnknownField { field: field_path });
    }
    Ok(())
}

fn required_object<'a>(
    value: &'a Value,
    field: &str,
) -> Result<&'a Map<String, Value>, RequestDecodeError> {
    value
        .as_object()
        .ok_or_else(|| RequestDecodeError::InvalidType {
            field: field.to_owned(),
            expected: "a JSON object",
        })
}

fn required_string_value<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<&'a str, RequestDecodeError> {
    object
        .get(key)
        .ok_or_else(|| RequestDecodeError::MissingField {
            field: field.to_owned(),
        })?
        .as_str()
        .ok_or_else(|| RequestDecodeError::InvalidType {
            field: field.to_owned(),
            expected: "a string",
        })
}

fn required_non_empty_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<&'a str, RequestDecodeError> {
    let value = required_string_value(object, key, field)?;
    require_non_empty(value, field)?;
    Ok(value)
}

fn optional_non_empty_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<Option<&'a str>, RequestDecodeError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            require_non_empty(value, field)?;
            Ok(Some(value))
        }
        Some(_) => Err(RequestDecodeError::InvalidType {
            field: field.to_owned(),
            expected: "a string",
        }),
    }
}

fn optional_bool(
    object: &Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<Option<bool>, RequestDecodeError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(RequestDecodeError::InvalidType {
            field: field.to_owned(),
            expected: "a boolean",
        }),
    }
}

fn optional_u64(
    object: &Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<Option<u64>, RequestDecodeError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => {
            value
                .as_u64()
                .map(Some)
                .ok_or_else(|| RequestDecodeError::InvalidType {
                    field: field.to_owned(),
                    expected: "a non-negative integer",
                })
        }
        Some(_) => Err(RequestDecodeError::InvalidType {
            field: field.to_owned(),
            expected: "a non-negative integer",
        }),
    }
}

fn require_non_empty(value: &str, field: &str) -> Result<(), RequestDecodeError> {
    if value.is_empty() {
        return Err(RequestDecodeError::EmptyField {
            field: field.to_owned(),
        });
    }
    Ok(())
}

fn require_exact_string(
    value: &Value,
    expected: &'static str,
    field: &str,
) -> Result<(), RequestDecodeError> {
    let actual = value
        .as_str()
        .ok_or_else(|| RequestDecodeError::InvalidType {
            field: field.to_owned(),
            expected: "a string",
        })?;
    if actual != expected {
        return Err(RequestDecodeError::UnsupportedField {
            field: field.to_owned(),
        });
    }
    Ok(())
}
