//! Provider 与客户端协议之间唯一的 canonical event 边界。

use std::collections::BTreeMap;
use std::fmt;

use bytes::Bytes;
use serde_json::Value;
use thiserror::Error;

use crate::error::{IdentifierError, OpaqueUpstreamValue, validate_text};
use crate::metering::{CalculatedCost, ProviderReportedCost, Usage};
use crate::operation::ProviderSessionState;
use crate::upstream::UpstreamTransport;

/// 一次响应的稳定元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseMeta {
    response_id: String,
    model: Option<String>,
    finish_reason: Option<FinishReason>,
}

impl ResponseMeta {
    /// 创建响应元数据。
    #[must_use]
    pub fn new(response_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            response_id: response_id.into(),
            model: Some(model.into()),
            finish_reason: None,
        }
    }

    /// 创建不声明模型的 Provider 原生端点响应元数据。
    #[must_use]
    pub fn for_provider_endpoint(response_id: impl Into<String>) -> Self {
        Self {
            response_id: response_id.into(),
            model: None,
            finish_reason: None,
        }
    }

    /// 设置终止原因。
    #[must_use]
    pub const fn with_finish_reason(mut self, finish_reason: FinishReason) -> Self {
        self.finish_reason = Some(finish_reason);
        self
    }

    /// 返回客户端可见的 Provider 原生响应 ID。
    #[must_use]
    pub fn response_id(&self) -> &str {
        &self.response_id
    }

    /// 返回对客户端公开的模型名。
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// 返回规范化终止原因。
    #[must_use]
    pub const fn finish_reason(&self) -> Option<FinishReason> {
        self.finish_reason
    }
}

/// 规范化终止原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FinishReason {
    /// 正常停止。
    Stop,
    /// 达到长度限制。
    Length,
    /// 发出了工具调用。
    ToolCall,
    /// 内容策略停止。
    ContentFilter,
    /// Provider 返回了完整但非上述类别的终态。
    Other,
}

/// 输出内容类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ContentKind {
    /// 文本。
    Text,
    /// 推理摘要或推理内容。
    Reasoning,
    /// 工具调用。
    ToolCall,
    /// 图像。
    Image,
    /// 音频。
    Audio,
}

/// 新增的输出内容项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentItem {
    index: u32,
    kind: ContentKind,
}

impl ContentItem {
    /// 创建内容项。
    #[must_use]
    pub const fn new(index: u32, kind: ContentKind) -> Self {
        Self { index, kind }
    }

    /// 返回内容索引。
    #[must_use]
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// 返回内容类别。
    #[must_use]
    pub const fn kind(&self) -> ContentKind {
        self.kind
    }
}

/// 文本增量。
#[derive(Clone, PartialEq, Eq)]
pub struct TextDelta {
    /// 内容索引。
    pub content_index: u32,
    /// 增量正文。
    pub text: String,
}

impl fmt::Debug for TextDelta {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TextDelta")
            .field("content_index", &self.content_index)
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

/// 推理内容增量。
#[derive(Clone, PartialEq, Eq)]
pub struct ReasoningDelta {
    /// 内容索引。
    pub content_index: u32,
    /// 增量正文。
    pub text: String,
}

impl fmt::Debug for ReasoningDelta {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReasoningDelta")
            .field("content_index", &self.content_index)
            .field("text_bytes", &self.text.len())
            .finish()
    }
}

/// 工具调用增量。
#[derive(Clone, PartialEq, Eq)]
pub struct ToolCallDelta {
    /// 内容索引。
    pub content_index: u32,
    /// 稳定 tool call ID。
    pub call_id: String,
    /// 首个增量可携带工具名。
    pub name: Option<String>,
    /// JSON arguments 字符串增量。
    pub arguments_delta: String,
}

impl fmt::Debug for ToolCallDelta {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolCallDelta")
            .field("content_index", &self.content_index)
            .field("call_id", &self.call_id)
            .field("name", &self.name)
            .field("arguments_bytes", &self.arguments_delta.len())
            .finish()
    }
}

/// Provider 返回的协议原生 JSON event。
///
/// Core 不解释 `data`，只把它与同一上游事件产生的 canonical facts 一起
/// 交给客户端协议 adapter。Debug 永不输出正文。
#[derive(Clone, PartialEq)]
pub struct ProtocolWireEvent {
    protocol: String,
    event_type: Option<String>,
    data: Value,
    has_json_data: bool,
    raw_sse_frame: Option<Bytes>,
    raw_json_body: Option<Bytes>,
    sse_id: Option<String>,
    sse_retry: Option<u64>,
}

impl ProtocolWireEvent {
    /// 创建协议原生 JSON event。
    ///
    /// # Errors
    ///
    /// 协议名不满足内部路由标识约束时返回错误。上游事件名保持不透明。
    pub fn json(
        protocol: impl Into<String>,
        event_type: Option<String>,
        data: Value,
    ) -> Result<Self, IdentifierError> {
        Self::json_with_sse_metadata(protocol, event_type, data, None, None)
    }

    /// 创建携带原生 SSE 元数据的协议 JSON event。
    ///
    /// # Errors
    ///
    /// 协议名不满足内部路由标识约束时返回错误。SSE 元数据保持不透明。
    pub fn json_with_sse_metadata(
        protocol: impl Into<String>,
        event_type: Option<String>,
        data: Value,
        sse_id: Option<String>,
        sse_retry: Option<u64>,
    ) -> Result<Self, IdentifierError> {
        let protocol = protocol.into();
        validate_text(&protocol, 64, true, None)?;
        Ok(Self {
            protocol,
            event_type,
            data,
            has_json_data: true,
            raw_sse_frame: None,
            raw_json_body: None,
            sse_id,
            sse_retry,
        })
    }

    /// 创建携带未经改写 SSE 原始帧的协议 JSON event。
    ///
    /// `data` 只供 Core 旁路观测；客户端 SSE 输出必须优先使用 `raw_sse_frame`。
    ///
    /// # Errors
    ///
    /// 与 [`Self::json_with_sse_metadata`] 相同。
    pub fn json_with_raw_sse_metadata(
        protocol: impl Into<String>,
        event_type: Option<String>,
        data: Value,
        raw_sse_frame: Bytes,
        sse_id: Option<String>,
        sse_retry: Option<u64>,
    ) -> Result<Self, IdentifierError> {
        let mut event =
            Self::json_with_sse_metadata(protocol, event_type, data, sse_id, sse_retry)?;
        event.raw_sse_frame = Some(raw_sse_frame);
        Ok(event)
    }

    /// 创建只有原始 SSE 字节、没有可解析 JSON 的协议 event。
    ///
    /// 例如 keep-alive 注释和未知的非 JSON SSE 帧仍须原样透传，但不会参与
    /// WebSocket JSON 输出或旁路观测。
    ///
    /// # Errors
    ///
    /// 协议名不满足 wire 安全约束时返回错误。
    pub fn raw_sse(
        protocol: impl Into<String>,
        raw_sse_frame: Bytes,
    ) -> Result<Self, IdentifierError> {
        let protocol = protocol.into();
        validate_text(&protocol, 64, true, None)?;
        Ok(Self {
            protocol,
            event_type: None,
            data: Value::Null,
            has_json_data: false,
            raw_sse_frame: Some(raw_sse_frame),
            raw_json_body: None,
            sse_id: None,
            sse_retry: None,
        })
    }

    /// 创建未经改写的完整 JSON 响应正文。
    ///
    /// 该载荷用于非流式协议端点；Core 不解析或重编码其中的值。
    ///
    /// # Errors
    ///
    /// 协议名不满足内部路由标识约束时返回错误。
    pub fn raw_json(
        protocol: impl Into<String>,
        raw_json_body: Bytes,
    ) -> Result<Self, IdentifierError> {
        let protocol = protocol.into();
        validate_text(&protocol, 64, true, None)?;
        Ok(Self {
            protocol,
            event_type: None,
            data: Value::Null,
            has_json_data: false,
            raw_sse_frame: None,
            raw_json_body: Some(raw_json_body),
            sse_id: None,
            sse_retry: None,
        })
    }

    /// 返回客户端协议名称。
    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    /// 返回可选的协议事件名称。
    #[must_use]
    pub fn event_type(&self) -> Option<&str> {
        self.event_type.as_deref()
    }

    /// 返回上游 SSE `id` 字段。
    #[must_use]
    pub fn sse_id(&self) -> Option<&str> {
        self.sse_id.as_deref()
    }

    /// 返回上游 SSE `retry` 字段。
    #[must_use]
    pub const fn sse_retry(&self) -> Option<u64> {
        self.sse_retry
    }

    /// 返回协议原生 JSON 数据。
    #[must_use]
    pub const fn data(&self) -> &Value {
        &self.data
    }

    /// 返回该 event 是否携带可供旁路观测的 JSON 数据。
    #[must_use]
    pub const fn has_json_data(&self) -> bool {
        self.has_json_data
    }

    /// 返回可直接写给 SSE 客户端的未经改写原始帧。
    #[must_use]
    pub const fn raw_sse_frame(&self) -> Option<&Bytes> {
        self.raw_sse_frame.as_ref()
    }

    /// 返回可直接写给 JSON 客户端的未经改写响应正文。
    #[must_use]
    pub const fn raw_json_body(&self) -> Option<&Bytes> {
        self.raw_json_body.as_ref()
    }

    /// 拆出未经改写的完整 JSON 响应正文。
    #[must_use]
    pub fn into_raw_json_body(self) -> Option<Bytes> {
        self.raw_json_body
    }

    /// 拆出协议原生 JSON 数据。
    #[must_use]
    pub fn into_data(self) -> Value {
        self.data
    }
}

impl fmt::Debug for ProtocolWireEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProtocolWireEvent")
            .field("protocol", &self.protocol)
            .field("event_type", &self.event_type)
            .field("has_json_data", &self.has_json_data)
            .field("has_raw_sse_frame", &self.raw_sse_frame.is_some())
            .field("has_raw_json_body", &self.raw_json_body.is_some())
            .field("has_sse_id", &self.sse_id.is_some())
            .field("sse_retry", &self.sse_retry)
            .field("data", &"<not included in Debug>")
            .finish()
    }
}

/// 上游响应使用的 HTTP 协议版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamHttpVersion {
    Unknown,
    Http09,
    Http10,
    Http11,
    Http2,
    Http3,
}

impl UpstreamHttpVersion {
    /// 解析 transport 已规范化的协议版本。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "HTTP/0.9" | "0.9" => Some(Self::Http09),
            "HTTP/1.0" | "HTTP/1" | "1.0" => Some(Self::Http10),
            "HTTP/1.1" | "1.1" => Some(Self::Http11),
            "HTTP/2" | "HTTP/2.0" | "2" | "2.0" => Some(Self::Http2),
            "HTTP/3" | "HTTP/3.0" | "3" | "3.0" => Some(Self::Http3),
            _ => None,
        }
    }

    /// 返回数据库使用的稳定名称。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Http09 => "HTTP/0.9",
            Self::Http10 => "HTTP/1.0",
            Self::Http11 => "HTTP/1.1",
            Self::Http2 => "HTTP/2",
            Self::Http3 => "HTTP/3",
        }
    }
}

/// Provider transport 边界测得的阶段耗时。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderResponseTimings {
    pub transport_decision_wait_ms: Option<u64>,
    pub connect_ms: Option<u64>,
    pub headers_ms: Option<u64>,
    pub first_event_ms: Option<u64>,
    pub first_reasoning_ms: Option<u64>,
    pub first_text_ms: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub provider_processing_ms: Option<u64>,
}

/// Provider 已筛选的响应观测 JSON。
///
/// Core 只验证它是有界 JSON object，并在请求终态原样交给存储层；字段语义、
/// 脱敏和版本演进均由所属 Provider 负责。这样 Provider 专有协议不会渗入路由
/// 或管理领域。
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderResponseMetadata(String);

impl ProviderResponseMetadata {
    const MAX_BYTES: usize = 32 * 1024;

    /// 从 Provider 已筛选的 JSON object 创建观测快照。
    #[must_use]
    pub fn new(json: String) -> Option<Self> {
        if json.len() > Self::MAX_BYTES {
            return None;
        }
        serde_json::from_str::<Value>(&json)
            .ok()
            .filter(Value::is_object)
            .map(|_| Self(json))
    }

    /// 返回不透明的 JSON 文本，供持久化 adapter 原样验证和写入。
    #[must_use]
    pub fn as_json(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ProviderResponseMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderResponseMetadata(<provider-owned>)")
    }
}

/// Provider 已筛选、可由协议 adapter 尝试表达的不透明响应头。
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderResponseHeader {
    name: String,
    value: Bytes,
}

impl ProviderResponseHeader {
    /// 保存 Provider 交付的原始名称和值；HTTP 可表达性由最终 adapter 判断。
    #[must_use]
    pub fn new(name: impl Into<String>, value: Bytes) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn value(&self) -> &Bytes {
        &self.value
    }
}

impl fmt::Debug for ProviderResponseHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderResponseHeader")
            .field("name", &"<redacted>")
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Core 消费的实际上游响应事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResponseObservation {
    transport: UpstreamTransport,
    http_version: Option<UpstreamHttpVersion>,
    websocket_pool: Option<WebSocketPoolKind>,
    status_code: Option<u16>,
    request_id: Option<OpaqueUpstreamValue>,
    service_tier: Option<String>,
    timings: ProviderResponseTimings,
    client_headers: Vec<ProviderResponseHeader>,
    provider_metadata: Option<ProviderResponseMetadata>,
}

impl ProviderResponseObservation {
    #[must_use]
    pub fn new(transport: UpstreamTransport) -> Self {
        Self {
            transport,
            http_version: None,
            websocket_pool: None,
            status_code: None,
            request_id: None,
            service_tier: None,
            timings: ProviderResponseTimings::default(),
            client_headers: Vec::new(),
            provider_metadata: None,
        }
    }

    #[must_use]
    pub const fn with_http_version(mut self, version: UpstreamHttpVersion) -> Self {
        self.http_version = Some(version);
        self
    }

    #[must_use]
    pub const fn with_websocket_pool(mut self, kind: WebSocketPoolKind) -> Self {
        self.websocket_pool = Some(kind);
        self
    }

    #[must_use]
    pub fn with_status_code(mut self, status_code: u16) -> Self {
        self.status_code = (100..=599).contains(&status_code).then_some(status_code);
        self
    }

    #[must_use]
    pub fn with_request_id(mut self, request_id: OpaqueUpstreamValue) -> Self {
        self.request_id = Some(request_id);
        self
    }

    /// 附加 Provider 在响应中确认的服务档位。
    ///
    /// # Errors
    ///
    /// 档位为空、超过 64 字节或包含控制字符时返回错误。
    pub fn try_with_service_tier(
        mut self,
        service_tier: impl Into<String>,
    ) -> Result<Self, IdentifierError> {
        let service_tier = service_tier.into();
        validate_text(&service_tier, 64, false, None)?;
        self.service_tier = Some(service_tier);
        Ok(self)
    }

    /// 附加合法服务档位；不适合持久化的上游观测值会被忽略。
    #[must_use]
    pub fn with_service_tier_if_valid(mut self, service_tier: impl Into<String>) -> Self {
        let service_tier = service_tier.into();
        if validate_text(&service_tier, 64, false, None).is_ok() {
            self.service_tier = Some(service_tier);
        }
        self
    }

    #[must_use]
    pub const fn with_timings(mut self, timings: ProviderResponseTimings) -> Self {
        self.timings = timings;
        self
    }

    #[must_use]
    pub fn with_client_headers(mut self, client_headers: Vec<ProviderResponseHeader>) -> Self {
        self.client_headers = client_headers;
        self
    }

    /// 附加由 Provider 自己定义且已筛选的响应观测快照。
    #[must_use]
    pub fn with_provider_metadata(mut self, metadata: ProviderResponseMetadata) -> Self {
        self.provider_metadata = Some(metadata);
        self
    }

    #[must_use]
    pub const fn transport(&self) -> &UpstreamTransport {
        &self.transport
    }

    #[must_use]
    pub const fn http_version(&self) -> Option<UpstreamHttpVersion> {
        self.http_version
    }

    #[must_use]
    pub const fn websocket_pool(&self) -> Option<WebSocketPoolKind> {
        self.websocket_pool
    }

    #[must_use]
    pub const fn status_code(&self) -> Option<u16> {
        self.status_code
    }

    #[must_use]
    pub const fn request_id(&self) -> Option<&OpaqueUpstreamValue> {
        self.request_id.as_ref()
    }

    #[must_use]
    pub fn service_tier(&self) -> Option<&str> {
        self.service_tier.as_deref()
    }

    #[must_use]
    pub const fn timings(&self) -> ProviderResponseTimings {
        self.timings
    }

    #[must_use]
    pub fn client_headers(&self) -> &[ProviderResponseHeader] {
        &self.client_headers
    }

    /// 返回 Provider 专有的安全观测 JSON；Core 不读取其字段。
    #[must_use]
    pub const fn provider_metadata(&self) -> Option<&ProviderResponseMetadata> {
        self.provider_metadata.as_ref()
    }
}

/// WebSocket 请求使用新连接或复用池中连接的事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebSocketPoolKind {
    New,
    Reuse,
}

impl WebSocketPoolKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Reuse => "reuse",
        }
    }
}

/// 所有 Provider 都必须输出的稳定事件集合。
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum GatewayEvent {
    /// 响应开始；必须是首事件。
    Started(ResponseMeta),
    /// 声明一个后续可增量写入的内容项。
    ContentAdded(ContentItem),
    /// 文本增量。
    TextDelta(TextDelta),
    /// 推理增量。
    ReasoningDelta(ReasoningDelta),
    /// 工具调用增量。
    ToolCallDelta(ToolCallDelta),
    /// 规范化用量。
    Usage(Usage),
    /// Provider 域依据实际模型和终态用量计算的费用。
    CalculatedCost(CalculatedCost),
    /// Provider 最终 usage chunk 上报的实际已计费总额；同 attempt 最新值覆盖旧值。
    ProviderCost(ProviderReportedCost),
    /// 响应完成；必须是末事件。
    Completed(ResponseMeta),
}

/// Provider 单个上游事件产生的事实与可选协议原生表达。
///
/// 一个值至少包含一个 canonical fact 或一条 wire event。把同一 wire event
/// 产生的多个 canonical facts 放在同一封套，可避免客户端重复收到该事件。
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderEvent {
    canonical: Vec<GatewayEvent>,
    wire: Option<Box<ProtocolWireEvent>>,
    observation: Option<Box<ProviderResponseObservation>>,
    session_update: Option<Box<ProviderSessionState>>,
}

impl ProviderEvent {
    /// 创建只有一个 canonical fact 的事件。
    #[must_use]
    pub fn canonical(event: GatewayEvent) -> Self {
        Self {
            canonical: vec![event],
            wire: None,
            observation: None,
            session_update: None,
        }
    }

    /// 创建只有协议原生表达的事件。
    #[must_use]
    pub fn wire(wire: ProtocolWireEvent) -> Self {
        Self {
            canonical: Vec::new(),
            wire: Some(Box::new(wire)),
            observation: None,
            session_update: None,
        }
    }

    /// 创建同一上游事件的 canonical facts 与协议原生表达。
    #[must_use]
    pub fn canonical_with_wire(canonical: Vec<GatewayEvent>, wire: ProtocolWireEvent) -> Self {
        Self {
            canonical,
            wire: Some(Box::new(wire)),
            observation: None,
            session_update: None,
        }
    }

    /// 创建仅供 Core 消费的上游响应观察；该事件不会进入客户端 adapter。
    #[must_use]
    pub fn observation(observation: ProviderResponseObservation) -> Self {
        Self {
            canonical: Vec::new(),
            wire: None,
            observation: Some(Box::new(observation)),
            session_update: None,
        }
    }

    /// 把本事件标记为 Provider 连接内状态的提交边界。
    pub fn attach_session_update(&mut self, state: ProviderSessionState) {
        self.session_update = Some(Box::new(state));
    }

    /// 返回 Provider 私有的连接内状态更新。
    #[must_use]
    pub fn session_update(&self) -> Option<&ProviderSessionState> {
        self.session_update.as_deref()
    }

    /// 取出 Provider 连接内状态更新，交给协议连接持有。
    #[must_use]
    pub fn take_session_update(&mut self) -> Option<ProviderSessionState> {
        self.session_update.take().map(|state| *state)
    }

    /// 返回 Core 可解释的全部 facts。
    #[must_use]
    pub fn canonical_facts(&self) -> &[GatewayEvent] {
        &self.canonical
    }

    /// 返回协议原生表达。
    #[must_use]
    pub fn wire_event(&self) -> Option<&ProtocolWireEvent> {
        self.wire.as_deref()
    }

    /// 取出仅供 Core 持久化的响应观察。
    #[must_use]
    pub fn take_observation(&mut self) -> Option<ProviderResponseObservation> {
        self.observation.take().map(|observation| *observation)
    }

    /// 返回仅供 Core 持久化的响应观察。
    #[must_use]
    pub fn response_observation(&self) -> Option<&ProviderResponseObservation> {
        self.observation.as_deref()
    }

    /// 拆分 canonical facts 与协议原生表达。
    #[must_use]
    pub fn into_parts(self) -> (Vec<GatewayEvent>, Option<ProtocolWireEvent>) {
        (self.canonical, self.wire.map(|wire| *wire))
    }

    /// 返回是否含有可用于 commit barrier 的 canonical fact。
    #[must_use]
    pub fn has_canonical_facts(&self) -> bool {
        !self.canonical.is_empty()
    }

    /// 返回该封套是否仍包含可交付客户端的表达。
    #[must_use]
    pub fn has_client_event(&self) -> bool {
        !self.canonical.is_empty() || self.wire.is_some()
    }
}

impl From<GatewayEvent> for ProviderEvent {
    fn from(event: GatewayEvent) -> Self {
        Self::canonical(event)
    }
}

/// Canonical event 顺序错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EventSequenceError {
    /// 首事件不是 `Started`。
    #[error("canonical stream must start with Started")]
    MissingStarted,
    /// `Started` 重复。
    #[error("canonical stream contains more than one Started event")]
    DuplicateStarted,
    /// 内容索引重复。
    #[error("canonical stream adds content index {index} more than once")]
    DuplicateContent {
        /// 重复索引。
        index: u32,
    },
    /// Delta 引用了不存在或类别错误的内容项。
    #[error("canonical delta does not match content index {index}")]
    InvalidDeltaTarget {
        /// 内容索引。
        index: u32,
    },
    /// `Completed` 重复或其后仍有事件。
    #[error("canonical stream emitted an event after Completed")]
    EventAfterCompleted,
    /// Stream 未产生 `Completed` 就结束。
    #[error("canonical stream ended before Completed")]
    MissingCompleted,
}

/// 增量校验 canonical event 顺序的轻量状态机。
#[derive(Debug, Default)]
pub struct EventSequenceValidator {
    started: bool,
    completed: bool,
    content: BTreeMap<u32, ContentKind>,
}

impl EventSequenceValidator {
    /// 创建空校验器。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            started: false,
            completed: false,
            content: BTreeMap::new(),
        }
    }

    /// 校验一个事件并推进状态。
    ///
    /// # Errors
    ///
    /// 事件顺序或 delta 目标不满足 canonical contract 时返回错误。
    pub fn observe(&mut self, event: &GatewayEvent) -> Result<(), EventSequenceError> {
        if self.completed {
            return Err(EventSequenceError::EventAfterCompleted);
        }
        if !self.started && !matches!(event, GatewayEvent::Started(_)) {
            return Err(EventSequenceError::MissingStarted);
        }

        match event {
            GatewayEvent::Started(_) => {
                if self.started {
                    return Err(EventSequenceError::DuplicateStarted);
                }
                self.started = true;
            }
            GatewayEvent::ContentAdded(item) => {
                if self.content.insert(item.index(), item.kind()).is_some() {
                    return Err(EventSequenceError::DuplicateContent {
                        index: item.index(),
                    });
                }
            }
            GatewayEvent::TextDelta(delta) => {
                self.require_content(delta.content_index, ContentKind::Text)?;
            }
            GatewayEvent::ReasoningDelta(delta) => {
                self.require_content(delta.content_index, ContentKind::Reasoning)?;
            }
            GatewayEvent::ToolCallDelta(delta) => {
                self.require_content(delta.content_index, ContentKind::ToolCall)?;
            }
            GatewayEvent::Usage(_)
            | GatewayEvent::CalculatedCost(_)
            | GatewayEvent::ProviderCost(_) => {}
            GatewayEvent::Completed(_) => {
                self.completed = true;
            }
        }
        Ok(())
    }

    /// 校验 stream 是否以 `Completed` 正常结束。
    ///
    /// # Errors
    ///
    /// 未开始或未完成时返回错误。
    pub fn finish(&self) -> Result<(), EventSequenceError> {
        if !self.started {
            return Err(EventSequenceError::MissingStarted);
        }
        if !self.completed {
            return Err(EventSequenceError::MissingCompleted);
        }
        Ok(())
    }

    fn require_content(&self, index: u32, expected: ContentKind) -> Result<(), EventSequenceError> {
        if self.content.get(&index) == Some(&expected) {
            Ok(())
        } else {
            Err(EventSequenceError::InvalidDeltaTarget { index })
        }
    }
}
