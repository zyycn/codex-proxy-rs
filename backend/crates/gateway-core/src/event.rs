//! Provider 与客户端协议之间唯一的 canonical event 边界。

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Value;
use thiserror::Error;

use crate::accounting::{CalculatedCost, ProviderReportedCost, Usage};
use crate::engine::provider::UpstreamTransport;
use crate::error::{IdentifierError, SafeUpstreamValue, validate_text};
use crate::operation::ProviderSessionState;

/// 一次响应的稳定元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseMeta {
    response_id: String,
    model: String,
    finish_reason: Option<FinishReason>,
    upstream_response_id: Option<SafeUpstreamValue>,
}

impl ResponseMeta {
    /// 创建响应元数据。
    #[must_use]
    pub fn new(response_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            response_id: response_id.into(),
            model: model.into(),
            finish_reason: None,
            upstream_response_id: None,
        }
    }

    /// 设置终止原因。
    #[must_use]
    pub const fn with_finish_reason(mut self, finish_reason: FinishReason) -> Self {
        self.finish_reason = Some(finish_reason);
        self
    }

    /// 返回网关响应 ID。
    #[must_use]
    pub fn response_id(&self) -> &str {
        &self.response_id
    }

    /// 以客户端协议层冻结的网关 response ID 替换 Provider 临时 ID。
    ///
    /// Provider ID 只用于当前 attempt 内校验事件关联；跨请求 continuation 必须
    /// 使用网关拥有且可按调用方隔离解析的 ID。
    #[must_use]
    pub fn with_gateway_response_id(mut self, response_id: impl Into<String>) -> Self {
        self.response_id = response_id.into();
        self
    }

    /// 返回对客户端公开的模型名。
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 返回规范化终止原因。
    #[must_use]
    pub const fn finish_reason(&self) -> Option<FinishReason> {
        self.finish_reason
    }

    /// 附加 adapter 已分类为非 bearer 的上游 response ID。
    #[must_use]
    pub fn with_upstream_response_id(mut self, response_id: SafeUpstreamValue) -> Self {
        self.upstream_response_id = Some(response_id);
        self
    }

    /// 返回仅供 attempt 遥测持久化的安全上游 response ID。
    #[must_use]
    pub const fn upstream_response_id(&self) -> Option<&SafeUpstreamValue> {
        self.upstream_response_id.as_ref()
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
    sse_id: Option<String>,
    sse_retry: Option<u64>,
}

impl ProtocolWireEvent {
    /// 创建协议原生 JSON event。
    ///
    /// # Errors
    ///
    /// 协议名或显式事件名为空、过长或含控制字符时返回错误。
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
    /// 协议名、显式事件名或 SSE ID 不满足 wire 安全约束时返回错误。
    pub fn json_with_sse_metadata(
        protocol: impl Into<String>,
        event_type: Option<String>,
        data: Value,
        sse_id: Option<String>,
        sse_retry: Option<u64>,
    ) -> Result<Self, IdentifierError> {
        let protocol = protocol.into();
        validate_text(&protocol, 64, true, None)?;
        if let Some(event_type) = event_type.as_deref() {
            validate_text(event_type, 256, true, None)?;
        }
        if sse_id
            .as_deref()
            .is_some_and(|id| id.contains(['\0', '\r', '\n']))
        {
            return Err(IdentifierError::ControlCharacter);
        }
        Ok(Self {
            protocol,
            event_type,
            data,
            sse_id,
            sse_retry,
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
}

/// Provider 已筛选、可由协议 adapter 再次按白名单表达的响应头。
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderResponseHeader {
    name: String,
    value: SafeUpstreamValue,
}

impl ProviderResponseHeader {
    /// 创建规范化的小写响应头；非 ASCII token 名称直接拒绝。
    #[must_use]
    pub fn new(name: impl Into<String>, value: SafeUpstreamValue) -> Option<Self> {
        let name = name.into().trim().to_ascii_lowercase();
        let valid = !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        valid.then_some(Self { name, value })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn value(&self) -> &SafeUpstreamValue {
        &self.value
    }
}

impl fmt::Debug for ProviderResponseHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderResponseHeader")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Core 消费的实际上游响应事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResponseObservation {
    transport: UpstreamTransport,
    http_version: Option<UpstreamHttpVersion>,
    status_code: Option<u16>,
    request_id: Option<SafeUpstreamValue>,
    timings: ProviderResponseTimings,
    client_headers: Vec<ProviderResponseHeader>,
}

impl ProviderResponseObservation {
    #[must_use]
    pub fn new(transport: UpstreamTransport) -> Self {
        Self {
            transport,
            http_version: None,
            status_code: None,
            request_id: None,
            timings: ProviderResponseTimings::default(),
            client_headers: Vec::new(),
        }
    }

    #[must_use]
    pub const fn with_http_version(mut self, version: UpstreamHttpVersion) -> Self {
        self.http_version = Some(version);
        self
    }

    #[must_use]
    pub fn with_status_code(mut self, status_code: u16) -> Self {
        self.status_code = (100..=599).contains(&status_code).then_some(status_code);
        self
    }

    #[must_use]
    pub fn with_request_id(mut self, request_id: SafeUpstreamValue) -> Self {
        self.request_id = Some(request_id);
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

    #[must_use]
    pub const fn transport(&self) -> &UpstreamTransport {
        &self.transport
    }

    #[must_use]
    pub const fn http_version(&self) -> Option<UpstreamHttpVersion> {
        self.http_version
    }

    #[must_use]
    pub const fn status_code(&self) -> Option<u16> {
        self.status_code
    }

    #[must_use]
    pub const fn request_id(&self) -> Option<&SafeUpstreamValue> {
        self.request_id.as_ref()
    }

    #[must_use]
    pub const fn timings(&self) -> ProviderResponseTimings {
        self.timings
    }

    #[must_use]
    pub fn client_headers(&self) -> &[ProviderResponseHeader] {
        &self.client_headers
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

    /// 附着只由同协议客户端连接保存、并在下一轮原样交还 Provider 的状态。
    #[must_use]
    pub fn with_session_update(mut self, state: ProviderSessionState) -> Self {
        self.session_update = Some(Box::new(state));
        self
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

    /// 返回 Core 可改写客户端响应身份的全部 facts。
    #[must_use]
    pub fn canonical_facts_mut(&mut self) -> &mut [GatewayEvent] {
        &mut self.canonical
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

    /// 返回是否包含足以冻结下游 commit barrier 的 canonical fact。
    ///
    /// 单独的 wire event 不能冻结边界：协议层在看到 `Started`
    /// 携带的网关响应身份前无法安全编码它。Core 会保留这些
    /// 事件，直到首个 canonical 可见增量或终态再一并交付。
    #[must_use]
    pub fn is_commit_significant(&self) -> bool {
        self.canonical
            .iter()
            .any(GatewayEvent::is_commit_significant)
    }
}

impl From<GatewayEvent> for ProviderEvent {
    fn from(event: GatewayEvent) -> Self {
        Self::canonical(event)
    }
}

impl GatewayEvent {
    /// Canonical event 均为客户端 encoder 可交付事件。
    ///
    /// Attempt Coordinator 会把首个事件标记为 downstream commit 候选，
    /// adapter 可以先编码，但在 commit 持久化成功前不得把结果交给客户端。
    #[must_use]
    pub const fn is_downstream_deliverable(&self) -> bool {
        true
    }

    /// 返回该 fact 是否足以冻结下游 commit barrier。
    ///
    /// 生命周期、结构与结算 facts 可以在换号前安全丢弃；首个可见增量或
    /// Provider 正常终态才会使客户端输出不可撤回。
    #[must_use]
    pub const fn is_commit_significant(&self) -> bool {
        matches!(
            self,
            Self::TextDelta(_)
                | Self::ReasoningDelta(_)
                | Self::ToolCallDelta(_)
                | Self::Completed(_)
        )
    }

    /// 冻结客户端可见的网关 response ID，并返回 Provider 原生 response ID。
    ///
    /// 只有 Coordinator 会调用该边界；Provider 产生的 ID 永远不会直接下发。
    pub(crate) fn freeze_gateway_response_id(
        &mut self,
        gateway_response_id: &str,
    ) -> Result<Option<String>, crate::error::IdentifierError> {
        let metadata = match self {
            Self::Started(metadata) | Self::Completed(metadata) => metadata,
            _ => return Ok(None),
        };
        let upstream_response_id = SafeUpstreamValue::new(metadata.response_id.clone())?;
        let upstream_response_id_text = upstream_response_id.as_str().to_owned();
        metadata.response_id = gateway_response_id.to_owned();
        metadata.upstream_response_id = Some(upstream_response_id);
        Ok(Some(upstream_response_id_text))
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
