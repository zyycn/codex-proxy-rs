//! 账号领域模型。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::infra::json::SortDirection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSortField {
    Email,
    Status,
    PlanType,
    Usage,
    LastUsedAt,
    ExpiresAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountListSort {
    pub field: AccountSortField,
    pub direction: SortDirection,
}

/// 账号当前状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AccountStatus {
    /// 账号可正常对外服务。
    Active,
    /// 访问令牌已经过期，需要重新刷新。
    Expired,
    /// 当前配额已经耗尽。
    QuotaExhausted,
    /// 账号被显式禁用。
    Disabled,
    /// 账号被上游封禁。
    Banned,
}

impl AccountStatus {
    /// 返回账号状态的持久化/API 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Expired => "expired",
            Self::QuotaExhausted => "quota_exhausted",
            Self::Disabled => "disabled",
            Self::Banned => "banned",
        }
    }

    /// 解析持久化/API 使用的账号状态字符串。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "active" => Some(Self::Active),
            "expired" => Some(Self::Expired),
            "quota_exhausted" => Some(Self::QuotaExhausted),
            "disabled" => Some(Self::Disabled),
            "banned" => Some(Self::Banned),
            _ => None,
        }
    }
}

impl std::fmt::Display for AccountStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
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
