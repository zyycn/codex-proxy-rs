//! 多 Provider 账号目录与连接测试的公共事实。

use std::{pin::Pin, time::SystemTime};

use chrono::{DateTime, Utc};
use futures::Stream;

use gateway_core::routing::ProviderKind;

use super::{PageSize, Revision, observability::TimeRange};

/// Provider 账号当前可用性；权威定义与字符串编解码在 gateway-core，此处仅复用。
pub use gateway_core::engine::credential::AccountAvailability;

/// 管理页使用的归一化账号状态。
///
/// 只回答两个问题：是否可调度、是否需要人工干预。具体错误原因不进入此枚举，
/// 由 [`AccountErrorReason`] 承载；新增原因只扩展 reason，不改变分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    /// 正常，可调度。
    Normal,
    /// 配额耗尽（持久化终态），等待额度恢复，无需干预。
    QuotaExhausted,
    /// 限流中（quota 快照级信号），自动恢复，无需干预。
    RateLimited,
    /// 管理员手动停用（`enabled = false`）。
    Disabled,
    /// 错误（过期 / 失效 / 封禁等），需要人工干预。具体原因见 [`AccountErrorReason`]。
    Error,
}

impl AccountStatus {
    /// 返回稳定 wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::QuotaExhausted => "quota_exhausted",
            Self::RateLimited => "rate_limited",
            Self::Disabled => "disabled",
            Self::Error => "error",
        }
    }

    /// 解析稳定 wire 值。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(Self::Normal),
            "quota_exhausted" => Some(Self::QuotaExhausted),
            "rate_limited" => Some(Self::RateLimited),
            "disabled" => Some(Self::Disabled),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// 状态派生所需的最小事实；由 store / use_case 各自构造，派生规则唯一。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountStatusSignals {
    pub enabled: bool,
    pub availability: AccountAvailability,
    pub access_token_expired: bool,
    pub quota_limit_reached: bool,
    /// 429 临时限流（Redis 冷却）到期时间；`Some` 表示账号当前处于临时限流。
    pub rate_limited_until: Option<SystemTime>,
}

impl AccountStatusSignals {
    /// 派生产出运营分类。终态优先于瞬时信号：停用 > 配额耗尽 > 错误 > 限流。
    #[must_use]
    pub const fn derive(self) -> AccountStatus {
        if !self.enabled {
            AccountStatus::Disabled
        } else if matches!(self.availability, AccountAvailability::QuotaExhausted) {
            AccountStatus::QuotaExhausted
        } else if self.access_token_expired
            || matches!(
                self.availability,
                AccountAvailability::Expired
                    | AccountAvailability::Invalid
                    | AccountAvailability::Banned
                    | AccountAvailability::Unknown
            )
        {
            AccountStatus::Error
        } else if self.rate_limited_until.is_some() {
            AccountStatus::RateLimited
        } else if self.quota_limit_reached {
            AccountStatus::QuotaExhausted
        } else {
            AccountStatus::Normal
        }
    }
}

/// 错误分类下的具体原因；仅在 `AccountStatus::Error` 时存在。
///
/// 受控枚举用于展示与聚合，扩展不改变状态分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountErrorReason {
    Expired,
    TokenExpired,
    Invalid,
    Banned,
    Unknown,
}

impl AccountErrorReason {
    /// 返回稳定 wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::TokenExpired => "token_expired",
            Self::Invalid => "invalid",
            Self::Banned => "banned",
            Self::Unknown => "unknown",
        }
    }

    /// 解析稳定 wire 值。
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "expired" => Some(Self::Expired),
            "token_expired" => Some(Self::TokenExpired),
            "invalid" => Some(Self::Invalid),
            "banned" => Some(Self::Banned),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

/// 由信号派生错误原因；非错误分类（正常 / 配额耗尽 / 限流 / 停用）返回 `None`。
///
/// 与 [`AccountStatusSignals::derive`] 保持同一判定：`Error` 当且仅当此处返回 `Some`。
#[must_use]
pub fn derive_error_reason(signals: &AccountStatusSignals) -> Option<AccountErrorReason> {
    if !signals.enabled {
        return None;
    }
    match signals.availability {
        AccountAvailability::QuotaExhausted => None,
        AccountAvailability::Ready => signals
            .access_token_expired
            .then_some(AccountErrorReason::TokenExpired),
        AccountAvailability::Expired => Some(AccountErrorReason::Expired),
        AccountAvailability::Invalid => Some(AccountErrorReason::Invalid),
        AccountAvailability::Banned => Some(AccountErrorReason::Banned),
        AccountAvailability::Unknown => Some(AccountErrorReason::Unknown),
    }
}

/// 账号列表排序字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSortField {
    Email,
    Status,
    PlanType,
    Usage,
    LastUsedAt,
    ExpiresAt,
}

/// 账号列表排序方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

/// 一组完整的账号排序规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountSort {
    pub field: AccountSortField,
    pub direction: SortDirection,
}

/// 账号列表的存储查询条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountListQuery {
    pub page: u32,
    pub page_size: PageSize,
    pub provider_kind: Option<ProviderKind>,
    pub search: Option<String>,
    pub status: Option<AccountStatus>,
    pub sort: Option<AccountSort>,
}

/// 账号公共存储投影；Provider 专属字段不进入此结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRecord {
    pub id: String,
    pub provider_kind: ProviderKind,
    pub name: String,
    pub email: Option<String>,
    pub upstream_user_id: Option<String>,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    pub authentication_kind: String,
    pub credential_revision: Revision,
    pub has_refresh_token: bool,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub next_refresh_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub availability: AccountAvailability,
    pub availability_observed_at: DateTime<Utc>,
    /// 最近一次非 ready 状态变更的原始原因（上游错误原文 / 原因码）；恢复 ready 后清空。
    pub last_error_message: Option<String>,
    pub quota_observed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 单一货币的账号成本聚合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountCost {
    pub currency: String,
    pub amount: super::observability::DecimalAmount,
}

/// 账号在一个模型上的历史用量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountModelUsage {
    pub model: String,
    pub request_count: u64,
    pub success_count: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub image_input_tokens: Option<u64>,
    pub image_output_tokens: Option<u64>,
    pub image_request_count: u64,
    pub image_request_failed_count: u64,
    pub total_tokens: Option<u64>,
    pub cost_coverage: super::observability::CostCoverage,
    pub costs: Vec<AccountCost>,
    pub last_used_at: DateTime<Utc>,
}

/// 账号在一个小时窗口内的请求数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRequestBucket {
    pub bucket_start: DateTime<Utc>,
    pub request_count: u64,
}

/// 账号历史用量聚合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsage {
    pub account_id: String,
    pub request_count: u64,
    pub success_count: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub image_input_tokens: Option<u64>,
    pub image_output_tokens: Option<u64>,
    pub image_request_count: u64,
    pub image_request_failed_count: u64,
    pub total_tokens: Option<u64>,
    pub cost_coverage: super::observability::CostCoverage,
    pub costs: Vec<AccountCost>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub request_buckets: Vec<AccountRequestBucket>,
    pub models: Vec<AccountModelUsage>,
}

/// 某个账号在调用方指定时间窗口内的本地用量查询。
///
/// `key` 只用于把聚合结果关联回调用方的窗口，不承载 Provider 私有语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsageWindowQuery {
    pub account_id: String,
    pub key: String,
    pub range: TimeRange,
}

/// 一个账号时间窗口用量查询的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUsageWindowResult {
    pub account_id: String,
    pub key: String,
    pub usage: AccountUsage,
}

/// 账号列表页所需的完整存储事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountPage {
    pub config_revision: Revision,
    pub items: Vec<AccountRecord>,
    pub total: u64,
    pub summary: AccountSummary,
}

/// 统一账号目录的全局状态计数，不受当前筛选和分页影响。
///
/// 计数与 [`AccountStatus`] 一一对应，由 store 按派生状态聚合。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountSummary {
    pub total: u64,
    pub normal: u64,
    pub quota_exhausted: u64,
    pub rate_limited: u64,
    pub disabled: u64,
    pub error: u64,
}

/// 账号启停写入命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetAccountEnabled {
    pub account_id: String,
    pub enabled: bool,
}

/// 账号批量删除命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteAccounts {
    pub account_ids: Vec<String>,
}

/// 账号连接测试的语义事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountConnectionTestEvent {
    Started {
        model: String,
    },
    Request {
        model: String,
        input_text: String,
        stream: bool,
        store: bool,
    },
    Content {
        text: String,
    },
    Completed {
        account_status: AccountStatus,
    },
    Failed {
        message: String,
        provider_error_code: Option<String>,
        provider_error_type: Option<String>,
        upstream_status: Option<u16>,
        upstream_content_type: Option<String>,
        upstream_body: Option<String>,
        account_status: AccountStatus,
    },
}

/// 每次连接测试独占的有限事件流。
pub type AccountConnectionTestEventStream =
    Pin<Box<dyn Stream<Item = AccountConnectionTestEvent> + Send + 'static>>;
