use std::{collections::BTreeSet, time::Instant};

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::{
    infra::time::elapsed_millis_i64,
    upstream::protocol::{
        schema::reconvert_tuple_values,
        sse::{parse_sse_events, SseError, SseEvent},
    },
};

/// Codex Responses 上游请求体。
///
/// 发往上游的 Responses 请求。`body` 持有客户端原始 JSON object，逐字段（含顺序、
/// 含未知字段）透传上游，是上游请求体的唯一来源；`use_websocket`/`force_http_sse`
/// 仅用于本地传输选择，不写入 body。常用字段通过访问器方法读写。
/// 代理只做最小 patch（`stream`/`store` 由传输层控制，`input` 字符串做官方兼容转换，
/// implicit resume 时替换 `input`，模型后缀路由时写 `model`/`reasoning`/`service_tier`）。
///
/// 其余字段是代理控制状态，不进上游 body（原 `#[serde(skip)]` 字段）。
#[derive(Debug, Clone)]
pub struct CodexResponsesRequest {
    /// 上游请求体（唯一真相源）。
    body: Map<String, Value>,
    /// tuple schema 原始定义，仅供响应重构时使用。
    pub tuple_schema: Option<Value>,
    /// 是否由客户端显式提供了 prompt cache key。
    pub explicit_prompt_cache_key: bool,
    /// 客户端会话 ID。
    pub client_conversation_id: Option<String>,
    /// 变体身份键。
    pub variant_identity: Option<String>,
    /// 代理侧识别的客户端 IP，仅用于管理端使用记录展示。
    pub client_ip: Option<String>,
    /// 客户端 User-Agent，仅用于管理端使用记录展示。
    pub client_user_agent: Option<String>,
    /// 是否偏好 WebSocket 传输。
    pub use_websocket: bool,
    /// 是否强制 HTTP SSE。
    pub force_http_sse: bool,
    /// turn state 透传头。
    pub turn_state: Option<String>,
    /// turn metadata 透传头。
    pub turn_metadata: Option<String>,
    /// beta features 透传头。
    pub beta_features: Option<String>,
    /// 客户端版本头。
    pub version: Option<String>,
    /// timing metrics 透传头。
    pub include_timing_metrics: Option<String>,
    /// codex window id。
    pub codex_window_id: Option<String>,
    /// 父线程 ID。
    pub parent_thread_id: Option<String>,
}

impl Serialize for CodexResponsesRequest {
    /// 上游 body 序列化即原始 `body` map（HTTP SSE 直发；WebSocket 在外层前置 `type`）。
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.body.serialize(serializer)
    }
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

    if request.previous_response_id().is_some() {
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

/// 判断已收到的 Responses SSE 内容是否包含首个完整的有效数据事件。
pub fn response_body_has_first_event(body_bytes: &[u8]) -> bool {
    let body = String::from_utf8_lossy(body_bytes);
    let Some(complete_body) = complete_sse_body_prefix(&body) else {
        return false;
    };
    parse_sse_events(complete_body)
        .is_ok_and(|events| events.iter().any(response_sse_event_has_first_output))
}

/// 已收到首个有效 Responses SSE 输出事件时记录首 token 耗时。
pub fn update_first_response_event_ms(
    started_at: Instant,
    body_bytes: &[u8],
    first_token_ms: &mut Option<i64>,
) {
    if first_token_ms.is_none() && response_body_has_first_event(body_bytes) {
        *first_token_ms = Some(elapsed_millis_i64(started_at).max(1));
    }
}

fn response_sse_event_has_first_output(event: &SseEvent) -> bool {
    let data = event.data.trim();
    if data.is_empty() || data == "[DONE]" {
        return false;
    }

    let value = serde_json::from_str::<Value>(data).ok();
    let event_type = event.event.as_deref().or_else(|| {
        value
            .as_ref()
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
    });

    match event_type {
        Some("response.output_text.delta")
        | Some("response.reasoning_summary_text.delta")
        | Some("response.reasoning_text.delta")
        | Some("response.function_call_arguments.delta")
        | Some("response.custom_tool_call_input.delta") => value
            .as_ref()
            .and_then(|value| value.get("delta"))
            .and_then(Value::as_str)
            .is_some_and(|delta| !delta.is_empty()),
        Some("response.output_text.done") => value
            .as_ref()
            .and_then(|value| value.get("text"))
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty()),
        Some("response.function_call_arguments.done") => value
            .as_ref()
            .and_then(|value| value.get("arguments"))
            .and_then(Value::as_str)
            .is_some_and(|arguments| !arguments.is_empty()),
        Some("response.output_item.added" | "response.output_item.done") => value
            .as_ref()
            .and_then(|value| value.get("item"))
            .is_some_and(Value::is_object),
        _ => false,
    }
}

fn complete_sse_body_prefix(body: &str) -> Option<&str> {
    let lf_end = body.rfind("\n\n").map(|index| index + 2);
    let crlf_end = body.rfind("\r\n\r\n").map(|index| index + 4);
    lf_end
        .into_iter()
        .chain(crlf_end)
        .max()
        .map(|end| &body[..end])
}

/// 从 Codex SSE 收集出的非流式 Responses 结果。
#[derive(Debug, Clone, PartialEq)]
pub enum CollectedResponse {
    /// 收集到 `response.completed` 或 `response.incomplete`。
    Completed(Value),
    /// 收集到 `response.failed` 或 `error`。
    Failed(ResponsesSseFailure),
    /// SSE 未包含成功终止响应。
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
    pub(crate) fn from_event(event: &str, value: &Value) -> Self {
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
    let mut terminal_response = None;
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
            Some("response.completed" | "response.incomplete") => {
                terminal_response = value.get("response").cloned();
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
    let Some(mut response) = terminal_response else {
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
            Some("response.completed" | "response.incomplete") => {
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
    match responses_event_type(event_name, Some(&data)) {
        Some("response.output_text.delta") => {
            reconvert_output_text_delta_tuple_values(&mut data, tuple_schema);
        }
        Some("response.output_item.done") => {
            if let Some(item) = data.get_mut("item") {
                reconvert_output_item_tuple_values(item, tuple_schema);
            }
        }
        Some("response.completed" | "response.incomplete") => {
            if let Some(response) = data.get_mut("response") {
                reconvert_completed_response_tuple_values(response, Some(tuple_schema));
                sync_output_text_from_output(response);
            }
        }
        _ => {}
    }
    data
}

/// 判断 Responses SSE 事件是否为终止事件。
pub fn response_sse_event_is_terminal(event: &SseEvent) -> bool {
    let value = serde_json::from_str::<Value>(&event.data).ok();
    matches!(
        responses_event_type(event.event.as_deref(), value.as_ref()),
        Some("response.completed" | "response.incomplete" | "response.failed" | "error")
    )
}

fn responses_event_type<'a>(
    event_name: Option<&'a str>,
    data: Option<&'a Value>,
) -> Option<&'a str> {
    event_name.or_else(|| {
        data.and_then(|data| data.get("type"))
            .and_then(Value::as_str)
    })
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
    if response.get("status").and_then(Value::as_str) == Some("incomplete") {
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

    let item_status = response
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed");
    response["output"] = json!([{
        "type": "message",
        "status": item_status,
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

impl CodexResponsesRequest {
    /// 从客户端原始 Responses JSON object 构造上游请求。
    ///
    /// 只做最小规范化：`input` 缺省为 `[]`、`stream` 缺省 `true`、`store` 缺省 `false`。
    /// 其余字段（含未知字段）原样保留在 `body` 中透传上游。
    pub fn from_body(mut body: Map<String, Value>) -> Self {
        body.entry("input")
            .or_insert_with(|| Value::Array(Vec::new()));
        body.entry("stream").or_insert(Value::Bool(true));
        body.entry("store").or_insert(Value::Bool(false));
        Self {
            body,
            tuple_schema: None,
            explicit_prompt_cache_key: false,
            client_conversation_id: None,
            variant_identity: None,
            client_ip: None,
            client_user_agent: None,
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

    /// 构造默认的 HTTP SSE 请求（测试与内部构造用）。
    pub fn new_http_sse(
        model: impl Into<String>,
        instructions: impl Into<String>,
        input: Vec<Value>,
    ) -> Self {
        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(model.into()));
        body.insert(
            "instructions".to_string(),
            Value::String(instructions.into()),
        );
        body.insert("input".to_string(), Value::Array(input));
        Self::from_body(body)
    }

    /// 上游 body 的只读视图。
    pub fn body(&self) -> &Map<String, Value> {
        &self.body
    }

    // --- body 字段类型化访问器（上游语义字段，透传不重写）---

    /// 模型名。
    pub fn model(&self) -> &str {
        self.body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// 设置模型名（模型后缀路由归一）。
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.body
            .insert("model".to_string(), Value::String(model.into()));
    }

    /// 指令文本（缺省空串）。
    pub fn instructions(&self) -> &str {
        self.body
            .get("instructions")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// 输入条目切片（非数组时为空）。
    pub fn input(&self) -> &[Value] {
        self.body
            .get("input")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice)
    }

    /// 替换输入条目（implicit resume / 字符串输入兼容）。
    pub fn set_input(&mut self, input: Vec<Value>) {
        self.body.insert("input".to_string(), Value::Array(input));
    }

    /// 是否流式返回。
    pub fn stream(&self) -> bool {
        self.body
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// 设置流式标志。
    pub fn set_stream(&mut self, stream: bool) {
        self.body.insert("stream".to_string(), Value::Bool(stream));
    }

    /// 是否要求上游存储响应。
    pub fn store(&self) -> bool {
        self.body
            .get("store")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// reasoning 配置（透传，不规整）。
    pub fn reasoning(&self) -> Option<&Value> {
        self.body.get("reasoning")
    }

    /// 工具定义数组（非数组或空时 None）。
    pub fn tools(&self) -> Option<&[Value]> {
        self.body
            .get("tools")
            .and_then(Value::as_array)
            .filter(|tools| !tools.is_empty())
            .map(Vec::as_slice)
    }

    /// include 列表（透传原值）。
    pub fn include(&self) -> Option<&Value> {
        self.body.get("include")
    }

    /// service tier（透传原值）。
    pub fn service_tier(&self) -> Option<&str> {
        self.body.get("service_tier").and_then(Value::as_str)
    }

    /// 设置 service tier（模型后缀路由 / 归一）。
    pub fn set_service_tier(&mut self, service_tier: Option<String>) {
        match service_tier {
            Some(value) => {
                self.body
                    .insert("service_tier".to_string(), Value::String(value));
            }
            None => {
                self.body.remove("service_tier");
            }
        }
    }

    /// 前一个 response ID。
    pub fn previous_response_id(&self) -> Option<&str> {
        self.body
            .get("previous_response_id")
            .and_then(Value::as_str)
    }

    /// 设置 / 清除前一个 response ID。
    pub fn set_previous_response_id(&mut self, previous_response_id: Option<String>) {
        match previous_response_id {
            Some(value) => {
                self.body
                    .insert("previous_response_id".to_string(), Value::String(value));
            }
            None => {
                self.body.remove("previous_response_id");
            }
        }
    }

    /// 提示缓存键。
    pub fn prompt_cache_key(&self) -> Option<&str> {
        self.body.get("prompt_cache_key").and_then(Value::as_str)
    }

    /// 设置提示缓存键。
    pub fn set_prompt_cache_key(&mut self, prompt_cache_key: Option<String>) {
        match prompt_cache_key {
            Some(value) => {
                self.body
                    .insert("prompt_cache_key".to_string(), Value::String(value));
            }
            None => {
                self.body.remove("prompt_cache_key");
            }
        }
    }

    /// client metadata（透传原值）。
    pub fn client_metadata(&self) -> Option<&Value> {
        self.body.get("client_metadata")
    }

    /// 设置 / 合并 client metadata。
    pub fn set_client_metadata(&mut self, client_metadata: Option<Value>) {
        match client_metadata {
            Some(value) => {
                self.body.insert("client_metadata".to_string(), value);
            }
            None => {
                self.body.remove("client_metadata");
            }
        }
    }

    /// 判断请求是否声明了图片生成工具。
    pub fn expects_image_generation(&self) -> bool {
        self.tools().is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool.get("type").and_then(Value::as_str) == Some("image_generation"))
        })
    }
}

/// Codex compact 端点请求体。
///
/// 发往上游的 Responses compact 请求。`body` 持有客户端原始 JSON object
/// （已剥离 compact 上游不接受的字段），逐字段透传上游；
/// `client_ip`/`client_user_agent` 仅供管理端使用记录展示，不进上游 body。
#[derive(Debug, Clone)]
pub struct CodexCompactRequest {
    /// 上游请求体（唯一真相源）。
    pub body: Map<String, Value>,
    /// 代理侧识别的客户端 IP，仅用于管理端使用记录展示。
    pub client_ip: Option<String>,
    /// 客户端 User-Agent，仅用于管理端使用记录展示。
    pub client_user_agent: Option<String>,
}

impl Serialize for CodexCompactRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.body.serialize(serializer)
    }
}

impl CodexCompactRequest {
    /// 模型名。
    pub fn model(&self) -> &str {
        self.body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// 设置模型名（模型后缀路由归一）。
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.body
            .insert("model".to_string(), Value::String(model.into()));
    }

    /// reasoning 配置（透传原值）。
    pub fn reasoning(&self) -> Option<&Value> {
        self.body.get("reasoning")
    }
}
