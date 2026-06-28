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
use serde_json::{json, Value};
use std::convert::Infallible;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    http::middleware::request_id::RequestId,
    proxy::{
        auth::authorize_client_api_key,
        dispatch::responses::{ResponseDispatchError, ResponseDispatchStream},
    },
    runtime::state::AppState,
    upstream::{
        models::ModelCatalog,
        protocol::{
            responses::{apply_response_model_options, CodexResponsesRequest},
            schema::{prepare_schema, reconvert_tuple_values},
            sse::{encode_sse_event, parse_sse_events, SseError, DONE_SSE_FRAME},
        },
    },
};

use super::{
    errors::{
        chat_dispatch_error_response, chat_stream_dispatch_error_message,
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
    #[error("{0}")]
    Upstream(String),
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
        tool_choice: request.tool_choice,
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
        force_http_sse: true,
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

// ====================================================================
// 非流式 Chat 响应：Codex SSE → OpenAI Chat JSON
// ====================================================================

/// 从 Codex Responses SSE 收集 Chat Completions 响应。
pub fn chat_completion_from_codex_sse(
    body: &str,
    model: &str,
    _include_reasoning: bool,
    tuple_schema: Option<&Value>,
) -> Result<Option<Value>, ChatStreamTranslationError> {
    let events = parse_sse_events(body)?;
    let mut output_text = String::new();
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
            failed_response = Some(codex_stream_error_message(&value));
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
            Some("response.completed") => {
                completed_response = value.get("response").cloned();
            }
            _ => {}
        }
    }

    if let Some(failure) = failed_response {
        return Err(ChatStreamTranslationError::Upstream(failure));
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
    if !tool_calls.is_empty() {
        choices.push(json!({
            "index": 0,
            "message": {
                "role": "assistant",
                "content": output_text_value,
                "tool_calls": tool_calls,
            },
            "finish_reason": "tool_calls",
        }));
    } else {
        choices.push(json!({
            "index": 0,
            "message": {
                "role": "assistant",
                "content": output_text_value,
            },
            "finish_reason": "stop",
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
    let body_stream =
        futures_stream::once(async move { Ok::<Bytes, Infallible>(Bytes::from(initial_frame)) })
            .chain(stream.body.map(move |result| {
                let body = match result {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        translator
                            .push_str(&text)
                            .unwrap_or_else(|error| chat_stream_error_sse_frame(&error.to_string()))
                    }
                    Err(error) => chat_stream_error_sse_frame(&error.to_string()),
                };
                Ok::<Bytes, Infallible>(Bytes::from(body))
            }));

    event_stream_response(
        Body::from_stream(body_stream),
        SseResponseOptions::LIVE_CHAT,
    )
}

fn response_dispatch_chat_stream_error_response(error: &ResponseDispatchError) -> Response {
    let message = chat_stream_dispatch_error_message(error);
    chat_stream_error_response(&message)
}

fn chat_stream_error_response(message: &str) -> Response {
    event_stream_response(
        Body::from(chat_stream_error_sse_frame(message)),
        SseResponseOptions::CHAT_ERROR,
    )
}

fn chat_stream_error_sse_frame(message: &str) -> String {
    openai_sse_frame(
        "",
        &json!({
            "error": {
                "message": message,
                "type": "stream_error",
            }
        })
        .to_string(),
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
    let arguments = event
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(openai_tool_call(&call_id, name, arguments))
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
    Some(openai_tool_call(call_id, name, arguments))
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
