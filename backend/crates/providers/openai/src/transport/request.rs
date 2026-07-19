//! 核心 Generate operation 到 Codex Responses wire request 的严格编码。

use base64::Engine as _;
use gateway_core::operation::{
    ContentPart, GenerateRequest, ImageSource, MessageRole, OutputFormat, ReasoningEffort,
    ReasoningSummary,
};
use serde_json::{Map, Value, json};

use crate::transport::protocol::responses::{
    CodexRequestSemantics, CodexResponsesRequest, codex_request_semantics_from_parts,
    proactive_multi_agent_mode_from_text,
};

const CODEX_OPTION_FIELDS: &[&str] = &[
    "beta_features",
    "client_request_id",
    "codex_window_id",
    "conversation_id",
    "include_timing_metrics",
    "memgen_request",
    "parent_thread_id",
    "prompt_cache_key",
    "responses_lite",
    "schema_version",
    "service_tier",
    "session_id",
    "thread_id",
    "transport",
    "turn_id",
    "turn_metadata",
    "turn_state",
    "version",
];

/// Provider 专属编码错误；不保存 prompt、schema 或 option 值。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodexRequestEncodeError {
    #[error("Codex provider option schema is invalid")]
    InvalidProviderOptions,
    #[error("Codex provider option is unsupported")]
    UnsupportedProviderOption,
    #[error("Codex request contains unsupported content")]
    UnsupportedContent,
}

pub fn encode_generate_request(
    request: &GenerateRequest,
    upstream_model: &str,
) -> Result<CodexResponsesRequest, CodexRequestEncodeError> {
    let mut body = Map::new();
    body.insert("model".to_owned(), Value::String(upstream_model.to_owned()));
    body.insert("input".to_owned(), Value::Array(encode_messages(request)?));
    // ProviderStream 必须统一从 live canonical stream 解码；非流式仅由 client
    // protocol collector 聚合，不能另开一条 Provider 路径。
    body.insert("stream".to_owned(), Value::Bool(true));
    // 客户端每轮携带完整历史；OAuth 上游不代存正文。
    body.insert("store".to_owned(), Value::Bool(false));

    if !request.tools().is_empty() {
        body.insert(
            "tools".to_owned(),
            Value::Array(
                request
                    .tools()
                    .iter()
                    .map(|tool| {
                        let mut value = Map::new();
                        value.insert("type".to_owned(), Value::String("function".to_owned()));
                        value.insert("name".to_owned(), Value::String(tool.name().to_owned()));
                        if let Some(description) = tool.description() {
                            value.insert(
                                "description".to_owned(),
                                Value::String(description.to_owned()),
                            );
                        }
                        value.insert(
                            "parameters".to_owned(),
                            Value::Object(tool.input_schema().clone()),
                        );
                        value.insert("strict".to_owned(), Value::Bool(tool.strict()));
                        Value::Object(value)
                    })
                    .collect(),
            ),
        );
    }

    if !matches!(request.output_format(), OutputFormat::Text) {
        body.insert(
            "text".to_owned(),
            encode_output_format(request.output_format()),
        );
    }
    if let Some(reasoning) = request.reasoning() {
        let mut value = Map::new();
        if let Some(effort) = reasoning.effort {
            value.insert(
                "effort".to_owned(),
                Value::String(reasoning_effort(effort).to_owned()),
            );
        }
        if let Some(summary) = reasoning.summary {
            value.insert(
                "summary".to_owned(),
                Value::String(reasoning_summary(summary).to_owned()),
            );
        }
        body.insert("reasoning".to_owned(), Value::Object(value));
    }
    if let Some(tokens) = request.max_output_tokens() {
        body.insert("max_output_tokens".to_owned(), Value::from(tokens));
    }

    let mut encoded = CodexResponsesRequest::from_body(body);
    if let Some(options) = request.provider_options().get("openai") {
        apply_codex_options(&mut encoded, options)?;
    }
    Ok(encoded)
}

/// 复用正式 Codex 编码规则提取请求诊断语义；编码失败时不伪造 Provider 事实。
#[must_use]
pub fn codex_request_semantics(request: &GenerateRequest) -> CodexRequestSemantics {
    let turn_metadata = request
        .provider_options()
        .get("openai")
        .and_then(|options| options.get("turn_metadata"))
        .and_then(Value::as_str);
    let effort = request
        .reasoning()
        .and_then(|reasoning| reasoning.effort)
        .map(reasoning_effort);
    let proactive_multi_agent = request
        .messages()
        .iter()
        .rev()
        .filter(|message| message.role() == MessageRole::Developer)
        .find_map(|message| {
            message.content().iter().rev().find_map(|content| {
                let ContentPart::Text(text) = content else {
                    return None;
                };
                proactive_multi_agent_mode_from_text(text)
            })
        })
        .unwrap_or(false);
    codex_request_semantics_from_parts(turn_metadata, effort, proactive_multi_agent, false)
}

fn encode_messages(request: &GenerateRequest) -> Result<Vec<Value>, CodexRequestEncodeError> {
    let mut input = Vec::new();
    for message in request.messages() {
        let mut content = Vec::new();
        for part in message.content() {
            match part {
                ContentPart::Text(text) => content.push(json!({
                    "type": match message.role() {
                        MessageRole::Assistant => "output_text",
                        MessageRole::System
                        | MessageRole::Developer
                        | MessageRole::User => "input_text",
                    },
                    "text": text,
                })),
                ContentPart::Image(source) => {
                    if message.role() != MessageRole::User {
                        return Err(CodexRequestEncodeError::UnsupportedContent);
                    }
                    content.push(json!({
                        "type": "input_image",
                        "image_url": image_url(source),
                    }));
                }
                ContentPart::ToolResult { call_id, output } => {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": output,
                    }));
                }
                ContentPart::ToolCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    if message.role() != MessageRole::Assistant {
                        return Err(CodexRequestEncodeError::UnsupportedContent);
                    }
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call_id,
                        "name": name,
                        "arguments": arguments,
                    }));
                }
                _ => return Err(CodexRequestEncodeError::UnsupportedContent),
            }
        }
        if !content.is_empty() {
            input.push(json!({
                "type": "message",
                "role": message_role(message.role()),
                "content": content,
            }));
        }
    }
    if input.is_empty() {
        return Err(CodexRequestEncodeError::UnsupportedContent);
    }
    Ok(input)
}

fn image_url(source: &ImageSource) -> String {
    match source {
        ImageSource::Url(url) => url.clone(),
        ImageSource::Bytes { media_type, data } => format!(
            "data:{media_type};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(data)
        ),
    }
}

fn encode_output_format(format: &OutputFormat) -> Value {
    let format = match format {
        OutputFormat::Text => json!({"type": "text"}),
        OutputFormat::JsonObject => json!({"type": "json_object"}),
        OutputFormat::JsonSchema(schema) => {
            let mut value = Map::new();
            value.insert("type".to_owned(), Value::String("json_schema".to_owned()));
            value.insert("name".to_owned(), Value::String(schema.name().to_owned()));
            if let Some(description) = schema.description() {
                value.insert(
                    "description".to_owned(),
                    Value::String(description.to_owned()),
                );
            }
            value.insert("schema".to_owned(), Value::Object(schema.schema().clone()));
            value.insert("strict".to_owned(), Value::Bool(schema.strict()));
            Value::Object(value)
        }
    };
    json!({"format": format})
}

fn apply_codex_options(
    request: &mut CodexResponsesRequest,
    options: &Map<String, Value>,
) -> Result<(), CodexRequestEncodeError> {
    if options
        .keys()
        .any(|field| !CODEX_OPTION_FIELDS.contains(&field.as_str()))
    {
        return Err(CodexRequestEncodeError::UnsupportedProviderOption);
    }
    if options.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(CodexRequestEncodeError::InvalidProviderOptions);
    }

    request.turn_state = optional_string(options, "turn_state")?;
    request.turn_metadata = optional_string(options, "turn_metadata")?;
    request.beta_features = optional_string(options, "beta_features")?;
    request.version = optional_string(options, "version")?;
    request.include_timing_metrics = optional_string(options, "include_timing_metrics")?;
    request.codex_window_id = optional_string(options, "codex_window_id")?;
    request.parent_thread_id = optional_string(options, "parent_thread_id")?;
    request.client_conversation_id = optional_string(options, "conversation_id")?;
    request.local_conversation_id = request.client_conversation_id.clone();
    request.client_session_id = optional_string(options, "session_id")?;
    request.client_thread_id = optional_string(options, "thread_id")?;
    request.client_request_id = optional_string(options, "client_request_id")?;
    request.client_turn_id = optional_string(options, "turn_id")?;
    request.responses_lite = optional_string(options, "responses_lite")?;
    request.memgen_request = optional_string(options, "memgen_request")?;
    match optional_string(options, "transport")?.as_deref() {
        Some("http_sse") => encoded_transport(request, false),
        Some("websocket") => encoded_transport(request, true),
        Some(_) => return Err(CodexRequestEncodeError::InvalidProviderOptions),
        None => {}
    }

    for field in ["prompt_cache_key", "service_tier"] {
        if let Some(value) = optional_string(options, field)? {
            request
                .body_mut()
                .insert(field.to_owned(), Value::String(value));
        }
    }
    Ok(())
}

fn encoded_transport(request: &mut CodexResponsesRequest, websocket: bool) {
    request.force_http_sse = !websocket;
    request.force_websocket = websocket;
    request.use_websocket = websocket;
}

fn optional_string(
    options: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>, CodexRequestEncodeError> {
    match options.get(field) {
        None => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() && value.len() <= 8_192 => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(CodexRequestEncodeError::InvalidProviderOptions),
    }
}

const fn message_role(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::Developer => "developer",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}

const fn reasoning_effort(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::ExtraHigh => "xhigh",
    }
}

const fn reasoning_summary(summary: ReasoningSummary) -> &'static str {
    match summary {
        ReasoningSummary::Auto => "auto",
        ReasoningSummary::Concise => "concise",
        ReasoningSummary::Detailed => "detailed",
        ReasoningSummary::None => "none",
    }
}
