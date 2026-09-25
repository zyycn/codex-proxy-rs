//! 路由、调度与最终观测调用的跨进程数据合同。

use serde::{Deserialize, Serialize};

use crate::SendState;

/// `retry` 只继续宿主已选定的恢复路径，不能改账号、延迟或预算。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryAction {
    Stop,
    Retry,
}

/// 重试输入只包含安全事实，不携带上游错误正文、凭据或请求内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryDecisionRequest {
    pub request_id: String,
    pub attempt_index: u32,
    pub provider: String,
    pub model: Option<String>,
    pub error_kind: String,
    pub upstream_status: Option<u16>,
    pub send_state: SendState,
    pub remaining_routing_attempts: u32,
    pub remaining_deadline_ms: u64,
    pub allowed_actions: Vec<RetryAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetryDecision {
    Delegate,
    Stop,
    Retry,
}

/// 策略插件可见的安全 HTTP 头；值使用 base64 保留非 UTF-8 字节。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyHeader {
    pub name: String,
    pub value_base64: String,
}

/// 模型路由调用。原始正文在 RPC 二进制载荷中独立传递；无正文读取授权时载荷为空。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRouteRequest {
    pub request_id: String,
    pub operation: String,
    pub protocol: String,
    pub model: String,
    #[serde(default)]
    pub available_providers: Vec<String>,
    #[serde(default)]
    pub headers: Vec<PolicyHeader>,
}

/// 模型路由决定；宿主仍会复核目标、Key 权限和模型能力。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelRouteDecision {
    Unhandled,
    Route {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Reject,
}

/// 账号调度器可见的安全候选事实，不包含 credential 或账号资料。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountScheduleCandidate {
    pub account_id: String,
    pub weight: u16,
    pub in_flight: u32,
    pub maximum_concurrency: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_reset_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_remaining_rank: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_rate_basis_points: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_output_latency_ms: Option<u64>,
}

/// 账号调度调用。Key 与账号组仅供宿主匹配绑定，不发送给插件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountScheduleRequest {
    pub request_id: String,
    pub attempt_index: u32,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub candidates: Vec<AccountScheduleCandidate>,
}

/// 账号调度结果；账号租约始终由宿主在决定返回后取得。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccountScheduleDecision {
    Pick { account_id: String },
    Delegate,
    Reject,
}

/// 请求终态分类；`rejected` 表示请求在进入 Provider 执行前被宿主拒绝。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestOutcome {
    Succeeded,
    Failed,
    Rejected,
    Cancelled,
    Incomplete,
}

/// Core 对单次请求最终费用的可信程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestCostStatus {
    Known,
    Unknown,
}

/// 最终费用事实的来源；Usage 插件只消费结果，不重新计价。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestCostSource {
    ProviderReported,
    Calculated,
    Unavailable,
}

/// 精确十进制金额；字符串保留宿主的定点精度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestMoney {
    pub amount: String,
    pub currency: String,
}

/// 用量观察取得的最终费用；未知费用没有 `total`，不得解释为零。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestCost {
    pub status: RequestCostStatus,
    pub source: RequestCostSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<RequestMoney>,
}

/// 用量观察取得的毫秒级阶段耗时；缺失字段表示宿主没有该项事实。
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

/// 非成功请求的安全摘要，不包含错误消息、原始正文或诊断载荷。
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

/// 请求生命周期观察取得的最终宿主事实。
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

/// 用量观察取得的最终标准化用量；缺失字段表示宿主没有该项事实。
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

/// 同一实例的请求生命周期与用量订阅合并为一个最终观测调用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserveRequest {
    /// 不重投时仍保持稳定，供插件自行去重和关联日志。
    pub event_id: String,
    pub request_id: String,
    pub config_revision: u64,
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// 本次实际选中账号使用的上游模型，未进入上游时可能不存在。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    /// 上游响应报告的模型，与请求中的模型名称分开保存。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub completed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<RequestTerminal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<RequestUsage>,
}
