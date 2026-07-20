//! 多 Provider 账号目录与连接测试的公共事实。

use std::pin::Pin;

use chrono::{DateTime, Utc};
use futures::Stream;

use gateway_core::routing::{ProviderInstanceId, ProviderKind};

use super::{PageSize, Revision};

/// Provider 账号当前可用性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAvailability {
    Unknown,
    Ready,
    Cooldown,
    QuotaExhausted,
    Expired,
    Banned,
    Invalid,
}

/// 管理页使用的归一化账号状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    Active,
    Expired,
    QuotaExhausted,
    Disabled,
    Banned,
    Attention,
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
    pub provider_instance_id: ProviderInstanceId,
    pub provider_kind: ProviderKind,
    pub name: String,
    pub email: Option<String>,
    pub upstream_user_id: String,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    pub credential_revision: Revision,
    pub has_refresh_token: bool,
    pub access_token_expires_at: DateTime<Utc>,
    pub next_refresh_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub availability: AccountAvailability,
    pub availability_reason: Option<String>,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub availability_observed_at: DateTime<Utc>,
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
    pub total_tokens: Option<u64>,
    pub cost_coverage: super::observability::CostCoverage,
    pub costs: Vec<AccountCost>,
    pub last_used_at: DateTime<Utc>,
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
    pub total_tokens: Option<u64>,
    pub cost_coverage: super::observability::CostCoverage,
    pub costs: Vec<AccountCost>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub models: Vec<AccountModelUsage>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountSummary {
    pub total: u64,
    pub active: u64,
    pub quota_exhausted: u64,
    pub attention: u64,
}

/// 账号启停写入命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetAccountEnabled {
    pub expected_config_revision: Revision,
    pub account_id: String,
    pub enabled: bool,
}

/// 账号删除命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteAccount {
    pub expected_config_revision: Revision,
    pub account_id: String,
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
        account_status: AccountStatus,
    },
}

/// 每次连接测试独占的有限事件流。
pub type AccountConnectionTestEventStream =
    Pin<Box<dyn Stream<Item = AccountConnectionTestEvent> + Send + 'static>>;
