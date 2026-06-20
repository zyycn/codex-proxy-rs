//! OpenAI Chat Completions 请求到 Codex Responses 请求的纯转换。

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::protocol::codex::{
    responses::CodexResponsesRequest,
    schema::{prepare_schema, reconvert_tuple_values},
    sse::{encode_sse_event, parse_sse_events, SseError, DONE_SSE_FRAME},
};

/// OpenAI Chat Completions 请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    /// 客户端请求的模型名。
    pub model: String,
    /// 是否流式返回。
    #[serde(default)]
    pub stream: bool,
    /// 按 OpenAI 语义组织的消息列表。
    pub messages: Vec<ChatMessage>,
    /// 可选的 reasoning effort。
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// 可选的 service tier。
    #[serde(default)]
    pub service_tier: Option<String>,
    /// 原生 tools 字段。
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    /// tool choice 配置。
    #[serde(default)]
    pub tool_choice: Option<Value>,
    /// parallel tool calls 配置。
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    /// 旧式 functions 字段，会转换为 Codex tools。
    #[serde(default)]
    pub functions: Option<Vec<Value>>,
    /// 响应格式配置。
    #[serde(default)]
    pub response_format: Option<Value>,
    /// 可选的 user 字段，用作稳定会话锚点。
    #[serde(default)]
    pub user: Option<String>,
}

/// Chat 请求中的单条消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// 角色名。
    pub role: String,
    /// 消息内容，既可能是字符串也可能是结构化数组。
    #[serde(default)]
    pub content: Option<Value>,
    /// function/tool 相关的名称。
    #[serde(default)]
    pub name: Option<String>,
    /// assistant 消息内的 tool_calls。
    #[serde(default)]
    pub tool_calls: Option<Vec<Value>>,
    /// tool 响应对应的调用 ID。
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// 旧式 function_call 字段。
    #[serde(default)]
    pub function_call: Option<Value>,
}

/// Chat 转译失败的错误。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChatTranslationError {
    /// 请求没有任何消息，无法生成有效上游输入。
    #[error("messages must not be empty")]
    EmptyMessages,
}

/// Chat Completions 流式转换失败。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChatStreamTranslationError {
    /// 上游 SSE 无法解析。
    #[error("invalid upstream SSE response: {0}")]
    InvalidSse(#[from] SseError),
    /// 上游返回错误事件。
    #[error("{0}")]
    Upstream(String),
}

/// 增量转换 Codex Responses SSE 为 OpenAI Chat Completions chunk SSE。
#[derive(Debug)]
pub struct ChatCompletionStreamTranslator {
    pending: String,
    id: String,
    created: i64,
    model: String,
    include_reasoning: bool,
    tuple_schema: Option<Value>,
    tuple_text_buffer: Option<String>,
    has_tool_calls: bool,
    has_content: bool,
    function_call_items: BTreeMap<String, FunctionCallInfo>,
    tool_call_indices: BTreeMap<String, usize>,
    call_ids_with_deltas: BTreeSet<String>,
    next_tool_call_index: usize,
    closed: bool,
}

impl ChatCompletionStreamTranslator {
    /// 构造新的 chat stream 转换器。
    pub fn new(
        model: impl Into<String>,
        include_reasoning: bool,
        tuple_schema: Option<Value>,
    ) -> Self {
        let tuple_text_buffer = tuple_schema.as_ref().map(|_| String::new());
        Self {
            pending: String::new(),
            id: chat_completion_stream_id(),
            created: Utc::now().timestamp(),
            model: model.into(),
            include_reasoning,
            tuple_schema,
            tuple_text_buffer,
            has_tool_calls: false,
            has_content: false,
            function_call_items: BTreeMap::new(),
            tool_call_indices: BTreeMap::new(),
            call_ids_with_deltas: BTreeSet::new(),
            next_tool_call_index: 0,
            closed: false,
        }
    }

    /// TS 版本会先发 assistant role chunk。
    pub fn initial_frame(&self) -> String {
        self.frame(json!({
            "choices": [{
                "delta": {"role": "assistant"},
                "finish_reason": Value::Null,
                "index": 0,
            }],
        }))
    }

    /// 推入一段上游 SSE 文本，返回已完成事件对应的下游 SSE 文本。
    pub fn push_str(&mut self, chunk: &str) -> Result<String, ChatStreamTranslationError> {
        if self.closed {
            return Ok(String::new());
        }
        self.pending.push_str(chunk);
        let mut output = String::new();

        while let Some((event_end, separator_len)) = sse_event_separator(&self.pending) {
            let event_frame = self.pending[..event_end].to_string();
            self.pending.drain(..event_end + separator_len);
            if event_frame.trim().is_empty() {
                continue;
            }
            let events = parse_sse_events(&format!("{event_frame}\n\n"))?;
            for event in events {
                self.push_event(&event, &mut output)?;
            }
        }

        Ok(output)
    }

    fn push_event(
        &mut self,
        event: &crate::protocol::codex::sse::SseEvent,
        output: &mut String,
    ) -> Result<(), ChatStreamTranslationError> {
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            return Ok(());
        };
        let event_type = event
            .event
            .as_deref()
            .or_else(|| value.get("type").and_then(Value::as_str));

        if matches!(event_type, Some("error" | "response.failed")) {
            return Err(ChatStreamTranslationError::Upstream(
                codex_stream_error_message(&value),
            ));
        }

        match event_type {
            Some("response.output_text.delta") => {
                self.push_text_delta(&value, output);
            }
            Some("response.reasoning_summary_text.delta") if self.include_reasoning => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    self.has_content = true;
                    output.push_str(&self.frame(json!({
                        "choices": [{
                            "delta": {"reasoning_content": delta},
                            "finish_reason": Value::Null,
                            "index": 0,
                        }],
                    })));
                }
            }
            Some("response.output_item.added") => {
                self.push_function_call_start(&value, output);
            }
            Some("response.function_call_arguments.delta") => {
                self.push_function_call_delta(&value, output);
            }
            Some("response.function_call_arguments.done") => {
                self.push_function_call_done(&value, output);
            }
            Some("response.output_item.done") => {
                self.push_image_generation_done(&value, output);
            }
            Some("response.completed") => {
                self.push_completed(&value, output);
            }
            _ => {}
        }

        Ok(())
    }

    fn push_text_delta(&mut self, value: &Value, output: &mut String) {
        let Some(delta) = value.get("delta").and_then(Value::as_str) else {
            return;
        };
        self.has_content = true;
        if let Some(buffer) = &mut self.tuple_text_buffer {
            buffer.push_str(delta);
            return;
        }
        output.push_str(&self.frame(json!({
            "choices": [{
                "delta": {"content": delta},
                "finish_reason": Value::Null,
                "index": 0,
            }],
        })));
    }

    fn push_function_call_start(&mut self, value: &Value, output: &mut String) {
        let Some((item_id, call_id, name)) = function_call_item(value.get("item")) else {
            return;
        };
        self.has_tool_calls = true;
        self.has_content = true;
        let index = self.next_tool_call_index;
        self.next_tool_call_index += 1;
        self.function_call_items.insert(
            item_id,
            FunctionCallInfo {
                call_id: call_id.clone(),
                name: name.clone(),
            },
        );
        self.tool_call_indices.insert(call_id.clone(), index);
        output.push_str(&self.frame(json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "function": {"arguments": "", "name": name},
                        "id": call_id,
                        "index": index,
                        "type": "function",
                    }],
                },
                "finish_reason": Value::Null,
                "index": 0,
            }],
        })));
    }

    fn push_function_call_delta(&mut self, value: &Value, output: &mut String) {
        let Some((call_id, delta)) = self.function_call_delta(value) else {
            return;
        };
        self.call_ids_with_deltas.insert(call_id.clone());
        let index = self.tool_call_indices.get(&call_id).copied().unwrap_or(0);
        output.push_str(&self.frame(json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "function": {"arguments": delta},
                        "index": index,
                    }],
                },
                "finish_reason": Value::Null,
                "index": 0,
            }],
        })));
    }

    fn push_function_call_done(&mut self, value: &Value, output: &mut String) {
        let Some((call_id, arguments)) = self.function_call_done(value) else {
            return;
        };
        if self.call_ids_with_deltas.contains(&call_id) {
            return;
        }
        let index = self.tool_call_indices.get(&call_id).copied().unwrap_or(0);
        output.push_str(&self.frame(json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "function": {"arguments": arguments},
                        "index": index,
                    }],
                },
                "finish_reason": Value::Null,
                "index": 0,
            }],
        })));
    }

    fn push_image_generation_done(&mut self, value: &Value, output: &mut String) {
        let Some(item) = value.get("item") else {
            return;
        };
        if item.get("type").and_then(Value::as_str) != Some("image_generation_call") {
            return;
        }
        self.has_tool_calls = true;
        self.has_content = true;
        let index = self.next_tool_call_index;
        self.next_tool_call_index += 1;
        let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
        let mut arguments = json!({
            "result": item.get("result").and_then(Value::as_str).unwrap_or_default(),
        });
        if let Some(revised_prompt) = item.get("revised_prompt") {
            arguments["revised_prompt"] = revised_prompt.clone();
        }
        output.push_str(&self.frame(json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "function": {"arguments": "", "name": "image_generation"},
                        "id": id,
                        "index": index,
                        "type": "function",
                    }],
                },
                "finish_reason": Value::Null,
                "index": 0,
            }],
        })));
        output.push_str(&self.frame(json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "function": {"arguments": arguments.to_string()},
                        "index": index,
                    }],
                },
                "finish_reason": Value::Null,
                "index": 0,
            }],
        })));
    }

    fn push_completed(&mut self, value: &Value, output: &mut String) {
        self.flush_tuple_text(output);
        if !self.has_content {
            output.push_str(&self.frame(json!({
                "choices": [{
                    "delta": {"content": "[Error] Codex returned an empty response. Please retry."},
                    "finish_reason": Value::Null,
                    "index": 0,
                }],
            })));
        }
        let response = value.get("response");
        output.push_str(&self.frame(json!({
            "choices": [{
                "delta": {},
                "finish_reason": if self.has_tool_calls { "tool_calls" } else { "stop" },
                "index": 0,
            }],
            "usage": openai_stream_usage(response.and_then(|response| response.get("usage"))),
        })));
        output.push_str(DONE_SSE_FRAME);
        self.closed = true;
    }

    fn flush_tuple_text(&mut self, output: &mut String) {
        let Some(buffer) = self.tuple_text_buffer.take() else {
            return;
        };
        if buffer.is_empty() {
            return;
        }
        let content = self
            .tuple_schema
            .as_ref()
            .and_then(|tuple_schema| reconvert_tuple_text(&buffer, tuple_schema))
            .unwrap_or(buffer);
        output.push_str(&self.frame(json!({
            "choices": [{
                "delta": {"content": content},
                "finish_reason": Value::Null,
                "index": 0,
            }],
        })));
    }

    fn function_call_delta(&self, value: &Value) -> Option<(String, String)> {
        let event_id = value
            .get("call_id")
            .or_else(|| value.get("item_id"))
            .and_then(Value::as_str)?;
        let delta = value.get("delta").and_then(Value::as_str)?;
        let call_id = self
            .function_call_items
            .get(event_id)
            .map(|info| info.call_id.as_str())
            .unwrap_or(event_id)
            .to_string();
        Some((call_id, delta.to_string()))
    }

    fn function_call_done(&self, value: &Value) -> Option<(String, String)> {
        let event_id = value
            .get("call_id")
            .or_else(|| value.get("item_id"))
            .and_then(Value::as_str)?;
        let call_id = self
            .function_call_items
            .get(event_id)
            .map(|info| info.call_id.as_str())
            .unwrap_or(event_id)
            .to_string();
        let arguments = value
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Some((call_id, arguments))
    }

    fn frame(&self, mut chunk: Value) -> String {
        chunk["id"] = Value::String(self.id.clone());
        chunk["object"] = Value::String("chat.completion.chunk".to_string());
        chunk["created"] = json!(self.created);
        chunk["model"] = Value::String(self.model.clone());
        encode_sse_event("", &chunk.to_string())
    }
}

/// 将 OpenAI Chat Completions 请求纯转换为 Codex Responses 请求。
pub fn translate_chat_to_codex(
    request: ChatCompletionRequest,
) -> Result<CodexResponsesRequest, ChatTranslationError> {
    if request.messages.is_empty() {
        return Err(ChatTranslationError::EmptyMessages);
    }

    let client_conversation_id = request
        .user
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let instructions = chat_instructions(&request.messages);
    let mut input = chat_input(request.messages);
    if input.is_empty() {
        input.push(json!({"role": "user", "content": ""}));
    }

    let mut codex_request = CodexResponsesRequest::new_http_sse(request.model, instructions, input);
    codex_request.force_http_sse = true;
    codex_request.tools = codex_tools(request.tools, request.functions);
    codex_request.tool_choice = request.tool_choice;
    codex_request.parallel_tool_calls = request.parallel_tool_calls;

    let response_format = response_format_text(request.response_format);
    codex_request.text = response_format.text;
    codex_request.tuple_schema = response_format.tuple_schema;
    codex_request.service_tier = request.service_tier;
    codex_request.prompt_cache_key = client_conversation_id.clone();
    codex_request.client_conversation_id = client_conversation_id;

    if let Some(effort) = request.reasoning_effort {
        codex_request.reasoning = Some(json!({"effort": effort, "summary": "auto"}));
    }

    Ok(codex_request)
}

/// 将 Codex SSE 完成响应转换为 OpenAI Chat Completions 响应体。
pub fn chat_completion_from_codex_sse(
    body: &str,
    model: &str,
    include_reasoning: bool,
    tuple_schema: Option<&Value>,
) -> Result<Option<Value>, SseError> {
    let events = parse_sse_events(body)?;
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut function_call_items = BTreeMap::new();
    let mut finished_call_ids = BTreeSet::new();
    let mut tool_calls = Vec::new();
    let mut completed_response = None;

    for event in events {
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        match event.event.as_deref() {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    content.push_str(delta);
                }
            }
            Some("response.reasoning_summary_text.delta") if include_reasoning => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    reasoning.push_str(delta);
                }
            }
            Some("response.output_item.added") => {
                if let Some((item_id, call_id, name)) = function_call_item(value.get("item")) {
                    function_call_items.insert(item_id, FunctionCallInfo { call_id, name });
                }
            }
            Some("response.function_call_arguments.done") => {
                if let Some(tool_call) =
                    tool_call_from_done_event(&value, &function_call_items, &mut finished_call_ids)
                {
                    tool_calls.push(tool_call);
                }
            }
            Some("response.output_item.done") => {
                if let Some(tool_call) = tool_call_from_output_item(
                    value.get("item"),
                    &function_call_items,
                    &mut finished_call_ids,
                ) {
                    tool_calls.push(tool_call);
                }
            }
            Some("response.completed") => {
                completed_response = value.get("response").cloned();
            }
            _ => {}
        }
    }

    let Some(response) = completed_response else {
        return Ok(None);
    };
    if content.is_empty() {
        content = output_text_from_response(&response);
    }
    if let Some(tuple_schema) = tuple_schema {
        content = reconvert_tuple_text(&content, tuple_schema).unwrap_or(content);
    }
    let mut message = json!({
        "role": "assistant",
        "content": if content.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            Value::String(content)
        },
    });
    if include_reasoning && !reasoning.is_empty() {
        message["reasoning_content"] = Value::String(reasoning);
    }
    let has_tool_calls = !tool_calls.is_empty();
    if has_tool_calls {
        message["tool_calls"] = Value::Array(tool_calls);
    }

    Ok(Some(json!({
        "id": format!("chatcmpl-{}", Uuid::new_v4().simple()),
        "object": "chat.completion",
        "created": Utc::now().timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": if has_tool_calls { "tool_calls" } else { "stop" },
        }],
        "usage": openai_usage(response.get("usage")),
    })))
}

fn chat_instructions(messages: &[ChatMessage]) -> String {
    let instructions = messages
        .iter()
        .filter(|message| message.role == "system" || message.role == "developer")
        .map(|message| extract_text(&message.content))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    if instructions.is_empty() {
        "You are a helpful assistant.".to_string()
    } else {
        instructions
    }
}

fn chat_input(messages: Vec<ChatMessage>) -> Vec<Value> {
    let mut input = Vec::new();

    for message in messages {
        match message.role.as_str() {
            "system" | "developer" => {}
            "assistant" => push_assistant_message(&mut input, message),
            "tool" => input.push(json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id.unwrap_or_else(|| "unknown".to_string()),
                "output": extract_text(&message.content),
            })),
            "function" => input.push(json!({
                "type": "function_call_output",
                "call_id": format!("fc_{}", message.name.unwrap_or_else(|| "unknown".to_string())),
                "output": extract_text(&message.content),
            })),
            _ => input.push(json!({
                "role": "user",
                "content": extract_content(&message.content),
            })),
        }
    }

    input
}

fn push_assistant_message(input: &mut Vec<Value>, message: ChatMessage) {
    let text = extract_text(&message.content);
    let has_tool_calls = message
        .tool_calls
        .as_ref()
        .is_some_and(|calls| !calls.is_empty());

    if !text.is_empty() || (!has_tool_calls && message.function_call.is_none()) {
        input.push(json!({"role": "assistant", "content": text}));
    }

    if let Some(tool_calls) = message.tool_calls {
        for tool_call in tool_calls {
            let call_id = tool_call
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let function = tool_call.get("function").unwrap_or(&Value::Null);
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let arguments = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("");
            input.push(json!({
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": arguments,
            }));
        }
    }

    if let Some(function_call) = message.function_call {
        let name = function_call
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let arguments = function_call
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("");
        input.push(json!({
            "type": "function_call",
            "call_id": format!("fc_{name}"),
            "name": name,
            "arguments": arguments,
        }));
    }
}

fn extract_text(content: &Option<Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(parts) = content.as_array() else {
        return String::new();
    };

    parts
        .iter()
        .filter_map(|part| {
            (part.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| part.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn extract_content(content: &Option<Value>) -> Value {
    let Some(content) = content else {
        return Value::String(String::new());
    };
    if content.as_str().is_some() {
        return content.clone();
    }

    let Some(parts) = content.as_array() else {
        return Value::String(String::new());
    };
    let has_image = parts
        .iter()
        .any(|part| part.get("type").and_then(Value::as_str) == Some("image_url"));
    if !has_image {
        return Value::String(extract_text(&Some(content.clone())));
    }

    let codex_parts = parts
        .iter()
        .filter_map(codex_content_part)
        .collect::<Vec<_>>();
    if codex_parts.is_empty() {
        Value::String(String::new())
    } else {
        Value::Array(codex_parts)
    }
}

fn codex_content_part(part: &Value) -> Option<Value> {
    match part.get("type").and_then(Value::as_str)? {
        "text" => part
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "input_text", "text": text})),
        "image_url" => image_url(part).map(|url| json!({"type": "input_image", "image_url": url})),
        _ => None,
    }
}

fn image_url(part: &Value) -> Option<&str> {
    let image = part.get("image_url")?;
    image
        .as_str()
        .or_else(|| image.get("url").and_then(Value::as_str))
}

fn reconvert_tuple_text(text: &str, tuple_schema: &Value) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(text).ok()?;
    Some(reconvert_tuple_values(parsed, tuple_schema).to_string())
}

fn codex_tools(tools: Option<Vec<Value>>, functions: Option<Vec<Value>>) -> Option<Vec<Value>> {
    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        return Some(tools);
    }

    functions
        .filter(|functions| !functions.is_empty())
        .map(|functions| {
            functions
                .into_iter()
                .map(|function| json!({"type": "function", "function": function}))
                .collect()
        })
}

struct PreparedResponseFormat {
    text: Option<Value>,
    tuple_schema: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionCallInfo {
    call_id: String,
    name: String,
}

fn response_format_text(response_format: Option<Value>) -> PreparedResponseFormat {
    let Some(format) = response_format else {
        return PreparedResponseFormat {
            text: None,
            tuple_schema: None,
        };
    };
    let Some(kind) = format.get("type").and_then(Value::as_str) else {
        return PreparedResponseFormat {
            text: None,
            tuple_schema: None,
        };
    };

    match kind {
        "json_object" => PreparedResponseFormat {
            text: Some(json!({"format": {"type": "json_object"}})),
            tuple_schema: None,
        },
        "json_schema" => {
            let Some(schema) = format.get("json_schema") else {
                return PreparedResponseFormat {
                    text: None,
                    tuple_schema: None,
                };
            };
            let name = schema
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("response");
            let prepared_schema =
                prepare_schema(schema.get("schema").cloned().unwrap_or_else(|| json!({})));
            let mut codex_format = json!({
                "type": "json_schema",
                "name": name,
                "schema": prepared_schema.schema,
            });
            if let Some(strict) = schema.get("strict").and_then(Value::as_bool) {
                codex_format["strict"] = Value::Bool(strict);
            }

            PreparedResponseFormat {
                text: Some(json!({"format": codex_format})),
                tuple_schema: prepared_schema.original_schema,
            }
        }
        _ => PreparedResponseFormat {
            text: None,
            tuple_schema: None,
        },
    }
}

fn output_text_from_response(response: &Value) -> String {
    response
        .get("output_text")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| {
            response
                .get("output")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(output_text_from_item)
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .unwrap_or_default()
        })
}

fn output_text_from_item(item: &Value) -> Option<String> {
    let content = item.get("content")?.as_array()?;
    let text = content
        .iter()
        .filter_map(|part| {
            let part_type = part.get("type")?.as_str()?;
            if part_type != "output_text" && part_type != "text" {
                return None;
            }
            part.get("text")?.as_str()
        })
        .collect::<String>();
    (!text.is_empty()).then_some(text)
}

fn function_call_item(item: Option<&Value>) -> Option<(String, String, String)> {
    let item = item?;
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return None;
    }
    let item_id = item.get("id").and_then(Value::as_str)?;
    let call_id = item.get("call_id").and_then(Value::as_str)?;
    let name = item.get("name").and_then(Value::as_str)?;
    Some((item_id.to_string(), call_id.to_string(), name.to_string()))
}

fn tool_call_from_done_event(
    event: &Value,
    function_call_items: &BTreeMap<String, FunctionCallInfo>,
    finished_call_ids: &mut BTreeSet<String>,
) -> Option<Value> {
    let event_id = event
        .get("call_id")
        .or_else(|| event.get("item_id"))
        .and_then(Value::as_str)?;
    let info = function_call_items.get(event_id);
    let call_id = info
        .map(|info| info.call_id.as_str())
        .unwrap_or(event_id)
        .to_string();
    if !finished_call_ids.insert(call_id.clone()) {
        return None;
    }
    let name = event
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| info.map(|info| info.name.as_str()))
        .unwrap_or("unknown");
    let arguments = event
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(openai_tool_call(call_id, name, arguments))
}

fn tool_call_from_output_item(
    item: Option<&Value>,
    function_call_items: &BTreeMap<String, FunctionCallInfo>,
    finished_call_ids: &mut BTreeSet<String>,
) -> Option<Value> {
    let item = item?;
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return None;
    }
    let item_id = item.get("id").and_then(Value::as_str);
    let info = item_id.and_then(|item_id| function_call_items.get(item_id));
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .or_else(|| info.map(|info| info.call_id.as_str()))
        .or(item_id)?;
    if !finished_call_ids.insert(call_id.to_string()) {
        return None;
    }
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| info.map(|info| info.name.as_str()))
        .unwrap_or("unknown");
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(openai_tool_call(call_id.to_string(), name, arguments))
}

fn openai_tool_call(call_id: String, name: &str, arguments: &str) -> Value {
    json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments,
        },
    })
}

fn openai_usage(usage: Option<&Value>) -> Value {
    let prompt_tokens = usage
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let completion_tokens = usage
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let cached_tokens = usage
        .and_then(|usage| usage.get("input_tokens_details"))
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let reasoning_tokens = usage
        .and_then(|usage| usage.get("output_tokens_details"))
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64);
    let mut openai_usage = json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens.saturating_add(completion_tokens),
        "prompt_tokens_details": {
            "cached_tokens": cached_tokens,
        },
    });
    if let Some(reasoning_tokens) = reasoning_tokens {
        openai_usage["completion_tokens_details"] = json!({
            "reasoning_tokens": reasoning_tokens,
        });
    }
    openai_usage
}

fn openai_stream_usage(usage: Option<&Value>) -> Value {
    let Some(usage) = usage else {
        return Value::Null;
    };
    let prompt_tokens = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let completion_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let mut openai_usage = json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens.saturating_add(completion_tokens),
    });
    if let Some(cached_tokens) = usage
        .get("input_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64)
    {
        openai_usage["prompt_tokens_details"] = json!({
            "cached_tokens": cached_tokens,
        });
    }
    if let Some(reasoning_tokens) = usage
        .get("output_tokens_details")
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64)
    {
        openai_usage["completion_tokens_details"] = json!({
            "reasoning_tokens": reasoning_tokens,
        });
    }
    openai_usage
}

fn sse_event_separator(input: &str) -> Option<(usize, usize)> {
    let lf = input.find("\n\n").map(|index| (index, 2));
    let crlf = input.find("\r\n\r\n").map(|index| (index, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

fn chat_completion_stream_id() -> String {
    let suffix = Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(24)
        .collect::<String>();
    format!("chatcmpl-{suffix}")
}

fn codex_stream_error_message(value: &Value) -> String {
    value
        .pointer("/error/message")
        .or_else(|| value.pointer("/response/error/message"))
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or("Upstream Codex response failed")
        .to_string()
}
