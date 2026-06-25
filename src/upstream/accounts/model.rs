//! 账号领域模型。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 账号当前状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AccountStatus {
    /// 账号可正常对外服务。
    Active,
    /// 访问令牌已经过期，需要重新刷新。
    Expired,
    /// 当前配额已经耗尽。
    QuotaExhausted,
    /// 账号正处于刷新流程中。
    Refreshing,
    /// 账号被显式禁用。
    Disabled,
    /// 账号被上游封禁。
    Banned,
}

impl std::fmt::Display for AccountStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Expired => write!(f, "expired"),
            Self::QuotaExhausted => write!(f, "quota_exhausted"),
            Self::Refreshing => write!(f, "refreshing"),
            Self::Disabled => write!(f, "disabled"),
            Self::Banned => write!(f, "banned"),
        }
    }
}

/// 账号聚合根。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// 账号主键。
    pub id: String,
    /// 展示用邮箱地址。
    pub email: Option<String>,
    /// 上游账号标识。
    pub account_id: Option<String>,
    /// 上游用户标识。
    pub user_id: Option<String>,
    /// 后台自定义标签。
    pub label: Option<String>,
    /// 订阅计划类型。
    pub plan_type: Option<String>,
    /// 当前访问令牌。
    pub access_token: String,
    /// 刷新令牌。
    pub refresh_token: Option<String>,
    /// 访问令牌过期时间。
    pub access_token_expires_at: Option<DateTime<Utc>>,
    /// 下一次允许刷新 token 的时间。
    pub next_refresh_at: Option<DateTime<Utc>>,
    /// 当前账号状态。
    pub status: AccountStatus,
    /// 是否已经触发配额封顶。
    pub quota_limit_reached: bool,
    /// 是否需要执行额外配额校验。
    pub quota_verify_required: bool,
    /// 配额冷却结束时间。
    pub quota_cooldown_until: Option<DateTime<Utc>>,
    /// Cloudflare 冷却结束时间。
    pub cloudflare_cooldown_until: Option<DateTime<Utc>>,
    /// 历史总请求数。
    pub request_count: u64,
    /// 历史空响应次数。
    pub empty_response_count: u64,
    /// 历史图片输入 token 数。
    pub image_input_tokens: u64,
    /// 历史图片输出 token 数。
    pub image_output_tokens: u64,
    /// 历史图片请求数。
    pub image_request_count: u64,
    /// 历史图片请求失败数。
    pub image_request_failed_count: u64,
    /// 当前窗口请求数。
    pub window_request_count: u64,
    /// 当前窗口输入 token 数。
    pub window_input_tokens: u64,
    /// 当前窗口输出 token 数。
    pub window_output_tokens: u64,
    /// 当前窗口缓存 token 数。
    pub window_cached_tokens: u64,
    /// 当前窗口图片输入 token 数。
    pub window_image_input_tokens: u64,
    /// 当前窗口图片输出 token 数。
    pub window_image_output_tokens: u64,
    /// 当前窗口图片请求数。
    pub window_image_request_count: u64,
    /// 当前窗口图片请求失败数。
    pub window_image_request_failed_count: u64,
    /// 当前统计窗口起始时间。
    pub window_started_at: Option<DateTime<Utc>>,
    /// 当前统计窗口重置时间。
    pub window_reset_at: Option<DateTime<Utc>>,
    /// 当前限流窗口大小（秒）。
    pub limit_window_seconds: Option<u64>,
    /// 首次加入时间。
    pub added_at: String,
    /// 最近一次使用时间。
    pub last_used_at: Option<String>,
}

/// 账号用量增量（来自 usage.rs）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountUsageDelta {
    /// 请求数增量。
    pub requests: u64,
    /// 输入 token 增量。
    pub input_tokens: u64,
    /// 输出 token 增量。
    pub output_tokens: u64,
    /// 缓存 token 增量。
    pub cached_tokens: u64,
    /// reasoning token 增量。
    pub reasoning_tokens: u64,
    /// 上游返回的总 token 增量。
    pub total_tokens: u64,
    /// 空响应数增量。
    pub empty_responses: u64,
    /// 图片工具输入 token 增量。
    pub image_input_tokens: u64,
    /// 图片工具输出 token 增量。
    pub image_output_tokens: u64,
    /// 图片工具成功请求数增量。
    pub image_requests: u64,
    /// 图片工具失败请求数增量。
    pub image_request_failures: u64,
}

impl AccountUsageDelta {
    /// 合并两个用量增量。
    pub fn merged(self, other: Self) -> Self {
        Self {
            requests: self.requests + other.requests,
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            cached_tokens: self.cached_tokens + other.cached_tokens,
            reasoning_tokens: self.reasoning_tokens + other.reasoning_tokens,
            total_tokens: self.total_tokens + other.total_tokens,
            empty_responses: self.empty_responses + other.empty_responses,
            image_input_tokens: self.image_input_tokens + other.image_input_tokens,
            image_output_tokens: self.image_output_tokens + other.image_output_tokens,
            image_requests: self.image_requests + other.image_requests,
            image_request_failures: self.image_request_failures + other.image_request_failures,
        }
    }
}
