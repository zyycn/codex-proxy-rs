//! Codex Responses 请求、传输要求、事件信号与流式错误的协议解析

use std::{fmt, time::Duration};

use gateway_core::account::FastMode;
use gateway_protocol::openai::{
    CodexResponsesRequestSemantics as CodexRequestSemantics,
    codex_responses_request_semantics_with_turn_metadata, events,
};
use reqwest::header::HeaderMap;
use serde::Serialize;
use serde_json::{Map, Value};

/// 官方 Codex 客户端据此触发完整历史重放的稳定错误码
pub(crate) const PREVIOUS_RESPONSE_NOT_FOUND_CODE: &str = "previous_response_not_found";
/// Responses WebSocket 用于回传同一 turn 不透明状态的官方 client metadata 键
pub(crate) const X_CODEX_TURN_STATE_CLIENT_METADATA_KEY: &str = "x-codex-turn-state";
/// 本地生成 history unavailable 错误时使用的官方提示文本
pub(crate) const PREVIOUS_RESPONSE_NOT_FOUND_MESSAGE: &str =
    "Previous response was not found. Retrying the full request.";
/// Codex 自动审批（Guardian）请求声明的子代理类型
const GUARDIAN_SUBAGENT_KIND: &str = "guardian";
/// Codex Responses 上游请求体
///
/// 发往上游的 Responses 请求
/// `body` 持有客户端原始 JSON object，逐字段（含顺序、
/// 含未知字段）透传上游，是上游请求体的唯一来源；`use_websocket`/`force_http_sse`
/// 仅用于本地传输选择，不写入 body
/// 常用字段通过访问器方法读写
/// 普通客户端请求不修改 body；模型路由只写入明确受控字段
///
/// 其余字段是代理控制状态，不进上游 body（原 `#[serde(skip)]` 字段）
#[derive(Clone)]
pub struct CodexResponsesRequest {
    /// 上游请求体（唯一真相源）
    body: Map<String, Value>,
    /// API 边界保存、Provider 逐条恢复的普通客户端请求头
    pub(crate) passthrough_headers: HeaderMap,
    /// 是否由客户端显式提供了 prompt cache key
    pub explicit_prompt_cache_key: bool,
    /// 客户端会话 ID
    pub client_conversation_id: Option<String>,
    /// 客户端 session ID，仅保留在受控本地上下文
    pub client_session_id: Option<String>,
    /// 官方元数据中的逻辑会话身份，独立于缓存路由键
    pub client_account_session_id: Option<String>,
    /// 后代线程身份，用于严格模式下只跟随会话账号
    pub client_account_follow_only: bool,
    /// 客户端 thread ID，仅保留在受控本地上下文
    pub client_thread_id: Option<String>,
    /// 客户端 request ID，仅保留在受控本地上下文
    pub client_request_id: Option<String>,
    /// 客户端 turn ID，仅保留在受控本地上下文
    pub client_turn_id: Option<String>,
    /// 连接池和 affinity 使用的本地会话身份，不发送上游
    pub local_conversation_id: Option<String>,
    /// 变体身份键
    pub variant_identity: Option<String>,
    /// 代理侧识别的客户端 IP，仅用于管理端使用记录展示
    pub client_ip: Option<String>,
    /// 客户端 User-Agent，仅用于管理端使用记录展示
    pub client_user_agent: Option<String>,
    /// 已鉴权客户端 API key 的稳定 ID，仅用于事实归因
    pub client_api_key_id: Option<String>,
    /// 是否偏好 WebSocket 传输
    pub use_websocket: bool,
    /// 是否强制 HTTP SSE
    pub force_http_sse: bool,
    /// turn state 透传头
    pub turn_state: Option<String>,
    /// turn metadata 透传头
    pub turn_metadata: Option<String>,
    /// beta features 透传头
    pub beta_features: Option<String>,
    /// 下游 `version` 扩展头；存在时上游值由 Desktop 版本画像统一生成
    pub version: Option<String>,
    /// timing metrics 透传头
    pub include_timing_metrics: Option<String>,
    /// Responses Lite 请求语义；HTTP 使用 header，WebSocket 使用 client metadata 投影
    pub responses_lite: Option<String>,
    /// Memory consolidation 请求语义；HTTP 与 WebSocket opening 均使用 header
    pub memgen_request: Option<String>,
    /// Codex window ID
    pub codex_window_id: Option<String>,
    /// 代理分配的下游 WebSocket 连接 ID，仅用于隔离本地连接池通道
    pub downstream_websocket_connection_id: Option<String>,
    /// 父线程 ID
    pub parent_thread_id: Option<String>,
    /// 已知 previous response 的持久化范围，仅用于本地 transport 校验
    pub previous_response_scope: Option<PreviousResponseScope>,
}

impl fmt::Debug for CodexResponsesRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexResponsesRequest")
            .field("body", &"<not included in Debug>")
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

/// previous response 在上游的可续接范围
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviousResponseScope {
    Persisted,
    ConnectionLocal,
    ExternalUnknown,
}

impl Serialize for CodexResponsesRequest {
    /// 上游 body 序列化即原始 `body` map（HTTP SSE 直发；WebSocket 在外层前置 `type`）
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.body.serialize(serializer)
    }
}

/// Codex Responses 请求对上游传输的显式要求
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportRequirement {
    /// 客户端显式要求 HTTP
    HttpRequired,
    /// `generate=false + store=false` 预热必须保留在同一条 WebSocket
    ExplicitWebSocketWarmup,
    /// 客户端 WebSocket 的非持久化新链，默认在池化连接上建立后续续接状态
    /// Provider 可在发送前对 OAuth 大新链选择 HTTP，后续由客户端完整重放
    WebSocketNewChain,
    /// 只能使用持有指定 connection-local response 的精确 WebSocket
    ExactWebSocketContinuation,
    /// previous response 已持久化，允许 WebSocket 或 HTTP/2
    PersistedContinuation,
    /// previous response 的所有权未知，只允许当前选定账号原样尝试
    ExternalUnknown,
    /// 没有 previous response 的普通新链
    NewChain,
}

impl TransportRequirement {
    /// 默认是否要求 WebSocket；Provider 的 OAuth 大新链预检可在发送前选择 HTTP
    pub fn requires_websocket(self) -> bool {
        matches!(
            self,
            Self::ExplicitWebSocketWarmup
                | Self::WebSocketNewChain
                | Self::ExactWebSocketContinuation
        )
    }

    /// WebSocket 尚未发送 payload 时失败，是否允许切到同账号 HTTP/2
    pub fn allows_pre_send_http_fallback(self) -> bool {
        matches!(
            self,
            Self::PersistedContinuation | Self::ExternalUnknown | Self::NewChain
        )
    }

    /// 无续接依赖的新请求可以在明确的连接级拒绝后重新建连
    pub fn allows_connection_restart(self) -> bool {
        matches!(self, Self::NewChain | Self::WebSocketNewChain)
    }

    /// 用于审计与遥测的稳定名称
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HttpRequired => "http_required",
            Self::ExplicitWebSocketWarmup => "explicit_websocket_warmup",
            Self::WebSocketNewChain => "websocket_new_chain",
            Self::ExactWebSocketContinuation => "exact_websocket_continuation",
            Self::PersistedContinuation => "persisted_continuation",
            Self::ExternalUnknown => "external_unknown",
            Self::NewChain => "new_chain",
        }
    }
}

/// 将已完成 history preparation 的请求规范化为唯一 transport requirement
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
        // HTTP store=false 的成功响应无法在池化 WebSocket 上续接，默认不走 HTTP 快路径；
        // Provider 的 OAuth 大新链预检例外通过客户端完整重放恢复后续请求
        None if request.downstream_websocket_connection_id.is_some() && !request.store() => {
            TransportRequirement::WebSocketNewChain
        }
        None => TransportRequirement::NewChain,
    }
}

/// 仅空响应前导和元数据可在已交付后被下一次尝试替换
pub(crate) fn is_replaceable_response_prelude(event_type: &str, value: &Value) -> bool {
    match event_type {
        // 两类官方元数据都不代表输出，轮次与安全状态仍随原始事件交付
        "codex.response.metadata" | "response.metadata" | "codex.rate_limits" => true,
        "response.created" | "response.in_progress" => value
            .pointer("/response/output")
            .is_none_or(|output| output.as_array().is_some_and(Vec::is_empty)),
        _ => false,
    }
}

/// Responses 响应载荷与实际内容的观测信号
///
/// `response_data` 表示收到非空响应载荷；`semantic_output` 仍要求
/// 实际内容，供重试与交付边界使用，不能与首字观测互换
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResponseEventSignals {
    pub protocol_progress: bool,
    pub response_data: bool,
    pub semantic_output: bool,
    pub reasoning_output: bool,
    pub text_output: bool,
}

/// 仅使用同一条完成响应的官方时间戳计算响应耗时
///
/// 整数秒时间戳可能得到零跨度，缺失、倒序或无法落盘的时间均保留未知
pub(crate) fn response_duration_ms(value: &Value) -> Option<u64> {
    let response = value.get("response")?;
    let created_at = response.get("created_at")?.as_f64()?;
    let completed_at = response.get("completed_at")?.as_f64()?;
    if created_at < 0.0 || completed_at <= created_at {
        return None;
    }
    let duration = Duration::try_from_secs_f64(completed_at - created_at).ok()?;
    let millis = i64::try_from(duration.as_millis()).ok()?;
    (millis > 0).then_some(millis as u64)
}

/// 官方专项计时快照，毫秒小数按原始值保存，各层级不可混算
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct ResponseTimingMetrics {
    pub upstream_api_overhead_ms: Option<f64>,
    pub upstream_engine_ms: Option<f64>,
}

impl ResponseTimingMetrics {
    pub(crate) fn from_event(value: &Value) -> Self {
        let Some(metrics) = value.get("timing_metrics") else {
            return Self::default();
        };
        let valid_milliseconds =
            |value: &f64| value.is_finite() && *value >= 0.0 && *value < i64::MAX as f64;
        let milliseconds =
            |source: &Value, key: &str| source.get(key)?.as_f64().filter(valid_milliseconds);
        if let Some(path) = metrics.get("critical_path") {
            // 外层 logical_turn 可累计多次响应与工具等待，只取完整的当前响应阶段
            if path.get("scope").and_then(Value::as_str) != Some("response")
                || path.get("coverage").and_then(Value::as_str) != Some("complete")
                || path.get("boundary_type").and_then(Value::as_str)
                    != Some("actionable_output_item_done")
            {
                return Self::default();
            }
            // API 开销由推理前与其他处理构成，缺少任一分项时不能按零补齐
            let overhead = milliseconds(path, "responses_pre_inference_ms")
                .zip(milliseconds(path, "responses_other_ms"))
                .map(|(before, other)| before + other)
                .filter(valid_milliseconds);
            return Self {
                upstream_api_overhead_ms: overhead,
                upstream_engine_ms: milliseconds(path, "engine_wall_ms"),
            };
        }
        // 直接返回的专项指标保留原口径，明确标为整轮累计的值不能归入单次请求
        if metrics
            .get("timing_scope")
            .is_some_and(|scope| scope.as_str() != Some("response"))
        {
            return Self::default();
        }
        Self {
            upstream_api_overhead_ms: milliseconds(
                metrics,
                "responses_duration_excl_engine_and_client_tool_time_ms",
            ),
            upstream_engine_ms: milliseconds(metrics, "engine_service_total_ms"),
        }
    }

    pub(crate) fn merge(&mut self, incoming: Self) {
        if incoming.upstream_api_overhead_ms.is_some() {
            self.upstream_api_overhead_ms = incoming.upstream_api_overhead_ms;
        }
        if incoming.upstream_engine_ms.is_some() {
            self.upstream_engine_ms = incoming.upstream_engine_ms;
        }
    }
}

/// 从已解析的 Responses 事件提取计时语义
///
/// 首响应载荷由解码器在 JSON 解析前观测
/// 语义输出仍要求文本、推理、工具参数、图片结果或工具执行
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
        Some("response.reasoning_summary_text.done" | "response.reasoning_text.done") => {
            signals.reasoning_output =
                non_empty_string(value.get("text").or_else(|| value.get("summary")));
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
        Some("response.custom_tool_call_input.done") => {
            signals.semantic_output = non_empty_string(value.get("input"));
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
            // done 的工具调用本身已进入不可安全重试的语义边界；不依赖每种工具的字段表
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

/// Codex Responses SSE 失败事件
#[derive(Clone, PartialEq, Eq)]
pub struct ResponsesSseFailure {
    /// SSE event 名称
    pub event: String,
    /// 上游错误消息
    pub message: String,
    /// 上游错误码
    pub upstream_code: Option<String>,
    /// 上游显式 message，保留缺失与空串的区别
    pub upstream_message: Option<String>,
    /// 上游显式错误类型；不从业务码推导
    pub upstream_type: Option<String>,
    /// 上游显式状态码；不从业务码或错误类型推导
    pub explicit_status_code: Option<u16>,
    /// 上游显式重试间隔，或从官方限流消息中解析出的重试间隔
    pub retry_after_seconds: Option<u64>,
    /// 当前错误事件自身携带的请求 ID；不是连接 opening ID
    pub(crate) request_id: Option<String>,
    /// 上游错误事件的原始 JSON data；只应在明确的失败审计边界读取
    raw_body: String,
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
        Self::from_raw_event(event, &value.to_string(), value)
    }

    pub(crate) fn from_raw_event(event: &str, raw_body: &str, value: &Value) -> Self {
        Self {
            event: event.to_string(),
            message: failure_message(value).unwrap_or_else(|| "Codex upstream SSE failed".into()),
            upstream_code: failure_code(value),
            upstream_message: failure_message(value),
            upstream_type: failure_type(value),
            explicit_status_code: failure_explicit_status_code(value),
            retry_after_seconds: events::retry_after_seconds_from_value(value),
            request_id:
                crate::transport::diagnostics::CodexUpstreamDiagnostics::error_event_request_id(
                    value,
                ),
            raw_body: raw_body.to_owned(),
        }
    }

    /// 返回上游错误事件未经重编码的 JSON data
    #[must_use]
    pub fn raw_body(&self) -> &str {
        &self.raw_body
    }
}

fn failure_message(value: &Value) -> Option<String> {
    value
        .pointer("/response/error/message")
        .or_else(|| value.pointer("/error/message"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn failure_code(value: &Value) -> Option<String> {
    value
        .pointer("/response/error/code")
        .or_else(|| value.pointer("/error/code"))
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn failure_type(value: &Value) -> Option<String> {
    failure_error(value)
        .and_then(|error| error.get("type"))
        .and_then(Value::as_str)
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
    /// 从客户端原始 Responses JSON object 构造上游请求
    ///
    /// 客户端提供的字段（含未知字段）原样保留在 `body` 中透传上游
    /// 协议默认值仅由类型化访问器在本地解释，不写回上游正文
    pub fn from_body(body: Map<String, Value>) -> Self {
        Self {
            body,
            passthrough_headers: HeaderMap::new(),
            explicit_prompt_cache_key: false,
            client_conversation_id: None,
            client_session_id: None,
            client_account_session_id: None,
            client_account_follow_only: false,
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
            downstream_websocket_connection_id: None,
            parent_thread_id: None,
            previous_response_scope: None,
        }
    }

    /// 上游 body 的只读视图
    pub fn body(&self) -> &Map<String, Value> {
        &self.body
    }

    /// Provider adapter 编码阶段写入已经白名单校验的上游字段
    pub(crate) fn body_mut(&mut self) -> &mut Map<String, Value> {
        &mut self.body
    }

    // --- body 字段类型化访问器（上游语义字段，透传不重写）---

    /// 模型名
    pub fn model(&self) -> &str {
        self.body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// 指令文本（缺省空串）
    pub fn instructions(&self) -> &str {
        self.body
            .get("instructions")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// 输入条目切片（非数组时为空）
    pub fn input(&self) -> &[Value] {
        self.body
            .get("input")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice)
    }

    /// 是否流式返回
    pub fn stream(&self) -> bool {
        self.body
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// 是否要求上游存储响应
    pub fn store(&self) -> bool {
        self.body
            .get("store")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// 是否实际生成模型响应；官方预热请求会显式传入 `false`
    pub fn generate(&self) -> bool {
        self.body
            .get("generate")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// reasoning 配置（透传，不规整）
    pub fn reasoning(&self) -> Option<&Value> {
        self.body.get("reasoning")
    }

    /// 工具定义数组（非数组或空时 None）
    pub fn tools(&self) -> Option<&[Value]> {
        self.body
            .get("tools")
            .and_then(Value::as_array)
            .filter(|tools| !tools.is_empty())
            .map(Vec::as_slice)
    }

    /// include 列表（透传原值）
    pub fn include(&self) -> Option<&Value> {
        self.body.get("include")
    }

    /// service tier（透传原值）
    pub fn service_tier(&self) -> Option<&str> {
        self.body.get("service_tier").and_then(Value::as_str)
    }

    /// 只改写顶层档位；开启需要目录证据，其他显式档位保留客户端选择
    pub(crate) fn apply_fast_policy(&mut self, mode: FastMode, supports_priority: bool) -> bool {
        let tier = self.service_tier().map(str::trim);
        let target = match mode {
            FastMode::Disabled
                if tier.is_some_and(|tier| {
                    tier.eq_ignore_ascii_case("priority") || tier.eq_ignore_ascii_case("fast")
                }) =>
            {
                "default"
            }
            FastMode::Enabled
                if supports_priority
                    && (matches!(self.body.get("service_tier"), None | Some(Value::Null))
                        || tier.is_some_and(|tier| tier.eq_ignore_ascii_case("default"))) =>
            {
                "priority"
            }
            _ => return false,
        };
        self.body
            .insert("service_tier".to_owned(), Value::String(target.to_owned()));
        true
    }

    /// 前一个 response ID
    pub fn previous_response_id(&self) -> Option<&str> {
        self.body
            .get("previous_response_id")
            .and_then(Value::as_str)
    }

    /// 设置 / 清除前一个 response ID
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

    /// 提示缓存键
    pub fn prompt_cache_key(&self) -> Option<&str> {
        self.body.get("prompt_cache_key").and_then(Value::as_str)
    }

    /// client metadata（透传原值）
    pub fn client_metadata(&self) -> Option<&Value> {
        self.body.get("client_metadata")
    }

    /// 提取 Codex 请求类型、子代理类型、推理预设与压缩语义
    pub fn semantics(
        &self,
        previous_reasoning_effort_override: Option<&str>,
    ) -> CodexRequestSemantics {
        let mut semantics = codex_responses_request_semantics_with_turn_metadata(
            self.body(),
            self.turn_metadata.as_deref(),
            previous_reasoning_effort_override,
        );
        // 官方 generate=false 只准备连接与上下文，不是模型推理
        // 预热分类参与用量筛选，不能仅凭客户端的 request_kind 提示排除真实推理
        if !self.generate() {
            semantics.request_kind = Some("prewarm".to_owned());
        } else if semantics.request_kind.as_deref() == Some("prewarm") {
            semantics.request_kind = None;
        }
        semantics
    }

    /// 返回请求语义与 child transport 隔离所用的子代理区分值
    ///
    /// Codex 原生请求在 turn metadata 中声明 `subagent_kind`；兼容客户端也可通过
    /// `client_metadata.x-openai-subagent` 声明同一语义
    /// 它不拆分根会话的账号首选项；
    /// `thread_spawn` 仍用它派生独立 WebSocket/continuation transport identity
    pub fn subagent_kind(&self) -> Option<String> {
        self.semantics(None)
            .subagent_kind
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                self.client_metadata()
                    .and_then(Value::as_object)
                    .and_then(|metadata| metadata.get("x-openai-subagent"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            })
    }

    /// Codex 执行命令前的 Guardian 自动审批请求；客户端以 `guardian` 子代理类型声明
    pub fn is_guardian(&self) -> bool {
        self.subagent_kind().as_deref() == Some(GUARDIAN_SUBAGENT_KIND)
    }

    /// 设置 / 合并 client metadata
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

    /// 替换客户端原本提供的账号身份字段；无法安全重建时删除该字段
    pub fn replace_existing_identity_field(&mut self, key: &str, value: Option<&str>) {
        if !self.body.contains_key(key) {
            return;
        }
        match value {
            Some(value) => {
                self.body
                    .insert(key.to_string(), Value::String(value.to_string()));
            }
            None => {
                self.body.remove(key);
            }
        }
    }
}
