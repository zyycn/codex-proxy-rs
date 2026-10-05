//! 路由、调度与重试调用的跨进程数据合同

use serde::{Deserialize, Serialize};

use crate::SendState;

/// `retry` 只继续宿主已选定的恢复路径，不能改账号、延迟或预算
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryAction {
    Stop,
    Retry,
}

/// 重试输入只包含安全事实，不携带上游错误正文、凭据或请求内容
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

/// 策略调用的完整 HTTP 头；值使用 base64 保留非 UTF-8 字节
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyHeader {
    pub name: String,
    pub value_base64: String,
}

/// 模型路由调用；原始正文在 RPC 二进制载荷中独立传递
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

/// 模型路由决定；宿主仍会复核目标、Key 权限和模型能力
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

/// 候选账号的即时调度指标；凭据和账号资料通过宿主账号接口读取
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

/// 账号调度输入；候选账号及即时调度指标由宿主冻结
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

/// 账号调度结果；账号租约始终由宿主在决定返回后取得
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccountScheduleDecision {
    Pick { account_id: String },
    Delegate,
    Reject,
}
