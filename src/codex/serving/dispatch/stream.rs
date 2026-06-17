use std::collections::BTreeSet;

use axum::http::StatusCode;
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::codex::gateway::{
    protocol::tuple_schema::reconvert_tuple_values,
    transport::{
        http_client::CodexClientError,
        sse::{encode_sse_event, parse_sse_events},
    },
};

pub(crate) enum CollectedResponse {
    Completed(Value),
    Failed(ResponsesSseFailure),
    MissingCompleted,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompletedResponseMetadata {
    pub response_id: String,
    pub function_call_ids: Vec<String>,
    pub replay_items: Vec<Value>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResponsesSseFailure {
    event: String,
    message: String,
    upstream_code: Option<String>,
    invalid_reasoning_replay: bool,
}

impl ResponsesSseFailure {
    fn from_event(event: &str, value: &Value) -> Self {
        Self {
            event: event.to_string(),
            message: failure_message(value).unwrap_or_else(|| "Codex upstream SSE failed".into()),
            upstream_code: failure_code(value),
            invalid_reasoning_replay: contains_invalid_encrypted_content_signal(value),
        }
    }

    pub(crate) fn invalid_reasoning_replay(&self) -> bool {
        self.invalid_reasoning_replay
    }

    pub(crate) fn upstream_error(&self) -> CodexClientError {
        let code = self.upstream_code.as_deref().unwrap_or("error");
        CodexClientError::Upstream {
            status: status_for_failure_code(code),
            retry_after_seconds: None,
            body: json!({
                "error": {
                    "type": code,
                    "code": code,
                    "message": self.message,
                }
            })
            .to_string(),
        }
    }

    pub(crate) fn metadata(&self, stream: bool) -> Value {
        let mut metadata = json!({"stream": stream});
        self.extend_metadata(&mut metadata);
        metadata
    }

    pub(super) fn extend_metadata(&self, metadata: &mut Value) {
        let Some(object) = metadata.as_object_mut() else {
            *metadata = self.metadata(true);
            return;
        };
        object.insert("failureEvent".to_string(), json!(self.event));
        object.insert("failureMessage".to_string(), json!(self.message));
        if let Some(code) = &self.upstream_code {
            object.insert("upstreamCode".to_string(), json!(code));
        }
    }
}

fn status_for_failure_code(code: &str) -> StatusCode {
    let lower = code.to_ascii_lowercase();
    if lower.contains("model")
        && (lower.contains("not_supported") || lower.contains("not_available"))
    {
        return StatusCode::BAD_REQUEST;
    }
    if lower.contains("invalid_request") || lower.contains("not_found") {
        return StatusCode::BAD_REQUEST;
    }
    if lower.contains("rate_limit") || lower.contains("usage_limit") {
        return StatusCode::TOO_MANY_REQUESTS;
    }
    if lower.contains("unauthorized")
        || lower.contains("invalid_api_key")
        || lower == "token_invalid"
        || lower == "token_expired"
        || lower == "account_deactivated"
    {
        return StatusCode::UNAUTHORIZED;
    }
    if lower.contains("forbidden") || lower.contains("banned") {
        return StatusCode::FORBIDDEN;
    }
    if lower.contains("payment") || lower.contains("quota") {
        return StatusCode::PAYMENT_REQUIRED;
    }
    StatusCode::BAD_GATEWAY
}

pub(crate) fn completed_response_json(
    body: &str,
    tuple_schema: Option<&Value>,
) -> Result<CollectedResponse, crate::codex::gateway::transport::sse::SseError> {
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
                if let Some(response) = value.get("response") {
                    completed_response = Some(response.clone());
                }
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

    // 检测空响应：没有输出文本且没有工具调用
    if is_empty_response(&response, &output_text, &output_items) {
        return Ok(CollectedResponse::Empty);
    }

    ensure_completed_response_output(&mut response, &output_items, &output_text);
    reconvert_completed_response_tuple_values(&mut response, tuple_schema);
    sync_output_text_from_output(&mut response);
    Ok(CollectedResponse::Completed(response))
}

/// 检测是否为空响应
fn is_empty_response(response: &Value, output_text: &str, output_items: &[Value]) -> bool {
    // 如果有输出文本，不是空响应
    if !output_text.trim().is_empty() {
        return false;
    }

    // 如果有工具调用，不是空响应
    if !output_items.is_empty() {
        return false;
    }

    // 检查 output_tokens 是否为 0
    let output_tokens = response
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    output_tokens == 0
}

pub(super) struct TupleStreamReconverter {
    tuple_schema: Option<Value>,
    pending: String,
    tuple_text_buffer: String,
}

impl TupleStreamReconverter {
    pub(super) fn new(tuple_schema: Option<Value>) -> Self {
        Self {
            tuple_schema,
            pending: String::new(),
            tuple_text_buffer: String::new(),
        }
    }

    pub(super) fn transform_chunk(&mut self, chunk: &str) -> String {
        if self.tuple_schema.is_none() {
            return chunk.to_string();
        }
        self.pending.push_str(chunk);
        let mut output = String::new();
        while let Some(delimiter_index) = self.pending.find("\n\n") {
            let block = self.pending[..delimiter_index].to_string();
            self.pending.drain(..delimiter_index + 2);
            output.push_str(&self.transform_block(&block));
        }
        output
    }

    pub(super) fn finish(&mut self) -> String {
        if self.tuple_schema.is_none() || self.pending.trim().is_empty() {
            self.pending.clear();
            return String::new();
        }
        let block = std::mem::take(&mut self.pending);
        self.transform_block(&block)
    }

    fn transform_block(&mut self, block: &str) -> String {
        if block.trim().is_empty() {
            return "\n\n".to_string();
        }
        let frame = format!("{block}\n\n");
        let Ok(mut events) = parse_sse_events(&frame) else {
            return frame;
        };
        if events.len() != 1 {
            return frame;
        }
        let event = events.remove(0);
        let Some(event_name) = event.event.as_deref() else {
            return frame;
        };
        match event_name {
            "response.output_text.delta" => self.transform_output_text_delta(&event.data, &frame),
            "response.completed" => {
                self.transform_response_completed(&event.data, event_name, &frame)
            }
            _ => frame,
        }
    }

    fn transform_output_text_delta(&mut self, data: &str, original_frame: &str) -> String {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return original_frame.to_string();
        };
        let Some(delta) = value.get("delta").and_then(Value::as_str) else {
            return original_frame.to_string();
        };
        self.tuple_text_buffer.push_str(delta);
        String::new()
    }

    fn transform_response_completed(
        &mut self,
        data: &str,
        event_name: &str,
        original_frame: &str,
    ) -> String {
        let Some(tuple_schema) = self.tuple_schema.as_ref() else {
            return original_frame.to_string();
        };
        let Ok(mut value) = serde_json::from_str::<Value>(data) else {
            return original_frame.to_string();
        };

        let mut output = String::new();
        if !self.tuple_text_buffer.is_empty() {
            let reconverted_text = reconvert_tuple_text(&self.tuple_text_buffer, tuple_schema)
                .unwrap_or_else(|| self.tuple_text_buffer.clone());
            self.tuple_text_buffer.clear();
            output.push_str(&encode_sse_event(
                "response.output_text.delta",
                &json!({
                    "type": "response.output_text.delta",
                    "delta": reconverted_text,
                })
                .to_string(),
            ));
        }

        if let Some(response) = value.get_mut("response") {
            reconvert_completed_response_tuple_values(response, Some(tuple_schema));
            sync_output_text_from_output(response);
        }
        output.push_str(&encode_sse_event(event_name, &value.to_string()));
        output
    }
}

pub(super) fn responses_sse_failure(
    body: &str,
) -> Result<Option<ResponsesSseFailure>, crate::codex::gateway::transport::sse::SseError> {
    for event in parse_sse_events(body)? {
        let Some(event_name @ ("error" | "response.failed")) = event.event.as_deref() else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        return Ok(Some(ResponsesSseFailure::from_event(event_name, &value)));
    }
    Ok(None)
}

pub(super) fn completed_response_metadata(
    body: &str,
) -> Result<Option<CompletedResponseMetadata>, crate::codex::gateway::transport::sse::SseError> {
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

pub(super) fn ensure_stream_metadata(metadata: &mut Value, stream_value: bool) {
    let Some(object) = metadata.as_object_mut() else {
        *metadata = json!({"stream": stream_value});
        return;
    };
    object
        .entry("stream".to_string())
        .or_insert_with(|| json!(stream_value));
}

pub(super) fn has_terminal_sse_event(
    body: &str,
) -> Result<bool, crate::codex::gateway::transport::sse::SseError> {
    parse_sse_events(body).map(|events| {
        events.iter().any(|event| {
            matches!(
                event.event.as_deref(),
                Some("response.completed" | "response.failed" | "error")
            )
        })
    })
}

pub(super) fn premature_close_failed_event(detail: Option<&str>) -> String {
    let message = match detail.filter(|value| !value.trim().is_empty()) {
        Some(detail) => format!("Upstream stream closed before response.completed: {detail}"),
        None => "Upstream stream closed before response.completed".to_string(),
    };
    response_failed_event(ResponsesStreamError {
        kind: "server_error",
        code: "stream_disconnected",
        message: &message,
    })
}

pub(super) fn responses_stream_error_event(status: StatusCode, message: &str) -> String {
    let clean_message = message
        .strip_prefix("Codex API error (")
        .and_then(|tail| tail.split_once("): "))
        .map_or(message, |(_, clean)| clean);
    let lower = clean_message.to_ascii_lowercase();
    let error = if status == StatusCode::TOO_MANY_REQUESTS {
        ResponsesStreamError {
            kind: "rate_limit_error",
            code: "rate_limit_exceeded",
            message: clean_message,
        }
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        ResponsesStreamError {
            kind: "invalid_request_error",
            code: "authentication_error",
            message: clean_message,
        }
    } else if lower.contains("error sending request") {
        ResponsesStreamError {
            kind: "server_error",
            code: "upstream_transport_error",
            message: clean_message,
        }
    } else {
        ResponsesStreamError {
            kind: if status.is_client_error() {
                "invalid_request_error"
            } else {
                "server_error"
            },
            code: "codex_api_error",
            message: clean_message,
        }
    };
    response_failed_event(error)
}

#[derive(Clone, Copy, Serialize)]
struct ResponsesStreamError<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    code: &'static str,
    message: &'a str,
}

#[derive(Serialize)]
struct ResponseFailedData<'a> {
    #[serde(rename = "type")]
    event_type: &'static str,
    response: ResponseFailedResponse<'a>,
    error: ResponsesStreamError<'a>,
}

#[derive(Serialize)]
struct ResponseFailedResponse<'a> {
    id: String,
    status: &'static str,
    error: ResponsesStreamError<'a>,
}

fn response_failed_event(error: ResponsesStreamError<'_>) -> String {
    let response_id_suffix: String = Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(24)
        .collect();
    let data = ResponseFailedData {
        event_type: "response.failed",
        response: ResponseFailedResponse {
            id: format!("resp_proxy_{response_id_suffix}"),
            status: "failed",
            error,
        },
        error,
    };
    encode_sse_event(
        "response.failed",
        &serde_json::to_string(&data).expect("response.failed event should serialize"),
    )
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

pub(crate) fn contains_invalid_encrypted_content_signal(value: &Value) -> bool {
    visit_invalid_encrypted_content(value, 0)
}

fn visit_invalid_encrypted_content(value: &Value, depth: usize) -> bool {
    if depth > 5 {
        return false;
    }
    match value {
        Value::String(value) => string_contains_invalid_encrypted_content(value),
        Value::Array(values) => values
            .iter()
            .any(|value| visit_invalid_encrypted_content(value, depth + 1)),
        Value::Object(object) => object.iter().any(|(key, value)| {
            string_contains_invalid_encrypted_content(key)
                || visit_invalid_encrypted_content(value, depth + 1)
        }),
        _ => false,
    }
}

fn string_contains_invalid_encrypted_content(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("invalid_encrypted_content")
        || (lower.contains("invalid") && lower.contains("encrypted") && lower.contains("content"))
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
        let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
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
}

fn reconvert_tuple_text(text: &str, tuple_schema: &Value) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(text).ok()?;
    Some(reconvert_tuple_values(parsed, tuple_schema).to_string())
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
