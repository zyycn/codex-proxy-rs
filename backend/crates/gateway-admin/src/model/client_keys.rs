//! Client API Key 的 Command、Result 与安全秘密类型

use std::{collections::BTreeMap, fmt, num::NonZeroU16};

use chrono::{DateTime, Utc};

use gateway_core::{
    engine::budget::{ClientBudgetLimits, ClientBudgetStatus},
    policy::{ClientApiKeyId, PlaintextClientApiKey, RateLimits},
    routing::{AccountGroupId, ProviderKind},
};

use super::{
    AdminModelError, Revision, account_groups::AccountGroupRef,
    plugin_resources::PluginResourceOwner,
};

pub type ProviderRequestProfileOverrides =
    BTreeMap<ProviderKind, gateway_core::account::OpaqueProviderData>;
pub type ProviderRequestProfileOverrideUpdates =
    BTreeMap<ProviderKind, Option<gateway_core::account::OpaqueProviderData>>;

/// Client Key 关联分组的数量与唯一性约束
pub fn validate_group_ids(group_ids: &[AccountGroupId]) -> Result<(), AdminModelError> {
    if group_ids.len() > 1000
        || group_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != group_ids.len()
    {
        return Err(AdminModelError::InvalidClientKeyGroups);
    }
    Ok(())
}

/// Client Key 列表保持旧 HTTP 合同允许的完整非零 `u16` 页大小
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientKeyPageSize(NonZeroU16);

impl ClientKeyPageSize {
    /// 创建 1 至 65535 的 Client Key 页大小
    ///
    /// # Errors
    ///
    /// `value` 为零时返回 [`AdminModelError::InvalidClientKeyPageSize`]
    pub fn new(value: u16) -> Result<Self, AdminModelError> {
        NonZeroU16::new(value)
            .map(Self)
            .ok_or(AdminModelError::InvalidClientKeyPageSize)
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// Client Key 列表排序字段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKeySortField {
    Name,
    Enabled,
    CreatedAt,
    LastUsedAt,
}

/// Client Key 列表排序方向
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

/// Client Key 列表排序规则
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientKeySort {
    pub field: ClientKeySortField,
    pub direction: SortDirection,
}

/// 与排序字段绑定的游标值
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientKeyCursorValue {
    Name(String),
    Enabled(bool),
    CreatedAt(DateTime<Utc>),
    LastUsedAt(Option<DateTime<Utc>>),
}

/// Client Key 的稳定键集游标
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKeyCursor {
    pub sort: ClientKeySort,
    pub value: ClientKeyCursorValue,
    pub id: ClientApiKeyId,
}

/// Client Key 列表查询
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKeyListQuery {
    pub cursor: Option<ClientKeyCursor>,
    pub page_size: ClientKeyPageSize,
    pub search: Option<String>,
    pub sort: ClientKeySort,
}

/// 不含完整明文 Key 的管理投影
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKeyRecord {
    pub peer_sharing: gateway_core::policy::ClientPeerSharing,
    pub request_profile_overrides: ProviderRequestProfileOverrides,
    pub id: ClientApiKeyId,
    pub name: String,
    pub label: Option<String>,
    pub groups: Vec<AccountGroupRef>,
    pub provider_kinds: Vec<ProviderKind>,
    pub prefix: String,
    pub enabled: bool,
    pub limits: RateLimits,
    pub budget: ClientBudgetStatus,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Client Key 列表页
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKeyPage {
    pub config_revision: Revision,
    pub items: Vec<ClientKeyRecord>,
    pub total: u64,
    pub next_cursor: Option<ClientKeyCursor>,
}

/// 仅在创建或显式读取密钥配置时返回的明文 Key；Debug 隐去明文
#[derive(Clone, PartialEq, Eq)]
pub struct ClientKeySecret {
    pub record: ClientKeyRecord,
    plaintext: String,
}

impl ClientKeySecret {
    #[must_use]
    pub fn new(record: ClientKeyRecord, plaintext: impl Into<String>) -> Self {
        Self {
            record,
            plaintext: plaintext.into(),
        }
    }

    #[must_use]
    pub fn expose_for_response(&self) -> &str {
        &self.plaintext
    }
}

impl fmt::Debug for ClientKeySecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientKeySecret")
            .field("record", &self.record)
            .field("plaintext", &"[REDACTED]")
            .finish()
    }
}

/// API 提交的 Client Key 创建命令
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateClientKey {
    pub peer_sharing: gateway_core::policy::ClientPeerSharing,
    pub request_profile_overrides: ProviderRequestProfileOverrides,
    pub custom_key: Option<PlaintextClientApiKey>,
    pub name: String,
    pub label: Option<String>,
    pub group_ids: Vec<AccountGroupId>,
    pub limits: RateLimits,
    pub budget: ClientBudgetLimits,
}

/// 管理用例生成 ID 与明文后的持久化命令
#[derive(Clone, PartialEq, Eq)]
pub struct NewClientKey {
    pub peer_sharing: gateway_core::policy::ClientPeerSharing,
    pub request_profile_overrides: ProviderRequestProfileOverrides,
    pub id: ClientApiKeyId,
    pub name: String,
    pub label: Option<String>,
    pub group_ids: Vec<AccountGroupId>,
    pub limits: RateLimits,
    pub budget: ClientBudgetLimits,
    pub plaintext: String,
}

impl fmt::Debug for NewClientKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewClientKey")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("group_ids", &self.group_ids)
            .field("plaintext", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

/// 修改 Client Key 的公开策略字段
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateClientKey {
    pub peer_sharing: Option<gateway_core::policy::ClientPeerSharing>,
    pub request_profile_override_updates: ProviderRequestProfileOverrideUpdates,
    pub id: ClientApiKeyId,
    pub name: String,
    pub label: Option<String>,
    pub group_ids: Vec<AccountGroupId>,
    pub limits: RateLimits,
    pub daily_limit_usd: Option<gateway_core::metering::Decimal>,
    pub weekly_limit_usd: Option<gateway_core::metering::Decimal>,
}

/// 批量编辑只覆盖明确选择的互联选项
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientPeerSharingPatch {
    pub enabled: Option<bool>,
    pub share_pool: Option<bool>,
    pub share_quota: Option<bool>,
    pub share_plans: Option<bool>,
}

impl ClientPeerSharingPatch {
    #[must_use]
    pub fn is_empty(self) -> bool {
        self == Self::default()
    }

    #[must_use]
    pub fn apply(
        self,
        current: gateway_core::policy::ClientPeerSharing,
    ) -> gateway_core::policy::ClientPeerSharing {
        gateway_core::policy::ClientPeerSharing {
            enabled: self.enabled.unwrap_or(current.enabled),
            share_pool: self.share_pool.unwrap_or(current.share_pool),
            share_quota: self.share_quota.unwrap_or(current.share_quota),
            share_plans: self.share_plans.unwrap_or(current.share_plans),
        }
    }
}

/// 对各个密钥独立应用同一份局部设置
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchUpdateClientKeys {
    pub ids: Vec<ClientApiKeyId>,
    pub group_ids: Option<Vec<AccountGroupId>>,
    pub enabled: Option<bool>,
    pub max_concurrency: Option<u64>,
    pub requests_per_minute: Option<u64>,
    pub daily_limit_usd: Option<gateway_core::metering::Decimal>,
    pub weekly_limit_usd: Option<gateway_core::metering::Decimal>,
    pub peer_sharing: ClientPeerSharingPatch,
    pub request_profile_override_updates: ProviderRequestProfileOverrideUpdates,
    pub installation_seed: Option<uuid::Uuid>,
}

impl BatchUpdateClientKeys {
    #[must_use]
    pub fn has_changes(&self) -> bool {
        self.group_ids.is_some()
            || self.enabled.is_some()
            || self.max_concurrency.is_some()
            || self.requests_per_minute.is_some()
            || self.daily_limit_usd.is_some()
            || self.weekly_limit_usd.is_some()
            || !self.peer_sharing.is_empty()
            || !self.request_profile_override_updates.is_empty()
            || self.installation_seed.is_some()
    }
}

/// 修改 Client Key 启用状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetClientKeyEnabled {
    pub id: ClientApiKeyId,
    pub enabled: bool,
}

/// 管理员选择的金额重置范围
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKeyBudgetPeriod {
    Daily,
    Weekly,
    All,
}

/// 清零所选窗口并恢复日历规则，保留限额与历史费用
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetClientKeyBudget {
    pub id: ClientApiKeyId,
    pub period: ClientKeyBudgetPeriod,
}

/// 显式窗口复用已有日／周金额上限，外部周期的选择归调用方
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKeyBudgetWindowPeriod {
    Daily,
    Weekly,
}

impl ClientKeyBudgetWindowPeriod {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
        }
    }
}

/// 手动对齐只需要账号身份，列表不读取凭据、额度或用量
#[derive(serde::Serialize, Debug)]
pub struct BudgetReferenceAccount {
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    pub provider: String,
}

#[derive(serde::Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BudgetReferenceAccountPage {
    pub items: Vec<BudgetReferenceAccount>,
    pub next_cursor: Option<String>,
}

/// 管理员可预览的账号窗口保留 Provider 的稳定标识
pub struct ClientKeyBudgetReference {
    pub key: String,
    pub label: String,
    pub period: ClientKeyBudgetWindowPeriod,
    pub bounds: ExplicitClientKeyBudgetWindow,
}

impl ClientKeyBudgetReference {
    /// 只取整账号当前的日／周窗口，对齐结束后恢复密钥自身周期
    pub fn from_current_window(
        window: super::provider_credentials::ProviderQuotaWindow,
        now: DateTime<Utc>,
    ) -> Option<Self> {
        use super::provider_credentials::QuotaLocalUsageAttribution;
        if window.local_usage_attribution != QuotaLocalUsageAttribution::AccountWide
            || window
                .upstream
                .as_ref()
                .is_some_and(|upstream| upstream.stale)
        {
            return None;
        }
        let period = match window.window_seconds? {
            86_400 => ClientKeyBudgetWindowPeriod::Daily,
            604_800 => ClientKeyBudgetWindowPeriod::Weekly,
            _ => return None,
        };
        let (starts_at, ends_at) = window.observed_bounds()?;
        (starts_at..ends_at).contains(&now).then_some(Self {
            key: window.key,
            label: window.label,
            period,
            bounds: ExplicitClientKeyBudgetWindow {
                starts_at,
                ends_at,
                resume_calendar: true,
            },
        })
    }
}

/// 按请求完成时间归属的左闭右开预算窗口
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExplicitClientKeyBudgetWindow {
    /// 一次性对齐的窗口到期后恢复原生滚动
    pub resume_calendar: bool,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

/// 窗口配置版本独立于费用累计与全局配置版本
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKeyBudgetWindow {
    pub revision: u64,
    pub explicit: bool,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub used_usd: gateway_core::metering::Decimal,
}

/// 当前窗口版本标识一次操作，同版本同参数可重试；None 释放显式控制
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetClientKeyBudgetWindow {
    pub id: ClientApiKeyId,
    pub period: ClientKeyBudgetWindowPeriod,
    pub expected_revision: u64,
    pub window: Option<ExplicitClientKeyBudgetWindow>,
}

/// 仅更新指定金额上限；省略的周期保持不变，不修改已用金额或窗口
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateClientKeyBudgetLimits {
    pub id: ClientApiKeyId,
    pub daily_limit_usd: Option<gateway_core::metering::Decimal>,
    pub weekly_limit_usd: Option<gateway_core::metering::Decimal>,
}

/// 预算变更的调用来源；插件身份由 Runtime 给出，不能从插件请求反序列化
#[derive(Debug, Clone)]
pub enum ClientKeyBudgetMutationOrigin {
    Admin,
    Plugin(PluginResourceOwner),
}

/// 删除 Client Key
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteClientKey {
    pub id: ClientApiKeyId,
}

/// Client Key 创建结果；完整明文仅存在于该一次性结果中
pub struct CreatedClientKey {
    pub config_revision: Revision,
    pub secret: ClientKeySecret,
}

impl fmt::Debug for CreatedClientKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreatedClientKey")
            .field("config_revision", &self.config_revision)
            .field("secret", &self.secret)
            .finish()
    }
}

/// Client Key 普通写操作结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKeyMutation {
    pub config_revision: Revision,
    pub record: Option<ClientKeyRecord>,
    pub id: ClientApiKeyId,
}
