//! Provider 与客户端协议之间唯一的 canonical event 边界。

use std::collections::BTreeMap;
use std::fmt;

use thiserror::Error;

use crate::accounting::{CalculatedCost, ProviderReportedCost, Usage};
use crate::error::SafeUpstreamValue;

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

impl GatewayEvent {
    /// Canonical event 均为客户端 encoder 可交付事件。
    ///
    /// Attempt Coordinator 会把首个事件标记为 downstream commit 候选，
    /// adapter 可以先编码，但在 commit 持久化成功前不得把结果交给客户端。
    #[must_use]
    pub const fn is_downstream_deliverable(&self) -> bool {
        true
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
