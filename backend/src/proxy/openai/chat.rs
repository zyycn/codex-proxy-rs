//! OpenAI Chat Completions 请求到 Codex Responses 请求的纯转换，以及 HTTP 处理器。

use std::collections::{BTreeMap, BTreeSet};

use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::Utc;
use futures::{stream as futures_stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::convert::Infallible;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    http::middleware::request_id::RequestId,
    proxy::{
        auth::authorize_client_api_key,
        dispatch::responses::{errors::ResponseDispatchError, service::ResponseDispatchStream},
    },
    runtime::state::AppState,
    upstream::{
        models::catalog::ModelCatalog,
        protocol::{
            responses::{apply_response_model_options, CodexResponsesRequest},
            schema::{prepare_schema, reconvert_tuple_values},
            sse::{
                encode_sse_event, parse_sse_events, sse_frame_separator, SseError, DONE_SSE_FRAME,
            },
        },
    },
};

use super::{
    errors::{
        chat_dispatch_error_response, chat_stream_dispatch_openai_error,
        invalid_chat_completion_request_response, missing_client_api_key_response,
        model_not_found_response,
    },
    models::model_catalog_for_state,
    sse::{event_stream_response, openai_sse_frame, SseResponseOptions},
};

// ====================================================================
// OpenAI 协议类型
// ====================================================================

/// OpenAI Chat Completions 请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    #[serde(default)]
    pub stream: bool,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default)]
    pub functions: Option<Vec<Value>>,
    #[serde(default)]
    pub response_format: Option<Value>,
    #[serde(default)]
    pub user: Option<String>,
}

/// Chat 请求中的单条消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub function_call: Option<Value>,
}

// ====================================================================
// 转换错误
// ====================================================================

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChatTranslationError {
    #[error("messages must not be empty")]
    EmptyMessages,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChatStreamTranslationError {
    #[error("invalid upstream SSE response: {0}")]
    InvalidSse(#[from] SseError),
    #[error("{message}")]
    Upstream {
        message: String,
        error_type: String,
        code: String,
    },
}

// ====================================================================
// 流式转换器
// ====================================================================

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

    pub fn initial_frame(&self) -> String {
        self.frame(json!({
            "choices": [{
                "delta": {"role": "assistant"},
                "finish_reason": Value::Null,
                "index": 0,
            }],
        }))
    }

    pub fn push_str(&mut self, chunk: &str) -> Result<String, ChatStreamTranslationError> {
        if self.closed {
            return Ok(String::new());
        }
        self.pending.push_str(chunk);
        let mut output = String::new();

        while let Some((event_end, separator_len)) = sse_frame_separator(&self.pending) {
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
        event: &crate::upstream::protocol::sse::SseEvent,
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
            let (message, error_type, code) = codex_stream_error_details(&value);
            return Err(ChatStreamTranslationError::Upstream {
                message,
                error_type,
                code,
            });
        }

        match event_type {
            Some("response.output_text.delta") => {
                self.push_text_delta(&value, output);
            }
            Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta")
                if self.include_reasoning =>
            {
                self.push_reasoning_delta(&value, output);
            }
            Some("response.output_item.added") => {
                self.push_function_call_start(&value, output);
            }
            Some(
                "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta",
            ) => {
                self.push_function_call_delta(&value, output);
            }
            Some("response.function_call_arguments.done") => {
                self.push_function_call_done(&value, output);
            }
            Some("response.output_item.done") => {
                self.push_output_item_done(&value, output);
            }
            Some("response.completed" | "response.incomplete") => {
                self.push_terminal_response(&value, output);
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

    fn push_reasoning_delta(&mut self, value: &Value, output: &mut String) {
        let Some(delta) = value.get("delta").and_then(Value::as_str) else {
            return;
        };
        self.has_content = true;
        output.push_str(&self.frame(json!({
            "choices": [{
                "delta": {"reasoning_content": delta},
                "finish_reason": Value::Null,
                "index": 0,
            }],
        })));
    }

    fn push_function_call_start(&mut self, value: &Value, output: &mut String) {
        let Some((item_id, call_id, name)) = function_call_item(value.get("item")) else {
            return;
        };
        self.ensure_function_call_started(item_id, call_id, name, output);
    }

    fn ensure_function_call_started(
        &mut self,
        item_id: String,
        call_id: String,
        name: String,
        output: &mut String,
    ) {
        self.function_call_items
            .entry(item_id)
            .or_insert_with(|| FunctionCallInfo {
                call_id: call_id.clone(),
                name: name.clone(),
            });
        if self.tool_call_indices.contains_key(&call_id) {
            return;
        }
        self.has_tool_calls = true;
        self.has_content = true;
        let index = self.next_tool_call_index;
        self.next_tool_call_index += 1;
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

    fn push_output_item_done(&mut self, value: &Value, output: &mut String) {
        if self.push_image_generation_done(value, output) {
            return;
        }
        self.push_function_call_output_item_done(value, output);
    }

    fn push_image_generation_done(&mut self, value: &Value, output: &mut String) -> bool {
        let Some(item) = value.get("item") else {
            return false;
        };
        if item.get("type").and_then(Value::as_str) != Some("image_generation_call") {
            return false;
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
        true
    }

    fn push_function_call_output_item_done(&mut self, value: &Value, output: &mut String) {
        let Some((item_id, call_id, name, arguments)) =
            tool_call_output_item(value.get("item"), &self.function_call_items)
        else {
            return;
        };
        self.ensure_function_call_started(item_id, call_id.clone(), name, output);
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

    fn push_terminal_response(&mut self, value: &Value, output: &mut String) {
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
        let finish_reason = openai_finish_reason_for_response(response, self.has_tool_calls);
        output.push_str(&self.frame(json!({
            "choices": [{
                "delta": {},
                "finish_reason": finish_reason,
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

    fn frame(&self, mut chunk: Value) -> String {
        chunk["id"] = Value::String(self.id.clone());
        chunk["object"] = Value::String("chat.completion.chunk".to_string());
        chunk["created"] = json!(self.created);
        chunk["model"] = Value::String(self.model.clone());
        encode_sse_event("", &chunk.to_string())
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
            .map_or(event_id, |info| info.call_id.as_str())
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
            .map_or(event_id, |info| info.call_id.as_str())
            .to_string();
        let arguments = value
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Some((call_id, arguments))
    }
}

// ====================================================================
// 非流式转换：Chat → Codex Responses
// ====================================================================

/// 转换 OpenAI Chat 请求到 Codex Responses 请求。
pub fn translate_chat_to_codex(
    request: ChatCompletionRequest,
) -> Result<CodexResponsesRequest, ChatTranslationError> {
    if request.messages.is_empty() {
        return Err(ChatTranslationError::EmptyMessages);
    }

    let prepared = response_format_text(request.response_format);
    let user = request
        .user
        .as_deref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let use_websocket = user.is_some();
    let codex = CodexResponsesRequest::new_http_sse(
        request.model,
        String::new(),
        sanitize_chat_messages(&request.messages),
    );
    Ok(CodexResponsesRequest {
        instructions: String::new(),
        input: sanitize_chat_messages(&request.messages),
        text: prepared.text,
        tuple_schema: prepared.tuple_schema,
        tools: codex_tools(request.tools, request.functions),
        tool_choice: codex_tool_choice(request.tool_choice),
        parallel_tool_calls: request.parallel_tool_calls,
        reasoning: request.reasoning_effort.map(|effort| {
            json!({
                "effort": effort,
                "summary": "auto",
            })
        }),
        service_tier: request.service_tier,
        prompt_cache_key: user.clone(),
        client_conversation_id: user,
        use_websocket,
        ..codex
    })
}

fn sanitize_chat_messages(messages: &[ChatMessage]) -> Vec<Value> {
    let mut input = Vec::with_capacity(messages.len());
    for message in messages {
        match message.role.as_str() {
            "system" => push_system_message(&mut input, message),
            "user" => push_user_message(&mut input, message),
            "assistant" => push_assistant_message(&mut input, message),
            "tool" | "function" => push_tool_message(&mut input, message),
            _ => push_user_message(&mut input, message),
        }
    }
    input
}

fn push_system_message(input: &mut Vec<Value>, message: &ChatMessage) {
    let text = extract_text(&message.content);
    input.push(json!({"role": "system", "content": text}));
}

fn push_user_message(input: &mut Vec<Value>, message: &ChatMessage) {
    let content = extract_content(&message.content);
    input.push(json!({"role": "user", "content": content}));
}

fn push_tool_message(input: &mut Vec<Value>, message: &ChatMessage) {
    let call_id = message.tool_call_id.as_deref().unwrap_or("unknown");
    let content = extract_text(&message.content);
    input.push(json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": content,
    }));
}

fn push_assistant_message(input: &mut Vec<Value>, message: &ChatMessage) {
    let text = extract_text(&message.content);
    let has_tool_calls = message
        .tool_calls
        .as_ref()
        .is_some_and(|calls| !calls.is_empty());

    if !text.is_empty() || (!has_tool_calls && message.function_call.is_none()) {
        input.push(json!({"role": "assistant", "content": text}));
    }

    if let Some(tool_calls) = &message.tool_calls {
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

    if let Some(function_call) = &message.function_call {
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
    let mut codex_tools = Vec::new();
    if let Some(tools) = tools {
        codex_tools.extend(tools.into_iter().map(codex_tool));
    }
    if let Some(functions) = functions {
        codex_tools.extend(functions.into_iter().map(codex_function_tool));
    }
    (!codex_tools.is_empty()).then_some(codex_tools)
}

fn codex_tool(tool: Value) -> Value {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return tool;
    }
    if let Some(function) = tool.get("function").filter(|value| value.is_object()) {
        return codex_function_tool(function.clone());
    }
    codex_function_tool(tool)
}

fn codex_function_tool(function: Value) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("function".to_string()));
    for field in ["name", "description", "parameters", "strict"] {
        if let Some(value) = function.get(field) {
            object.insert(field.to_string(), value.clone());
        }
    }
    Value::Object(object)
}

fn codex_tool_choice(tool_choice: Option<Value>) -> Option<Value> {
    tool_choice.map(codex_tool_choice_value)
}

fn codex_tool_choice_value(choice: Value) -> Value {
    if choice.get("type").and_then(Value::as_str) != Some("function") {
        return choice;
    }
    let name = choice
        .get("name")
        .or_else(|| choice.pointer("/function/name"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty());
    let Some(name) = name else {
        return choice;
    };
    json!({
        "type": "function",
        "name": name,
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

// ====================================================================
// 非流式 Chat 响应：Codex SSE → OpenAI Chat JSON
// ====================================================================

/// 从 Codex Responses SSE 收集 Chat Completions 响应。
pub fn chat_completion_from_codex_sse(
    body: &str,
    model: &str,
    include_reasoning: bool,
    tuple_schema: Option<&Value>,
) -> Result<Option<Value>, ChatStreamTranslationError> {
    let events = parse_sse_events(body)?;
    let mut output_text = String::new();
    let mut reasoning_text = String::new();
    let mut function_call_items: BTreeMap<String, FunctionCallInfo> = BTreeMap::new();
    let mut finished_call_ids: BTreeSet<String> = BTreeSet::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut completed_response = None;
    let mut failed_response = None;

    for event in events {
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        let event_type = event
            .event
            .as_deref()
            .or_else(|| value.get("type").and_then(Value::as_str));

        if matches!(event_type, Some("error" | "response.failed")) {
            failed_response = Some(codex_stream_error_details(&value));
            continue;
        }

        match event_type {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    if let Some(tuple_schema) = tuple_schema {
                        if let Ok(parsed) = serde_json::from_str::<Value>(delta) {
                            output_text.push_str(
                                &reconvert_tuple_values(parsed, tuple_schema).to_string(),
                            );
                        } else {
                            output_text.push_str(delta);
                        }
                    } else {
                        output_text.push_str(delta);
                    }
                }
            }
            Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta")
                if include_reasoning =>
            {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    reasoning_text.push_str(delta);
                }
            }
            Some("response.output_item.added") => {
                if let Some((item_id, call_id, name)) = function_call_item(value.get("item")) {
                    function_call_items.insert(
                        item_id,
                        FunctionCallInfo {
                            call_id: call_id.clone(),
                            name: name.clone(),
                        },
                    );
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
            Some("response.completed" | "response.incomplete") => {
                completed_response = value.get("response").cloned();
            }
            _ => {}
        }
    }

    if let Some((message, error_type, code)) = failed_response {
        return Err(ChatStreamTranslationError::Upstream {
            message,
            error_type,
            code,
        });
    }

    let Some(response) = completed_response else {
        return Ok(None);
    };
    let output_text_value = if output_text.is_empty() {
        output_text_from_response(&response)
    } else {
        output_text
    };

    let mut choices = Vec::new();
    let finish_reason = openai_finish_reason_for_response(Some(&response), !tool_calls.is_empty());
    let mut message = serde_json::Map::new();
    message.insert("role".to_string(), Value::String("assistant".to_string()));
    message.insert("content".to_string(), Value::String(output_text_value));
    if include_reasoning && !reasoning_text.is_empty() {
        message.insert(
            "reasoning_content".to_string(),
            Value::String(reasoning_text),
        );
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_string(), Value::Array(tool_calls));
        choices.push(json!({
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": finish_reason,
        }));
    } else {
        choices.push(json!({
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": finish_reason,
        }));
    }

    let usage = openai_usage(response.get("usage"));

    let body = json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().simple().to_string().chars().take(24).collect::<String>()),
        "object": "chat.completion",
        "created": Utc::now().timestamp(),
        "model": model,
        "choices": choices,
        "usage": usage,
    });

    Ok(Some(body))
}

// ====================================================================
// HTTP 处理器
// ====================================================================

/// `POST /v1/chat/completions`
pub async fn chat_completions(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorize_client_api_key(&state, &headers).await {
        return missing_client_api_key_response().into_response();
    }

    let Ok(chat_request) = serde_json::from_slice::<ChatCompletionRequest>(&body) else {
        return invalid_chat_completion_request_response().into_response();
    };
    let model = chat_request.model.clone();
    let catalog = model_catalog_for_state(&state).await;
    if !catalog.is_recognized_model_name(&model) {
        return model_not_found_response().into_response();
    }
    let parsed_model = catalog.parse_model_name(&model);
    let display_model = ModelCatalog::build_display_model_name(&parsed_model);
    let stream = chat_request.stream;
    let Ok(mut codex_request) = translate_chat_to_codex(chat_request) else {
        return invalid_chat_completion_request_response().into_response();
    };
    super::responses::attach_client_context(&mut codex_request, &headers);
    apply_response_model_options(&mut codex_request, &parsed_model);
    let include_reasoning = codex_request
        .reasoning
        .as_ref()
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(Value::as_str)
        .is_some_and(|effort| !effort.trim().is_empty());
    let tuple_schema = codex_request.tuple_schema.clone();

    if stream {
        return match state
            .services
            .responses
            .stream(
                request_id.as_str(),
                "/v1/chat/completions",
                codex_request,
                &model,
            )
            .await
        {
            Ok(stream) => live_chat_event_stream_response(
                stream,
                &display_model,
                include_reasoning,
                tuple_schema,
            ),
            Err(error) => response_dispatch_chat_stream_error_response(&error),
        };
    }

    match state
        .services
        .chat
        .complete(request_id.as_str(), codex_request, &model)
        .await
    {
        Ok(body) => (StatusCode::OK, Json(body)).into_response(),
        Err(error) => chat_dispatch_error_response(&error),
    }
}

fn live_chat_event_stream_response(
    stream: ResponseDispatchStream,
    model: &str,
    include_reasoning: bool,
    tuple_schema: Option<Value>,
) -> Response {
    let mut translator =
        ChatCompletionStreamTranslator::new(model.to_string(), include_reasoning, tuple_schema);
    let initial_frame = translator.initial_frame();
    let mut terminated_with_error = false;
    let body_stream =
        futures_stream::once(async move { Ok::<Bytes, Infallible>(Bytes::from(initial_frame)) })
            .chain(stream.body.map(move |result| {
                if terminated_with_error {
                    return Ok::<Bytes, Infallible>(Bytes::new());
                }
                let body = match result {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        match translator.push_str(&text) {
                            Ok(body) => body,
                            Err(error) => {
                                terminated_with_error = true;
                                let (error_type, code, message) =
                                    chat_stream_translation_openai_error(&error);
                                chat_stream_error_sse_frame(&error_type, &code, &message)
                            }
                        }
                    }
                    Err(error) => {
                        terminated_with_error = true;
                        chat_stream_error_sse_frame(
                            "server_error",
                            "upstream_error",
                            &error.to_string(),
                        )
                    }
                };
                Ok::<Bytes, Infallible>(Bytes::from(body))
            }));

    event_stream_response(
        Body::from_stream(body_stream),
        SseResponseOptions::LIVE_CHAT,
    )
}

fn response_dispatch_chat_stream_error_response(error: &ResponseDispatchError) -> Response {
    let error = chat_stream_dispatch_openai_error(error);
    chat_stream_error_response(error.error_type, error.code, &error.message)
}

fn chat_stream_error_response(error_type: &str, code: &str, message: &str) -> Response {
    event_stream_response(
        Body::from(chat_stream_error_sse_frame(error_type, code, message)),
        SseResponseOptions::CHAT_ERROR,
    )
}

fn chat_stream_error_sse_frame(error_type: &str, code: &str, message: &str) -> String {
    format!(
        "{}{}",
        openai_sse_frame(
            "",
            &json!({
                "error": {
                    "message": message,
                    "type": error_type,
                    "code": code,
                }
            })
            .to_string(),
        ),
        DONE_SSE_FRAME
    )
}

// ====================================================================
// 辅助函数
// ====================================================================

fn output_text_from_response(response: &Value) -> String {
    response
        .get("output_text")
        .and_then(Value::as_str)
        .map_or_else(
            || {
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
            },
            ToString::to_string,
        )
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

fn openai_finish_reason_for_response(
    response: Option<&Value>,
    has_tool_calls: bool,
) -> &'static str {
    let Some(response) = response else {
        return openai_default_finish_reason(has_tool_calls);
    };
    if response.get("status").and_then(Value::as_str) != Some("incomplete") {
        return openai_default_finish_reason(has_tool_calls);
    }

    match response_incomplete_reason(response) {
        Some("max_output_tokens") => "length",
        Some("content_filter") => "content_filter",
        _ => openai_default_finish_reason(has_tool_calls),
    }
}

fn response_incomplete_reason(response: &Value) -> Option<&str> {
    response
        .pointer("/incomplete_details/reason")
        .or_else(|| response.get("reason"))
        .and_then(Value::as_str)
}

fn openai_default_finish_reason(has_tool_calls: bool) -> &'static str {
    if has_tool_calls {
        "tool_calls"
    } else {
        "stop"
    }
}

fn function_call_item(item: Option<&Value>) -> Option<(String, String, String)> {
    let item = item?;
    let item_type = item.get("type").and_then(Value::as_str)?;
    if !matches!(item_type, "function_call" | "custom_tool_call") {
        return None;
    }
    let item_id = item.get("id").and_then(Value::as_str)?;
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or(item_id);
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or(item_type);
    Some((item_id.to_string(), call_id.to_string(), name.to_string()))
}

fn tool_arguments_from_value(value: &Value) -> &str {
    value
        .get("arguments")
        .or_else(|| value.get("input"))
        .and_then(Value::as_str)
        .unwrap_or_default()
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
        .map_or(event_id, |info| info.call_id.as_str())
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
    let arguments = tool_arguments_from_value(event);
    Some(openai_tool_call(&call_id, name, arguments))
}

fn tool_call_output_item(
    item: Option<&Value>,
    function_call_items: &BTreeMap<String, FunctionCallInfo>,
) -> Option<(String, String, String, String)> {
    let item = item?;
    let (item_id, call_id, name) = function_call_item(Some(item))?;
    let info = function_call_items.get(&item_id);
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .or_else(|| info.map(|info| info.call_id.as_str()))
        .unwrap_or(&call_id)
        .to_string();
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| info.map(|info| info.name.as_str()))
        .unwrap_or(&name)
        .to_string();
    let arguments = tool_arguments_from_value(item).to_string();
    Some((item_id, call_id, name, arguments))
}

fn tool_call_from_output_item(
    item: Option<&Value>,
    function_call_items: &BTreeMap<String, FunctionCallInfo>,
    finished_call_ids: &mut BTreeSet<String>,
) -> Option<Value> {
    let (_, call_id, name, arguments) = tool_call_output_item(item, function_call_items)?;
    if !finished_call_ids.insert(call_id.clone()) {
        return None;
    }
    Some(openai_tool_call(&call_id, &name, &arguments))
}

fn openai_tool_call(call_id: &str, name: &str, arguments: &str) -> Value {
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

fn chat_completion_stream_id() -> String {
    let suffix = Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(24)
        .collect::<String>();
    format!("chatcmpl-{suffix}")
}

fn chat_stream_translation_openai_error(
    error: &ChatStreamTranslationError,
) -> (String, String, String) {
    match error {
        ChatStreamTranslationError::InvalidSse(_) => (
            "server_error".to_string(),
            "invalid_upstream_response".to_string(),
            "Invalid upstream Codex response".to_string(),
        ),
        ChatStreamTranslationError::Upstream {
            message,
            error_type,
            code,
        } => (error_type.clone(), code.clone(), message.clone()),
    }
}

fn codex_stream_error_details(value: &Value) -> (String, String, String) {
    let message = codex_stream_error_string(value, "message")
        .unwrap_or_else(|| "Upstream Codex response failed".to_string());
    let raw_code =
        codex_stream_error_string(value, "code").unwrap_or_else(|| "upstream_error".to_string());
    let code = openai_error_code_for_code(&raw_code);
    let error_type =
        codex_stream_error_type(value).unwrap_or_else(|| openai_error_type_for_code(&raw_code));
    (message, error_type, code)
}

fn codex_stream_error_type(value: &Value) -> Option<String> {
    codex_stream_nested_error_string(value, "type").or_else(|| {
        codex_stream_top_level_string(value, "type")
            .filter(|kind| !matches!(kind.as_str(), "error" | "response.failed"))
    })
}

fn codex_stream_error_string(value: &Value, field: &str) -> Option<String> {
    codex_stream_nested_error_string(value, field)
        .or_else(|| codex_stream_top_level_string(value, field))
}

fn codex_stream_nested_error_string(value: &Value, field: &str) -> Option<String> {
    value
        .pointer(&format!("/response/error/{field}"))
        .or_else(|| value.pointer(&format!("/error/{field}")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn codex_stream_top_level_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn openai_error_code_for_code(code: &str) -> String {
    let lower = code.to_ascii_lowercase();
    if lower.contains("quota") || lower.contains("payment") || lower == "insufficient_quota" {
        "insufficient_quota"
    } else if lower.contains("rate_limit") || lower.contains("usage_limit") {
        "rate_limit_exceeded"
    } else if lower.contains("auth")
        || lower.contains("token")
        || lower.contains("unauthorized")
        || lower.contains("invalid_api_key")
        || lower.contains("account_deactivated")
    {
        "invalid_api_key"
    } else if lower.contains("model_not_supported")
        || lower.contains("model_not_available")
        || lower.contains("model_unsupported")
    {
        "model_not_found"
    } else if lower.trim().is_empty() {
        "upstream_error"
    } else {
        code
    }
    .to_string()
}

fn openai_error_type_for_code(code: &str) -> String {
    let code = code.to_ascii_lowercase();
    if code.contains("quota") || code.contains("payment") || code == "insufficient_quota" {
        "insufficient_quota"
    } else if code.contains("rate_limit") || code.contains("usage_limit") {
        "rate_limit_error"
    } else if code.contains("auth")
        || code.contains("token")
        || code.contains("unauthorized")
        || code.contains("invalid_api_key")
        || code.contains("account_deactivated")
        || code.contains("model")
        || code.contains("invalid_request")
        || code.contains("not_found")
        || code.contains("context_window")
        || code.contains("invalid_prompt")
        || code.contains("cyber_policy")
        || code.contains("bad_request")
    {
        "invalid_request_error"
    } else {
        "server_error"
    }
    .to_string()
}
