//! OpenAI Responses API 类型。

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::protocol::codex::{
    responses::{CodexCompactRequest, CodexResponsesRequest},
    schema::{prepare_schema, reconvert_tuple_values},
    sse::{parse_sse_events, SseError},
};

/// OpenAI 请求声明的响应格式类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFormat {
    /// 普通文本输出。
    Text,
    /// JSON object 输出。
    JsonObject,
    /// JSON schema 输出。
    JsonSchema,
}

/// Responses API 请求体。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpenAiResponsesRequest {
    /// 模型名。
    pub model: String,
    /// 输入内容。
    #[serde(default = "default_responses_input")]
    pub input: Value,
    /// 指令文本。
    #[serde(default)]
    pub instructions: Option<String>,
    /// reasoning 配置。
    #[serde(default)]
    pub reasoning: Option<Value>,
    /// 工具定义。
    #[serde(default)]
    pub tools: Option<Value>,
    /// service tier。
    #[serde(default)]
    pub service_tier: Option<String>,
    /// 工具选择策略。
    #[serde(default)]
    pub tool_choice: Option<Value>,
    /// 是否允许并行工具调用。
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    /// 输出文本格式配置。
    #[serde(default)]
    pub text: Option<Value>,
    /// 图片生成开关。
    #[serde(default)]
    pub generate: Option<bool>,
    /// 提示缓存键。
    #[serde(default)]
    pub prompt_cache_key: Option<String>,
    /// include 列表。
    #[serde(default)]
    pub include: Option<Value>,
    /// 传给上游的 client metadata。
    #[serde(default)]
    pub client_metadata: Option<Value>,
    /// 前一个响应 ID。
    #[serde(default)]
    pub previous_response_id: Option<String>,
    /// Codex turn state 透传头。
    #[serde(default, rename = "turnState", alias = "turn_state")]
    pub turn_state: Option<String>,
    /// Codex turn metadata 透传头。
    #[serde(default, rename = "turnMetadata", alias = "turn_metadata")]
    pub turn_metadata: Option<String>,
    /// beta features 透传头。
    #[serde(default, rename = "betaFeatures", alias = "beta_features")]
    pub beta_features: Option<String>,
    /// 客户端版本头。
    #[serde(default)]
    pub version: Option<String>,
    /// timing metrics 透传头。
    #[serde(
        default,
        rename = "includeTimingMetrics",
        alias = "include_timing_metrics"
    )]
    pub include_timing_metrics: Option<String>,
    /// codex window id。
    #[serde(default, rename = "codexWindowId", alias = "codex_window_id")]
    pub codex_window_id: Option<String>,
    /// 父线程 ID。
    #[serde(default, rename = "parentThreadId", alias = "parent_thread_id")]
    pub parent_thread_id: Option<String>,
    /// 是否偏好 WebSocket 传输。
    #[serde(default)]
    pub use_websocket: Option<bool>,
    /// 是否流式返回。
    #[serde(default = "default_responses_stream")]
    pub stream: bool,
}

fn default_responses_input() -> Value {
    Value::Array(Vec::new())
}

fn default_responses_stream() -> bool {
    true
}

/// 将 OpenAI Responses 请求转换为 Codex Responses 请求。
pub fn translate_response_to_codex(request: OpenAiResponsesRequest) -> CodexResponsesRequest {
    let prepared_text = prepare_text_format(request.text, true);
    let mut codex_request = CodexResponsesRequest::new_http_sse(
        request.model,
        request.instructions.unwrap_or_default(),
        sanitize_responses_input(request.input),
    );
    codex_request.previous_response_id = request.previous_response_id;
    codex_request.turn_state = request.turn_state;
    codex_request.turn_metadata = non_empty_string(request.turn_metadata);
    codex_request.beta_features = non_empty_string(request.beta_features);
    codex_request.include_timing_metrics = non_empty_string(request.include_timing_metrics);
    codex_request.version = non_empty_string(request.version);
    codex_request.codex_window_id = non_empty_string(request.codex_window_id);
    codex_request.parent_thread_id = non_empty_string(request.parent_thread_id);
    codex_request.reasoning = responses_reasoning(request.reasoning);
    codex_request.tools = non_empty_array(request.tools);
    codex_request.tool_choice = request.tool_choice;
    codex_request.parallel_tool_calls = request.parallel_tool_calls;
    codex_request.text = prepared_text.text;
    codex_request.tuple_schema = prepared_text.tuple_schema;
    codex_request.generate = request.generate;
    codex_request.service_tier = non_empty_string(request.service_tier);
    codex_request.prompt_cache_key = non_empty_string(request.prompt_cache_key);
    codex_request.explicit_prompt_cache_key = codex_request.prompt_cache_key.is_some();
    codex_request.include = string_array(request.include);
    codex_request.client_metadata = sanitize_client_metadata(request.client_metadata);
    ensure_reasoning_include(&mut codex_request);
    match request.use_websocket {
        Some(true) => codex_request.use_websocket = true,
        Some(false) => codex_request.force_http_sse = true,
        None => {}
    }
    codex_request
}

/// 将 OpenAI Responses 请求转换为 Codex compact 请求。
pub fn translate_response_to_compact(request: OpenAiResponsesRequest) -> CodexCompactRequest {
    CodexCompactRequest {
        model: request.model,
        input: sanitize_responses_input(request.input),
        instructions: request.instructions.unwrap_or_default(),
        tools: non_empty_array(request.tools),
        parallel_tool_calls: request.parallel_tool_calls,
        reasoning: compact_reasoning(request.reasoning),
        text: prepare_text_format(request.text, false).text,
    }
}

struct PreparedTextFormat {
    text: Option<Value>,
    tuple_schema: Option<Value>,
}

fn prepare_text_format(text: Option<Value>, prepare_tuple_schema: bool) -> PreparedTextFormat {
    let Some(Value::Object(text)) = text else {
        return PreparedTextFormat {
            text: None,
            tuple_schema: None,
        };
    };
    let Some(Value::Object(format)) = text.get("format") else {
        return PreparedTextFormat {
            text: None,
            tuple_schema: None,
        };
    };
    let Some(format_type) = format.get("type").and_then(Value::as_str) else {
        return PreparedTextFormat {
            text: None,
            tuple_schema: None,
        };
    };

    let mut sanitized_format = Map::new();
    sanitized_format.insert("type".to_string(), Value::String(format_type.to_string()));
    if let Some(name) = format.get("name").and_then(Value::as_str) {
        sanitized_format.insert("name".to_string(), Value::String(name.to_string()));
    }

    let mut tuple_schema = None;
    if let Some(Value::Object(schema)) = format.get("schema") {
        let schema = Value::Object(schema.clone());
        let schema = if prepare_tuple_schema {
            let prepared = prepare_schema(schema);
            tuple_schema = prepared.original_schema;
            prepared.schema
        } else {
            schema
        };
        sanitized_format.insert("schema".to_string(), schema);
    }
    if let Some(strict) = format.get("strict").and_then(Value::as_bool) {
        sanitized_format.insert("strict".to_string(), Value::Bool(strict));
    }

    PreparedTextFormat {
        text: Some(json!({"format": sanitized_format})),
        tuple_schema,
    }
}

fn sanitize_responses_input(input: Value) -> Vec<Value> {
    match input {
        Value::Array(items) => sanitize_codex_input_items(items),
        Value::Null => Vec::new(),
        value => vec![value],
    }
}

fn sanitize_codex_input_items(input: Vec<Value>) -> Vec<Value> {
    input
        .into_iter()
        .filter_map(|item| {
            let Value::Object(object) = item else {
                return Some(item);
            };
            match object.get("type").and_then(Value::as_str) {
                Some("reasoning") => sanitize_reasoning_item(&object),
                Some("compaction") => sanitize_compaction_item(&object),
                _ => Some(Value::Object(object)),
            }
        })
        .collect()
}

fn sanitize_reasoning_item(item: &Map<String, Value>) -> Option<Value> {
    let id = non_empty_str(item.get("id"))?;
    let summary = sanitize_summary(item.get("summary"))?;
    let mut sanitized = Map::new();
    sanitized.insert("type".to_string(), Value::String("reasoning".to_string()));
    sanitized.insert("id".to_string(), Value::String(id.to_string()));
    sanitized.insert("summary".to_string(), Value::Array(summary));
    if let Some(status) = item
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| matches!(*status, "in_progress" | "completed" | "incomplete"))
    {
        sanitized.insert("status".to_string(), Value::String(status.to_string()));
    }
    if let Some(encrypted_content) = non_empty_str(item.get("encrypted_content")) {
        sanitized.insert(
            "encrypted_content".to_string(),
            Value::String(encrypted_content.to_string()),
        );
    }
    if let Some(content) = sanitize_reasoning_content(item.get("content")) {
        sanitized.insert("content".to_string(), Value::Array(content));
    }
    Some(Value::Object(sanitized))
}

fn sanitize_summary(value: Option<&Value>) -> Option<Vec<Value>> {
    let Value::Array(parts) = value? else {
        return None;
    };
    Some(
        parts
            .iter()
            .filter_map(|part| {
                let Value::Object(part) = part else {
                    return None;
                };
                if part.get("type").and_then(Value::as_str) != Some("summary_text") {
                    return None;
                }
                let text = part.get("text").and_then(Value::as_str)?;
                Some(json!({"type": "summary_text", "text": text}))
            })
            .collect(),
    )
}

fn sanitize_reasoning_content(value: Option<&Value>) -> Option<Vec<Value>> {
    let Value::Array(parts) = value? else {
        return None;
    };
    let content = parts
        .iter()
        .filter_map(|part| {
            let Value::Object(part) = part else {
                return None;
            };
            if part.get("type").and_then(Value::as_str) != Some("reasoning_text") {
                return None;
            }
            let text = part.get("text").and_then(Value::as_str)?;
            Some(json!({"type": "reasoning_text", "text": text}))
        })
        .collect::<Vec<_>>();
    (!content.is_empty()).then_some(content)
}

fn sanitize_compaction_item(item: &Map<String, Value>) -> Option<Value> {
    let encrypted_content = non_empty_str(item.get("encrypted_content"))?;
    let mut sanitized = Map::new();
    sanitized.insert("type".to_string(), Value::String("compaction".to_string()));
    sanitized.insert(
        "encrypted_content".to_string(),
        Value::String(encrypted_content.to_string()),
    );
    if let Some(id) = non_empty_str(item.get("id")) {
        sanitized.insert("id".to_string(), Value::String(id.to_string()));
    }
    Some(Value::Object(sanitized))
}

fn responses_reasoning(reasoning: Option<Value>) -> Option<Value> {
    let Value::Object(input) = reasoning? else {
        return None;
    };
    let effort = input.get("effort").and_then(Value::as_str);
    let summary = input
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let mut output = Map::new();
    output.insert("summary".to_string(), Value::String(summary.to_string()));
    if let Some(effort) = effort {
        output.insert("effort".to_string(), Value::String(effort.to_string()));
    }
    Some(Value::Object(output))
}

fn compact_reasoning(reasoning: Option<Value>) -> Option<Value> {
    let Value::Object(input) = reasoning? else {
        return None;
    };
    let mut output = Map::new();
    if let Some(effort) = input.get("effort").and_then(Value::as_str) {
        output.insert("effort".to_string(), Value::String(effort.to_string()));
    }
    if let Some(summary) = input.get("summary").and_then(Value::as_str) {
        output.insert("summary".to_string(), Value::String(summary.to_string()));
    }
    (!output.is_empty()).then_some(Value::Object(output))
}

fn ensure_reasoning_include(request: &mut CodexResponsesRequest) {
    if request.reasoning.is_none() {
        return;
    }
    if request
        .include
        .as_ref()
        .is_some_and(|include| !include.is_empty())
    {
        return;
    }
    request.include = Some(vec!["reasoning.encrypted_content".to_string()]);
}

fn sanitize_client_metadata(client_metadata: Option<Value>) -> Option<Value> {
    let Value::Object(input) = client_metadata? else {
        return None;
    };
    let metadata = input
        .into_iter()
        .filter_map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key, Value::String(value.to_string())))
        })
        .collect::<Map<_, _>>();
    (!metadata.is_empty()).then_some(Value::Object(metadata))
}

fn non_empty_array(value: Option<Value>) -> Option<Vec<Value>> {
    let Value::Array(values) = value? else {
        return None;
    };
    (!values.is_empty()).then_some(values)
}

fn string_array(value: Option<Value>) -> Option<Vec<String>> {
    let Value::Array(values) = value? else {
        return None;
    };
    values
        .into_iter()
        .map(|value| match value {
            Value::String(value) => Some(value),
            _ => None,
        })
        .collect()
}

fn non_empty_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    let value = value?.as_str()?;
    (!value.trim().is_empty()).then_some(value)
}

/// 从 Codex SSE 收集出的 Responses API 非流式结果。
#[derive(Debug, Clone, PartialEq)]
pub enum CollectedResponse {
    /// 上游返回了完成响应。
    Completed(Value),
    /// 上游返回了失败事件。
    Failed(ResponsesSseFailure),
    /// 上游没有返回 `response.completed`。
    MissingCompleted,
    /// 上游完成响应没有可见输出。
    Empty,
}

/// Responses SSE 失败事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsesSseFailure {
    /// SSE 事件名。
    pub event: String,
    /// 错误消息。
    pub message: String,
    /// 上游错误代码。
    pub upstream_code: Option<String>,
}

impl ResponsesSseFailure {
    fn from_event(event: &str, value: &Value) -> Self {
        Self {
            event: event.to_string(),
            message: failure_message(value).unwrap_or_else(|| "Codex upstream SSE failed".into()),
            upstream_code: failure_code(value),
        }
    }
}

/// 从完成 SSE 中提取会话亲和性和 replay 元数据。
#[derive(Debug, Clone, PartialEq)]
pub struct CompletedResponseMetadata {
    /// 响应 ID。
    pub response_id: String,
    /// 完成响应中包含的函数调用 ID。
    pub function_call_ids: Vec<String>,
    /// 可用于 reasoning replay 的响应条目。
    pub replay_items: Vec<Value>,
}

/// 将 Codex Responses SSE 完成响应收集为非流式 Responses API JSON。
///
/// # Errors
///
/// 当输入不是合法 SSE 流时，返回 [`SseError`]。
pub fn response_from_codex_sse(
    body: &str,
    tuple_schema: Option<&Value>,
) -> Result<CollectedResponse, SseError> {
    let events = parse_sse_events(body)?;
    let mut output_text = String::new();
    let mut output_items = Vec::new();
    let mut completed_response = None;
    let mut failed_response = None;

    for event in events {
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        match event.event.as_deref() {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    output_text.push_str(delta);
                }
            }
            Some("response.output_item.done") => {
                if let Some(item) = value.get("item") {
                    output_items.push(item.clone());
                }
            }
            Some("response.completed") => {
                completed_response = value.get("response").cloned();
            }
            Some(event_name @ ("error" | "response.failed")) if failed_response.is_none() => {
                failed_response = Some(ResponsesSseFailure::from_event(event_name, &value));
            }
            _ => {}
        }
    }

    if let Some(failure) = failed_response {
        return Ok(CollectedResponse::Failed(failure));
    }
    let Some(mut response) = completed_response else {
        return Ok(CollectedResponse::MissingCompleted);
    };
    if is_empty_response(&response, &output_text, &output_items) {
        return Ok(CollectedResponse::Empty);
    }

    ensure_completed_response_output(&mut response, &output_items, &output_text);
    reconvert_completed_response_tuple_values(&mut response, tuple_schema);
    sync_output_text_from_output(&mut response);
    Ok(CollectedResponse::Completed(response))
}

/// 从 Codex Responses SSE 中提取完成响应元数据。
///
/// # Errors
///
/// 当输入不是合法 SSE 流时，返回 [`SseError`]。
pub fn completed_response_metadata(
    body: &str,
) -> Result<Option<CompletedResponseMetadata>, SseError> {
    let events = parse_sse_events(body)?;
    let mut response_id = None;
    let mut function_call_ids = BTreeSet::new();
    let mut replay_items = Vec::new();

    for event in events {
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        match event.event.as_deref() {
            Some("response.output_item.done") => {
                if let Some(item) = value.get("item") {
                    collect_response_replay_items(item, &mut replay_items);
                }
                if let Some(call_id) = value.pointer("/item/call_id").and_then(Value::as_str) {
                    function_call_ids.insert(call_id.to_string());
                }
            }
            Some("response.completed") => {
                response_id = value
                    .pointer("/response/id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                collect_response_function_call_ids(&value, &mut function_call_ids);
                if let Some(output) = value.pointer("/response/output").and_then(Value::as_array) {
                    for item in output {
                        collect_response_replay_items(item, &mut replay_items);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(response_id.map(|response_id| CompletedResponseMetadata {
        response_id,
        function_call_ids: function_call_ids.into_iter().collect(),
        replay_items,
    }))
}

/// 对单个 Responses SSE 事件的数据执行 tuple schema 回转换。
pub fn reconvert_responses_sse_event_tuple_values(
    event_name: Option<&str>,
    mut data: Value,
    tuple_schema: &Value,
) -> Value {
    match responses_event_type(event_name, &data) {
        Some("response.output_text.delta") => {
            reconvert_output_text_delta_tuple_values(&mut data, tuple_schema);
        }
        Some("response.output_item.done") => {
            if let Some(item) = data.get_mut("item") {
                reconvert_output_item_tuple_values(item, tuple_schema);
            }
        }
        Some("response.completed") => {
            if let Some(response) = data.get_mut("response") {
                reconvert_completed_response_tuple_values(response, Some(tuple_schema));
                sync_output_text_from_output(response);
            }
        }
        _ => {}
    }
    data
}

fn responses_event_type<'a>(event_name: Option<&'a str>, data: &'a Value) -> Option<&'a str> {
    event_name.or_else(|| data.get("type").and_then(Value::as_str))
}

fn reconvert_output_text_delta_tuple_values(data: &mut Value, tuple_schema: &Value) {
    let Some(delta) = data.get("delta").and_then(Value::as_str) else {
        return;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(delta) else {
        return;
    };
    let reconverted = reconvert_tuple_values(parsed, tuple_schema);
    data["delta"] = Value::String(reconverted.to_string());
}

fn is_empty_response(response: &Value, output_text: &str, output_items: &[Value]) -> bool {
    if !output_text.trim().is_empty() || !output_items.is_empty() {
        return false;
    }

    response
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or_default()
        == 0
}

fn ensure_completed_response_output(
    response: &mut Value,
    output_items: &[Value],
    output_text: &str,
) {
    let output_is_empty = response
        .get("output")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty);
    if !output_is_empty {
        return;
    }

    if !output_items.is_empty() {
        response["output"] = Value::Array(output_items.to_vec());
        return;
    }
    if output_text.is_empty() {
        return;
    }

    response["output"] = json!([{
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{
            "type": "output_text",
            "text": output_text,
            "annotations": []
        }]
    }]);
}

fn reconvert_completed_response_tuple_values(response: &mut Value, tuple_schema: Option<&Value>) {
    let Some(tuple_schema) = tuple_schema else {
        return;
    };
    let Some(items) = response.get_mut("output").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        reconvert_output_item_tuple_values(item, tuple_schema);
    }
}

fn reconvert_output_item_tuple_values(item: &mut Value, tuple_schema: &Value) {
    let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    for part in content {
        let part_type = part.get("type").and_then(Value::as_str);
        if !matches!(part_type, Some("output_text" | "text")) {
            continue;
        }
        let Some(text) = part.get("text").and_then(Value::as_str) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        let reconverted = reconvert_tuple_values(parsed, tuple_schema);
        part["text"] = Value::String(reconverted.to_string());
    }
}

fn sync_output_text_from_output(response: &mut Value) {
    let Some(items) = response.get("output").and_then(Value::as_array) else {
        return;
    };
    let texts = items
        .iter()
        .filter_map(output_text_from_item)
        .collect::<Vec<_>>();
    if texts.is_empty() {
        return;
    }
    response["output_text"] = Value::String(texts.join("\n\n"));
}

fn collect_response_function_call_ids(value: &Value, function_call_ids: &mut BTreeSet<String>) {
    let Some(output) = value.pointer("/response/output").and_then(Value::as_array) else {
        return;
    };
    for item in output {
        if let Some(call_id) = item.get("call_id").and_then(Value::as_str) {
            function_call_ids.insert(call_id.to_string());
        }
    }
}

fn collect_response_replay_items(item: &Value, replay_items: &mut Vec<Value>) {
    if matches!(
        item.get("type").and_then(Value::as_str),
        Some("reasoning" | "function_call")
    ) {
        replay_items.push(item.clone());
    }
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

fn failure_message(value: &Value) -> Option<String> {
    value
        .pointer("/response/error/message")
        .or_else(|| value.pointer("/error/message"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .map(ToString::to_string)
}

fn failure_code(value: &Value) -> Option<String> {
    value
        .pointer("/response/error/code")
        .or_else(|| value.pointer("/error/code"))
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .filter(|code| !code.trim().is_empty())
        .map(ToString::to_string)
}
