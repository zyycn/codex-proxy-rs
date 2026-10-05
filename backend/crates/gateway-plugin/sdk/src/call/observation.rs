//! 请求完成与实际上游 WebSocket 事件的统一观察合同

use serde::{Deserialize, Serialize};

use crate::SendState;

/// 订阅决定投递哪些事件，不裁剪事件中的字段
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    RequestCompleted,
    #[serde(rename = "websocket_response")]
    WebSocketResponse,
}

/// 元数据在 RPC 参数中传递；WebSocket 原始帧独立放在二进制载荷中
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "event",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Event {
    RequestCompleted(Box<RequestCompleted>),
    #[serde(rename = "websocket_response")]
    WebSocketResponse(Box<WebSocketResponse>),
}

impl Event {
    #[must_use]
    pub fn request_id(&self) -> &str {
        match self {
            Self::RequestCompleted(event) => &event.request_id,
            Self::WebSocketResponse(event) => &event.request_id,
        }
    }
}

/// 请求终态分类；`rejected` 表示请求在进入 Provider 执行前被宿主拒绝
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestOutcome {
    Succeeded,
    Failed,
    Rejected,
    Cancelled,
    Incomplete,
}

/// Core 对单次请求最终费用的可信程度
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestCostStatus {
    Known,
    Unknown,
}

/// 最终费用事实的来源；观察插件只消费结果，不重新计价
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestCostSource {
    ProviderReported,
    Calculated,
    Unavailable,
}

/// 精确十进制金额；字符串保留宿主的定点精度
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestMoney {
    pub amount: String,
    pub currency: String,
}

/// 用量观察取得的最终费用；未知费用没有 `total`，不得解释为零
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestCost {
    pub status: RequestCostStatus,
    pub source: RequestCostSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<RequestMoney>,
}

/// 用量观察取得的毫秒级阶段耗时；缺失字段表示宿主没有该项事实
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestTimings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_decision_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_event_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_reasoning_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_text_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_processing_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}

/// 非成功请求的最终分类、发送状态与重试事实
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestFailure {
    pub outcome: RequestOutcome,
    pub send_state: SendState,
    pub attempt_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_status_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_status_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

/// 请求生命周期观察取得的最终宿主事实
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestTerminal {
    pub outcome: RequestOutcome,
    pub send_state: SendState,
    pub attempt_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_status_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// 用量观察取得的最终标准化用量；缺失字段表示宿主没有该项事实
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<RequestCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<RequestTimings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RequestFailure>,
}

/// 一次完成事件包含宿主已确认的全部终态、用量、费用与耗时
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestCompleted {
    /// 不重投时仍保持稳定，供插件自行去重和关联日志
    pub event_id: String,
    pub request_id: String,
    pub config_revision: u64,
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// 本次实际选中账号使用的上游模型，未进入上游时可能不存在
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    /// 上游响应报告的模型，与请求中的模型名称分开保存
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub completed_at_ms: u64,
    pub terminal: RequestTerminal,
    pub usage: RequestUsage,
}

/// 实际上游 WebSocket 响应事件
///
/// 原始 JSON 在 RPC 二进制载荷中独立传递；`payload_included` 表示本次
/// 事件是否包含原始字节，元数据不能替代原始载荷
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSocketResponse {
    pub event_id: String,
    pub request_id: String,
    pub config_revision: u64,
    pub operation: String,
    pub protocol: String,
    pub provider: String,
    pub attempt_index: u32,
    /// 请求内从 1 开始单调递增，切换 attempt 时不重置；有界投递可能产生间隔
    pub sequence: u64,
    pub payload_included: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    /// 实际 attempt 选择的账号
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// 事件名属于原始响应内容，只在 `payload_included=true` 时提供
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
}
