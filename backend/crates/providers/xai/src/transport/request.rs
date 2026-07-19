use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gateway_core::operation::{
    ContentPart, GenerateRequest, ImageSource, MessageRole, OutputFormat, ReasoningEffort,
    ReasoningSummary,
};
use serde_json::{Map, Value, json};
use url::Url;

use super::XAI_PROVIDER_NAME;
use super::config::valid_header_value;

const GROK_OPTION_FIELDS: &[&str] = &[
    "agent_id",
    "conversation_id",
    "schema_version",
    "session_id",
    "transport",
];
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_IMAGE_URL_BYTES: usize = 8 * 1024;

/// Strictly encoded official Responses request and typed Grok header options.
pub struct GrokResponsesRequest {
    body: Map<String, Value>,
    header_options: GrokRequestHeaderOptions,
}

impl GrokResponsesRequest {
    /// Returns the typed JSON object sent to `/v1/responses`.
    #[must_use]
    pub const fn body(&self) -> &Map<String, Value> {
        &self.body
    }

    pub fn encode(
        request: &GenerateRequest,
        upstream_model: &str,
    ) -> Result<Self, GrokRequestEncodeError> {
        let mut body = Map::new();
        body.insert("model".to_owned(), Value::String(upstream_model.to_owned()));
        body.insert("input".to_owned(), Value::Array(encode_messages(request)?));
        body.insert("stream".to_owned(), Value::Bool(true));
        // xAI OAuth session 只执行本轮推理；连续对话由 Gateway portable history 重放。
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

        let header_options = request
            .provider_options()
            .get(XAI_PROVIDER_NAME)
            .map(parse_provider_options)
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            body,
            header_options,
        })
    }

    pub(crate) fn to_json_bytes(&self) -> Result<Vec<u8>, GrokRequestEncodeError> {
        serde_json::to_vec(&self.body).map_err(|_| GrokRequestEncodeError::Serialization)
    }

    pub(crate) const fn header_options(&self) -> &GrokRequestHeaderOptions {
        &self.header_options
    }
}

impl fmt::Debug for GrokResponsesRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokResponsesRequest")
            .field("body_keys", &self.body.keys().collect::<Vec<_>>())
            .field("body", &"<prompt and tool payload redacted>")
            .field("header_options", &self.header_options)
            .finish()
    }
}

#[derive(Clone, Default)]
pub(crate) struct GrokRequestHeaderOptions {
    conversation_id: Option<String>,
    session_id: Option<String>,
    agent_id: Option<String>,
}

impl GrokRequestHeaderOptions {
    pub(crate) fn conversation_id(&self) -> Option<&str> {
        self.conversation_id.as_deref()
    }

    pub(crate) fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub(crate) fn agent_id(&self) -> Option<&str> {
        self.agent_id.as_deref()
    }
}

impl fmt::Debug for GrokRequestHeaderOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrokRequestHeaderOptions")
            .field(
                "conversation_id",
                &self.conversation_id.as_ref().map(|_| "<present>"),
            )
            .field("session_id", &self.session_id.as_ref().map(|_| "<present>"))
            .field("agent_id", &self.agent_id.as_ref().map(|_| "<present>"))
            .finish()
    }
}

/// Generate-to-Responses encoding error that never retains option or prompt
/// values.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum GrokRequestEncodeError {
    /// Grok provider option schema or value is malformed.
    #[error("Grok Build provider option schema is invalid")]
    InvalidProviderOptions,
    /// A Grok-specific option is unknown or unsupported.
    #[error("Grok Build provider option is unsupported")]
    UnsupportedProviderOption,
    /// Canonical content cannot be represented by this Responses adapter.
    #[error("Grok Build request contains unsupported content")]
    UnsupportedContent,
    /// Image URL, MIME type, or size is unsafe.
    #[error("Grok Build request contains an invalid image")]
    InvalidImage,
    /// Typed JSON serialization unexpectedly failed.
    #[error("Grok Build request serialization failed")]
    Serialization,
}

fn encode_messages(request: &GenerateRequest) -> Result<Vec<Value>, GrokRequestEncodeError> {
    let mut input = Vec::new();
    for message in request.messages() {
        let mut content = Vec::new();
        for part in message.content() {
            match part {
                ContentPart::Text(text) => content.push(json!({
                    "type": match message.role() {
                        MessageRole::Assistant => "output_text",
                        MessageRole::System | MessageRole::Developer | MessageRole::User => {
                            "input_text"
                        }
                    },
                    "text": text,
                })),
                ContentPart::Image(source) => {
                    if message.role() != MessageRole::User {
                        return Err(GrokRequestEncodeError::UnsupportedContent);
                    }
                    content.push(json!({
                        "type": "input_image",
                        "image_url": encode_image(source)?,
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
                        return Err(GrokRequestEncodeError::UnsupportedContent);
                    }
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call_id,
                        "name": name,
                        "arguments": arguments,
                    }));
                }
                _ => return Err(GrokRequestEncodeError::UnsupportedContent),
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
        return Err(GrokRequestEncodeError::UnsupportedContent);
    }
    Ok(input)
}

fn encode_image(source: &ImageSource) -> Result<String, GrokRequestEncodeError> {
    match source {
        ImageSource::Url(value) => {
            if value.len() > MAX_IMAGE_URL_BYTES {
                return Err(GrokRequestEncodeError::InvalidImage);
            }
            let url = Url::parse(value).map_err(|_| GrokRequestEncodeError::InvalidImage)?;
            if url.scheme() != "https"
                || url.host().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(GrokRequestEncodeError::InvalidImage);
            }
            Ok(value.clone())
        }
        ImageSource::Bytes { media_type, data } => {
            if data.is_empty()
                || data.len() > MAX_IMAGE_BYTES
                || !matches!(
                    media_type.as_str(),
                    "image/gif" | "image/jpeg" | "image/png" | "image/webp"
                )
            {
                return Err(GrokRequestEncodeError::InvalidImage);
            }
            Ok(format!(
                "data:{media_type};base64,{}",
                STANDARD.encode(data)
            ))
        }
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

fn parse_provider_options(
    options: &Map<String, Value>,
) -> Result<GrokRequestHeaderOptions, GrokRequestEncodeError> {
    if options
        .keys()
        .any(|field| !GROK_OPTION_FIELDS.contains(&field.as_str()))
    {
        return Err(GrokRequestEncodeError::UnsupportedProviderOption);
    }
    if options.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(GrokRequestEncodeError::InvalidProviderOptions);
    }
    if let Some(transport) = options.get("transport")
        && transport.as_str() != Some("http_sse")
    {
        return Err(GrokRequestEncodeError::InvalidProviderOptions);
    }
    Ok(GrokRequestHeaderOptions {
        conversation_id: optional_header_value(options, "conversation_id")?,
        session_id: optional_header_value(options, "session_id")?,
        agent_id: optional_header_value(options, "agent_id")?,
    })
}

fn optional_header_value(
    options: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>, GrokRequestEncodeError> {
    match options.get(field) {
        None => Ok(None),
        Some(Value::String(value)) if valid_header_value(value, 256) => Ok(Some(value.clone())),
        Some(_) => Err(GrokRequestEncodeError::InvalidProviderOptions),
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
