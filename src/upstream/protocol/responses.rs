use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::upstream::{
    models::{ModelConfig, ParsedModelName},
    protocol::{
        schema::reconvert_tuple_values,
        sse::{parse_sse_events, SseError},
    },
};

/// Codex Responses 上游请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexResponsesRequest {
    /// 模型名。
    pub model: String,
    /// 指令文本。
    pub instructions: String,
    /// 输入消息与结构化条目。
    pub input: Vec<Value>,
    /// 是否流式返回。
    pub stream: bool,
    /// 是否要求上游存储响应。
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// reasoning 配置。
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 工具定义。
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 工具选择策略。
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 是否允许并行工具调用。
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 输出文本格式配置。
    pub text: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 图片生成开关。
    pub generate: Option<bool>,
    #[serde(skip)]
    /// tuple schema 原始定义，仅供响应重构时使用。
    pub tuple_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// service tier。
    pub service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 用于显式续链的前一个 response ID。
    pub previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 提示缓存键。
    pub prompt_cache_key: Option<String>,
    #[serde(skip)]
    /// 是否由客户端显式提供了 prompt cache key。
    pub explicit_prompt_cache_key: bool,
    #[serde(skip)]
    /// 客户端会话 ID。
    pub client_conversation_id: Option<String>,
    #[serde(skip)]
    /// 变体身份键。
    pub variant_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// include 列表。
    pub include: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 传给上游的 client metadata。
    pub client_metadata: Option<Value>,
    #[serde(skip)]
    /// 是否偏好 WebSocket 传输。
    pub use_websocket: bool,
    #[serde(skip)]
    /// 是否强制 HTTP SSE。
    pub force_http_sse: bool,
    #[serde(skip)]
    /// turn state 透传头。
    pub turn_state: Option<String>,
    #[serde(skip)]
    /// turn metadata 透传头。
    pub turn_metadata: Option<String>,
    #[serde(skip)]
    /// beta features 透传头。
    pub beta_features: Option<String>,
    #[serde(skip)]
    /// 客户端版本头。
    pub version: Option<String>,
    #[serde(skip)]
    /// timing metrics 透传头。
    pub include_timing_metrics: Option<String>,
    #[serde(skip)]
    /// codex window id。
    pub codex_window_id: Option<String>,
    #[serde(skip)]
    /// 父线程 ID。
    pub parent_thread_id: Option<String>,
}

/// Codex Responses 请求的上游传输决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexTransport {
    /// HTTP SSE。
    HttpSse,
    /// 优先 WebSocket，失败后可按条件回退 HTTP SSE。
    WebSocketPreferred,
    /// 必须使用 WebSocket。
    WebSocketRequired,
}

/// 判断 Responses 请求应使用哪种上游传输。
pub fn transport_for_request(request: &CodexResponsesRequest) -> CodexTransport {
    if request.force_http_sse {
        return CodexTransport::HttpSse;
    }

    if request.previous_response_id.is_some() {
        return CodexTransport::WebSocketRequired;
    }

    if request.use_websocket {
        return CodexTransport::WebSocketPreferred;
    }

    CodexTransport::HttpSse
}

/// 判断请求在 WebSocket 失败后是否允许 HTTP SSE 回退。
pub fn http_sse_fallback_allowed(request: &CodexResponsesRequest) -> bool {
    !matches!(
        transport_for_request(request),
        CodexTransport::WebSocketRequired
    )
}

/// 从 Codex SSE 收集出的非流式 Responses 结果。
#[derive(Debug, Clone, PartialEq)]
pub enum CollectedResponse {
    /// 收集到 `response.completed`。
    Completed(Value),
    /// 收集到 `response.failed` 或 `error`。
    Failed(ResponsesSseFailure),
    /// SSE 未包含 `response.completed`。
    MissingCompleted,
    /// 完成响应为空。
    Empty,
}

/// Codex Responses SSE 失败事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsesSseFailure {
    /// SSE event 名称。
    pub event: String,
    /// 上游错误消息。
    pub message: String,
    /// 上游错误码。
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
    /// 上游 response id。
    pub response_id: String,
    /// 完成响应中的 function call ids。
    pub function_call_ids: Vec<String>,
    /// 可用于 reasoning replay 的 output items。
    pub replay_items: Vec<Value>,
}

/// 将 Codex Responses SSE 完成响应收集为非流式 Responses JSON。
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

/// 将模型名后缀和模型配置应用到 Codex Responses 上游请求。
pub fn apply_response_model_options(
    request: &mut CodexResponsesRequest,
    parsed_model: &ParsedModelName,
    config: &ModelConfig,
) {
    request.model = parsed_model.model_id.clone();
    apply_reasoning_options(request, parsed_model, config);
    apply_service_tier_options(request, parsed_model, config);
}

fn apply_reasoning_options(
    request: &mut CodexResponsesRequest,
    parsed_model: &ParsedModelName,
    config: &ModelConfig,
) {
    let existing_reasoning = request.reasoning.take();
    let existing_object = match existing_reasoning {
        Some(Value::Object(object)) => object,
        Some(_) | None => Map::new(),
    };
    let effort = existing_object
        .get("effort")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| non_empty_string(parsed_model.reasoning_effort.as_deref()))
        .or_else(|| non_empty_string(config.default_reasoning_effort.as_deref()));
    if effort.is_none() && existing_object.is_empty() {
        request.reasoning = None;
        return;
    }

    let summary = existing_object
        .get("summary")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("auto");
    let mut reasoning = Map::new();
    reasoning.insert("summary".to_string(), Value::String(summary.to_string()));
    if let Some(effort) = effort {
        reasoning.insert("effort".to_string(), Value::String(effort));
    }
    request.reasoning = Some(Value::Object(reasoning));
    ensure_reasoning_include(request);
}

pub(crate) fn ensure_reasoning_include(request: &mut CodexResponsesRequest) {
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

fn apply_service_tier_options(
    request: &mut CodexResponsesRequest,
    parsed_model: &ParsedModelName,
    config: &ModelConfig,
) {
    request.service_tier = request
        .service_tier
        .take()
        .and_then(|value| non_empty_string(Some(&value)))
        .or_else(|| non_empty_string(parsed_model.service_tier.as_deref()))
        .or_else(|| non_empty_string(config.service_tier.as_deref()))
        .map(normalize_service_tier_for_upstream);
}

fn normalize_service_tier_for_upstream(service_tier: String) -> String {
    if service_tier == "fast" {
        "priority".to_string()
    } else {
        service_tier
    }
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

impl CodexResponsesRequest {
    /// 构造默认的 HTTP SSE 请求。
    pub fn new_http_sse(
        model: impl Into<String>,
        instructions: impl Into<String>,
        input: Vec<Value>,
    ) -> Self {
        Self {
            model: model.into(),
            instructions: instructions.into(),
            input,
            stream: true,
            store: false,
            reasoning: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            text: None,
            generate: None,
            tuple_schema: None,
            service_tier: None,
            previous_response_id: None,
            prompt_cache_key: None,
            explicit_prompt_cache_key: false,
            client_conversation_id: None,
            variant_identity: None,
            include: None,
            client_metadata: None,
            use_websocket: false,
            force_http_sse: false,
            turn_state: None,
            turn_metadata: None,
            beta_features: None,
            version: None,
            include_timing_metrics: None,
            codex_window_id: None,
            parent_thread_id: None,
        }
    }

    /// 判断请求是否声明了图片生成工具。
    pub fn expects_image_generation(&self) -> bool {
        self.tools.as_deref().is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool.get("type").and_then(Value::as_str) == Some("image_generation"))
        })
    }
}

/// Codex compact 端点请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexCompactRequest {
    /// 模型名。
    pub model: String,
    /// 输入消息与结构化条目。
    pub input: Vec<Value>,
    /// 指令文本。
    pub instructions: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 工具定义。
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 是否允许并行工具调用。
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// reasoning 配置。
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// 输出文本格式配置。
    pub text: Option<Value>,
}
