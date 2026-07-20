use std::{collections::BTreeSet, fmt};

use gateway_protocol::openai::{
    events,
    schema::reconvert_tuple_values,
    sse::{SseError, SseEvent, parse_sse_events},
};
use serde::Serialize;
use serde_json::{Map, Value};

const MULTI_AGENT_MODE_OPEN_TAG: &str = "<multi_agent_mode>";
const MULTI_AGENT_MODE_CLOSE_TAG: &str = "</multi_agent_mode>";
const PROACTIVE_MULTI_AGENT_MODE_PREFIX: &str = "Proactive multi-agent delegation is active.";

/// Codex Responses 上游请求体。
///
/// 发往上游的 Responses 请求。`body` 持有客户端原始 JSON object，逐字段（含顺序、
/// 含未知字段）透传上游，是上游请求体的唯一来源；`use_websocket`/`force_http_sse`
/// 仅用于本地传输选择，不写入 body。常用字段通过访问器方法读写。
/// 普通客户端请求不修改 body；模型路由只写入明确受控字段。
///
/// 其余字段是代理控制状态，不进上游 body（原 `#[serde(skip)]` 字段）。
#[derive(Clone)]
pub struct CodexResponsesRequest {
    /// 上游请求体（唯一真相源）。
    body: Map<String, Value>,
    /// tuple schema 原始定义，仅供响应重构时使用。
    pub tuple_schema: Option<Value>,
    /// 是否由客户端显式提供了 prompt cache key。
    pub explicit_prompt_cache_key: bool,
    /// 客户端会话 ID。
    pub client_conversation_id: Option<String>,
    /// 客户端 session ID，仅保留在受控本地上下文。
    pub client_session_id: Option<String>,
    /// 客户端 thread ID，仅保留在受控本地上下文。
    pub client_thread_id: Option<String>,
    /// 客户端 request ID，仅保留在受控本地上下文。
    pub client_request_id: Option<String>,
    /// 客户端 turn ID，仅保留在受控本地上下文。
    pub client_turn_id: Option<String>,
    /// 连接池和 affinity 使用的本地会话身份，不发送上游。
    pub local_conversation_id: Option<String>,
    /// 变体身份键。
    pub variant_identity: Option<String>,
    /// 代理侧识别的客户端 IP，仅用于管理端使用记录展示。
    pub client_ip: Option<String>,
    /// 客户端 User-Agent，仅用于管理端使用记录展示。
    pub client_user_agent: Option<String>,
    /// 已鉴权客户端 API key 的稳定 ID，仅用于事实归因。
    pub client_api_key_id: Option<String>,
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
    /// Responses Lite 请求语义；HTTP 使用 header，WebSocket 使用 client metadata 投影。
    pub responses_lite: Option<String>,
    /// Memory consolidation 请求语义；HTTP 与 WebSocket opening 均使用 header。
    pub memgen_request: Option<String>,
    /// codex window id。
    pub codex_window_id: Option<String>,
    /// 父线程 ID。
    pub parent_thread_id: Option<String>,
    /// 已知 previous response 的持久化范围，仅用于本地 transport 校验。
    pub previous_response_scope: Option<PreviousResponseScope>,
}

impl fmt::Debug for CodexResponsesRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexResponsesRequest")
            .field("body", &"<not included in Debug>")
            .field("has_tuple_schema", &self.tuple_schema.is_some())
            .field("explicit_prompt_cache_key", &self.explicit_prompt_cache_key)
            .field(
                "has_local_conversation_id",
                &self.local_conversation_id.is_some(),
            )
            .field("use_websocket", &self.use_websocket)
            .field("force_http_sse", &self.force_http_sse)
            .finish()
    }
}

/// previous response 在上游的可续接范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviousResponseScope {
    Persisted,
    ConnectionLocal,
    ExternalUnknown,
}

/// Responses 请求携带的 Codex 运行语义。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexRequestSemantics {
    /// Codex turn metadata 中的请求类型。
    pub request_kind: Option<String>,
    /// Codex turn metadata 中的子代理类型。
    pub subagent_kind: Option<String>,
    /// Codex 客户端选择的推理预设。
    pub reasoning_preset: Option<&'static str>,
    /// 请求是否为远端压缩。
    pub compact: bool,
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

/// Codex Responses 请求对上游传输的显式要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportRequirement {
    /// 客户端显式要求 HTTP。
    HttpRequired,
    /// `generate=false + store=false` 预热必须保留在同一条 WebSocket。
    ExplicitWebSocketWarmup,
    /// 只能使用持有指定 connection-local response 的精确 WebSocket。
    ExactWebSocketContinuation,
    /// previous response 已持久化，允许 WebSocket 或 HTTP/2。
    PersistedContinuation,
    /// previous response 的所有权未知，只允许当前选定账号原样尝试。
    ExternalUnknown,
    /// 没有 previous response 的普通新链。
    NewChain,
}

impl TransportRequirement {
    /// 是否必须使用 WebSocket，且禁止 HTTP fallback。
    pub fn requires_websocket(self) -> bool {
        matches!(
            self,
            Self::ExplicitWebSocketWarmup | Self::ExactWebSocketContinuation
        )
    }

    /// WebSocket 尚未发送 payload 时失败，是否允许切到同账号 HTTP/2。
    pub fn allows_pre_send_http_fallback(self) -> bool {
        matches!(
            self,
            Self::PersistedContinuation | Self::ExternalUnknown | Self::NewChain
        )
    }

    /// 用于审计与遥测的稳定名称。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HttpRequired => "http_required",
            Self::ExplicitWebSocketWarmup => "explicit_websocket_warmup",
            Self::ExactWebSocketContinuation => "exact_websocket_continuation",
            Self::PersistedContinuation => "persisted_continuation",
            Self::ExternalUnknown => "external_unknown",
            Self::NewChain => "new_chain",
        }
    }
}

/// 将已完成 history preparation 的请求规范化为唯一 transport requirement。
pub fn transport_requirement(request: &CodexResponsesRequest) -> TransportRequirement {
    if !request.generate() && !request.store() {
        return TransportRequirement::ExplicitWebSocketWarmup;
    }
    if request.force_http_sse {
        return TransportRequirement::HttpRequired;
    }
    match request.previous_response_id() {
        Some(_) => match request.previous_response_scope {
            Some(PreviousResponseScope::Persisted) => TransportRequirement::PersistedContinuation,
            Some(PreviousResponseScope::ConnectionLocal) => {
                TransportRequirement::ExactWebSocketContinuation
            }
            Some(PreviousResponseScope::ExternalUnknown) | None => {
                TransportRequirement::ExternalUnknown
            }
        },
        None if request.use_websocket => TransportRequirement::NewChain,
        None => TransportRequirement::HttpRequired,
    }
}

/// 单个 Responses 事件对计时系统提供的稳定语义信号。
///
/// `protocol_progress` 只说明上游仍在工作，不能替代首字；其余字段分别标记
/// 客户端可消费的输出、reasoning 输出与正文输出。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResponseEventSignals {
    pub protocol_progress: bool,
    pub semantic_output: bool,
    pub reasoning_output: bool,
    pub text_output: bool,
}

/// 从已解析的 Responses 事件提取计时语义。
///
/// 生命周期帧和结构帧不会被当作输出。`response.output_item.done` 与终态帧只有
/// 实际携带文本、工具参数、推理或图片结果时才算语义输出。
pub fn response_event_signals(event_type: Option<&str>, value: &Value) -> ResponseEventSignals {
    let mut signals = ResponseEventSignals {
        protocol_progress: !matches!(event_type, Some("response.failed" | "error")),
        ..ResponseEventSignals::default()
    };
    match event_type {
        Some("response.output_text.delta") => {
            signals.text_output = non_empty_string(value.get("delta"));
            signals.semantic_output = signals.text_output;
        }
        Some("response.output_text.done") => {
            signals.text_output = non_empty_string(value.get("text"));
            signals.semantic_output = signals.text_output;
        }
        Some("response.refusal.delta") => {
            signals.text_output = non_empty_string(value.get("delta"));
            signals.semantic_output = signals.text_output;
        }
        Some("response.refusal.done") => {
            signals.text_output = non_empty_string(value.get("refusal"));
            signals.semantic_output = signals.text_output;
        }
        Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => {
            signals.reasoning_output = non_empty_string(value.get("delta"));
            signals.semantic_output = signals.reasoning_output;
        }
        Some(
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta",
        ) => {
            signals.semantic_output = non_empty_string(value.get("delta"));
        }
        Some("response.function_call_arguments.done") => {
            signals.semantic_output = non_empty_string(value.get("arguments"));
        }
        Some("response.image_generation_call.partial_image") => {
            signals.semantic_output = non_empty_string(
                value
                    .get("partial_image_b64")
                    .or_else(|| value.get("partial_image")),
            );
        }
        Some("response.output_item.done") => {
            if let Some(item) = value.get("item") {
                merge_output_signals(&mut signals, output_item_signals(item));
            }
        }
        Some("response.content_part.done") => {
            if let Some(part) = value.get("part") {
                merge_output_signals(&mut signals, output_item_signals(part));
            }
        }
        Some("response.completed" | "response.incomplete") => {
            if let Some(items) = value.pointer("/response/output").and_then(Value::as_array) {
                merge_output_signals(&mut signals, output_items_signals(items));
            }
        }
        Some(event_type) if event_type.ends_with(".delta") => {
            signals.semantic_output = non_empty_semantic_value(value.get("delta"));
        }
        Some(event_type) if event_type.ends_with(".done") => {
            signals.semantic_output = [
                "text",
                "refusal",
                "arguments",
                "input",
                "transcript",
                "data",
                "output",
                "result",
            ]
            .into_iter()
            .any(|field| non_empty_semantic_value(value.get(field)));
        }
        Some(event_type) if is_tool_execution_event(event_type) => {
            signals.semantic_output = true;
        }
        _ => {}
    }
    signals
}

/// 判断已收到的 Responses SSE 内容是否包含首个完整的语义输出事件。
pub fn response_body_has_semantic_output(body_bytes: &[u8]) -> bool {
    response_body_signals(body_bytes).semantic_output
}

fn response_body_signals(body_bytes: &[u8]) -> ResponseEventSignals {
    let body = String::from_utf8_lossy(body_bytes);
    let Some(complete_body) = complete_sse_body_prefix(&body) else {
        return ResponseEventSignals::default();
    };
    let Ok(events) = parse_sse_events(complete_body) else {
        return ResponseEventSignals::default();
    };
    events
        .iter()
        .fold(ResponseEventSignals::default(), |mut signals, event| {
            let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
                return signals;
            };
            let event_type = event.event.as_deref().or_else(|| {
                value
                    .get("type")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
            });
            merge_event_signals(&mut signals, response_event_signals(event_type, &value));
            signals
        })
}

fn merge_event_signals(target: &mut ResponseEventSignals, source: ResponseEventSignals) {
    target.protocol_progress |= source.protocol_progress;
    merge_output_signals(target, source);
}

fn merge_output_signals(target: &mut ResponseEventSignals, source: ResponseEventSignals) {
    target.semantic_output |= source.semantic_output;
    target.reasoning_output |= source.reasoning_output;
    target.text_output |= source.text_output;
}

fn output_items_signals(items: &[Value]) -> ResponseEventSignals {
    items
        .iter()
        .fold(ResponseEventSignals::default(), |mut signals, item| {
            merge_output_signals(&mut signals, output_item_signals(item));
            signals
        })
}

fn output_item_signals(item: &Value) -> ResponseEventSignals {
    let mut signals = ResponseEventSignals::default();
    match item.get("type").and_then(Value::as_str) {
        Some("output_text" | "text") => {
            signals.text_output = non_empty_string(item.get("text"));
            signals.semantic_output = signals.text_output;
        }
        Some("reasoning") => {
            signals.reasoning_output = non_empty_semantic_value(item.get("text"))
                || non_empty_semantic_value(item.get("summary"));
            signals.semantic_output = signals.reasoning_output;
        }
        Some("refusal") => {
            signals.text_output =
                non_empty_string(item.get("refusal")) || non_empty_string(item.get("text"));
            signals.semantic_output = signals.text_output;
        }
        Some(item_type) if item_type.ends_with("_call") => {
            // done 的工具调用本身已进入不可安全重试的语义边界；不依赖每种工具的字段表。
            signals.semantic_output = true;
        }
        _ => {
            if let Some(items) = item.get("content").and_then(Value::as_array) {
                merge_output_signals(&mut signals, output_items_signals(items));
            }
        }
    }
    signals
}

fn non_empty_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
}

fn non_empty_semantic_value(value: Option<&Value>) -> bool {
    match value {
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Array(value)) => !value.is_empty(),
        Some(Value::Object(value)) => !value.is_empty(),
        Some(Value::Number(_) | Value::Bool(_)) => true,
        Some(Value::Null) | None => false,
    }
}

fn is_tool_execution_event(event_type: &str) -> bool {
    let Some((call_type, phase)) = event_type
        .strip_prefix("response.")
        .and_then(|event| event.rsplit_once('.'))
    else {
        return false;
    };
    call_type.ends_with("_call")
        && matches!(
            phase,
            "in_progress" | "searching" | "interpreting" | "completed" | "failed"
        )
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectedResponse {
    /// 收集到 `response.completed`。
    Completed(Value),
    /// 收集到 `response.incomplete`。
    Incomplete(Value),
    /// 收集到 `response.failed` 或 `error`。
    Failed(ResponsesSseFailure),
    /// SSE 未包含成功终止响应。
    MissingCompleted,
    /// 完成响应为空。
    Empty,
}

/// Codex Responses SSE 失败事件。
#[derive(Clone, PartialEq, Eq)]
pub struct ResponsesSseFailure {
    /// SSE event 名称。
    pub event: String,
    /// 上游错误消息。
    pub message: String,
    /// 上游错误码。
    pub upstream_code: Option<String>,
    /// 上游显式错误类型；不从业务码推导。
    pub upstream_type: Option<String>,
    /// 上游显式状态码；不从业务码或错误类型推导。
    pub explicit_status_code: Option<u16>,
    /// 上游显式重试间隔，或从官方限流消息中解析出的重试间隔。
    pub retry_after_seconds: Option<u64>,
}

impl fmt::Debug for ResponsesSseFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponsesSseFailure")
            .field("event", &self.event)
            .field("message", &"<redacted>")
            .field("has_upstream_code", &self.upstream_code.is_some())
            .field("has_upstream_type", &self.upstream_type.is_some())
            .field("explicit_status_code", &self.explicit_status_code)
            .field("retry_after_seconds", &self.retry_after_seconds)
            .finish()
    }
}

impl ResponsesSseFailure {
    pub fn from_event(event: &str, value: &Value) -> Self {
        Self {
            event: event.to_string(),
            message: failure_message(value).unwrap_or_else(|| "Codex upstream SSE failed".into()),
            upstream_code: failure_code(value),
            upstream_type: failure_type(value),
            explicit_status_code: failure_explicit_status_code(value),
            retry_after_seconds: events::retry_after_seconds_from_value(value),
        }
    }
}

/// 从完成 SSE 中提取会话亲和性和 replay 元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedResponseMetadata {
    /// 上游 response id。
    pub response_id: String,
    /// 完成响应中的 function call ids。
    pub function_call_ids: Vec<String>,
    /// 完成响应的完整 output items。
    pub output: Vec<Value>,
}

/// 从 Codex Responses SSE 中提取完成响应元数据。
pub fn completed_response_metadata(
    body: &str,
) -> Result<Option<CompletedResponseMetadata>, SseError> {
    let events = parse_sse_events(body)?;
    let mut response_id = None;
    let mut function_call_ids = BTreeSet::new();
    let mut output = Vec::new();
    let mut completed_items = Vec::new();

    for event in events {
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        match event.event.as_deref() {
            Some("response.output_item.done") => {
                if let Some(item) = value.get("item") {
                    completed_items.push(item.clone());
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
                if let Some(completed_output) =
                    value.pointer("/response/output").and_then(Value::as_array)
                {
                    completed_output.clone_into(&mut output);
                }
                if output.is_empty() {
                    output.clone_from(&completed_items);
                }
            }
            _ => {}
        }
    }

    Ok(response_id.map(|response_id| CompletedResponseMetadata {
        response_id,
        function_call_ids: function_call_ids.into_iter().collect(),
        output,
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

fn failure_type(value: &Value) -> Option<String> {
    failure_error(value)
        .and_then(|error| error.get("type"))
        .and_then(Value::as_str)
        .filter(|error_type| !error_type.trim().is_empty())
        .map(ToString::to_string)
}

fn failure_explicit_status_code(value: &Value) -> Option<u16> {
    value
        .get("status")
        .or_else(|| value.get("status_code"))
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
}

fn failure_error(value: &Value) -> Option<&Value> {
    value
        .pointer("/response/error")
        .or_else(|| value.get("error"))
}

impl CodexResponsesRequest {
    /// 从客户端原始 Responses JSON object 构造上游请求。
    ///
    /// 客户端提供的字段（含未知字段）原样保留在 `body` 中透传上游。
    /// 协议默认值仅由类型化访问器在本地解释，不写回上游正文。
    pub fn from_body(body: Map<String, Value>) -> Self {
        Self {
            body,
            tuple_schema: None,
            explicit_prompt_cache_key: false,
            client_conversation_id: None,
            client_session_id: None,
            client_thread_id: None,
            client_request_id: None,
            client_turn_id: None,
            local_conversation_id: None,
            variant_identity: None,
            client_ip: None,
            client_user_agent: None,
            client_api_key_id: None,
            use_websocket: false,
            force_http_sse: false,
            turn_state: None,
            turn_metadata: None,
            beta_features: None,
            version: None,
            include_timing_metrics: None,
            responses_lite: None,
            memgen_request: None,
            codex_window_id: None,
            parent_thread_id: None,
            previous_response_scope: None,
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

    /// Provider adapter 编码阶段写入已经白名单校验的上游字段。
    pub(crate) fn body_mut(&mut self) -> &mut Map<String, Value> {
        &mut self.body
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

    /// 替换输入条目（仅用于代理持有完整历史时的换号重放）。
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

    /// 是否实际生成模型响应；官方预热请求会显式传入 `false`。
    pub fn generate(&self) -> bool {
        self.body
            .get("generate")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// 设置上游存储标志。
    pub fn set_store(&mut self, store: bool) {
        self.body.insert("store".to_string(), Value::Bool(store));
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
                self.previous_response_scope = None;
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

    /// 提取 Codex 请求类型、子代理类型、推理预设与压缩语义。
    pub fn semantics(&self) -> CodexRequestSemantics {
        let turn_metadata = self.turn_metadata.as_deref().or_else(|| {
            self.client_metadata()?
                .get("x-codex-turn-metadata")?
                .as_str()
        });
        let effort = self
            .reasoning()
            .and_then(|value| value.get("effort"))
            .and_then(Value::as_str);
        let proactive_multi_agent = self
            .latest_multi_agent_mode()
            .is_some_and(|mode| mode.starts_with(PROACTIVE_MULTI_AGENT_MODE_PREFIX));
        let compact_trigger = self
            .input()
            .iter()
            .any(|item| item.get("type").and_then(Value::as_str) == Some("compaction_trigger"));
        codex_request_semantics_from_parts(
            turn_metadata,
            effort,
            proactive_multi_agent,
            compact_trigger,
        )
    }

    fn latest_multi_agent_mode(&self) -> Option<&str> {
        latest_multi_agent_mode(self.input().iter())
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

    /// 替换客户端原本提供的账号身份字段；无法安全重建时删除该字段。
    pub fn replace_existing_identity_field(&mut self, key: &str, value: Option<&str>) {
        if !self.body.contains_key(key) {
            return;
        }
        match value.filter(|value| !value.trim().is_empty()) {
            Some(value) => {
                self.body
                    .insert(key.to_string(), Value::String(value.to_string()));
            }
            None => {
                self.body.remove(key);
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

fn non_empty_owned_string(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

pub(crate) fn codex_request_semantics_from_parts(
    turn_metadata: Option<&str>,
    effort: Option<&str>,
    proactive_multi_agent: bool,
    compact_trigger: bool,
) -> CodexRequestSemantics {
    let turn_metadata = turn_metadata.and_then(|value| serde_json::from_str::<Value>(value).ok());
    let request_kind = turn_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("request_kind"))
        .and_then(Value::as_str)
        .and_then(non_empty_owned_string);
    let subagent_kind = turn_metadata
        .as_ref()
        .and_then(|metadata| metadata.get("subagent_kind"))
        .and_then(Value::as_str)
        .and_then(non_empty_owned_string);
    let reasoning_preset =
        (subagent_kind.is_none() && effort == Some("max") && proactive_multi_agent)
            .then_some("ultra");
    let compact = request_kind.as_deref() == Some("compaction") || compact_trigger;
    CodexRequestSemantics {
        request_kind,
        subagent_kind,
        reasoning_preset,
        compact,
    }
}

fn latest_multi_agent_mode<'a>(
    input: impl DoubleEndedIterator<Item = &'a Value>,
) -> Option<&'a str> {
    input.rev().find_map(|item| {
        if item.get("role").and_then(Value::as_str) != Some("developer") {
            return None;
        }
        item.get("content")?
            .as_array()?
            .iter()
            .rev()
            .filter_map(|content| content.get("text").and_then(Value::as_str))
            .find_map(multi_agent_mode_from_text)
    })
}

fn multi_agent_mode_from_text(text: &str) -> Option<&str> {
    let close = text.rfind(MULTI_AGENT_MODE_CLOSE_TAG)?;
    let before_close = &text[..close];
    let open = before_close.rfind(MULTI_AGENT_MODE_OPEN_TAG)?;
    let body = &before_close[open + MULTI_AGENT_MODE_OPEN_TAG.len()..];
    Some(body.trim())
}
