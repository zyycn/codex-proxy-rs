//! Provider-neutral account group commands and projections.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use gateway_core::{engine::credential::AccountStatusFacts, routing::AccountGroupId};

use super::{PageSize, Revision, observability::DecimalAmount};

/// Canonical `#RRGGBBAA` color persisted with an account group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupColor(String);

impl AccountGroupColor {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        (value.len() == 9
            && value.starts_with('#')
            && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| Self(value.to_ascii_uppercase()))
    }
}

/// Group member availability at the current observation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountGroupAccountSummary {
    pub available: u64,
    pub limited: u64,
    pub total: u64,
}

/// Group scheduling slots derived from available accounts and runtime leases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountGroupCapacity {
    pub used_slots: Option<u64>,
    pub total_slots: u64,
}

/// Successful, downstream-committed USD request costs for group accounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupUsage {
    pub today_usd: DecimalAmount,
    /// 当前 usage retention 窗口内、按请求发生时 routing group 快照归属的累计成本。
    pub retained_total_usd: DecimalAmount,
}

/// 当前页账号组 membership 对应的持久账号事实；运行态在 Admin query service 合并。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupMemberFact {
    pub group_id: AccountGroupId,
    pub account_id: String,
    pub status: AccountStatusFacts,
    pub total_slots: u64,
}

/// Lightweight group reference embedded in account and client-key views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupRef {
    pub id: AccountGroupId,
    pub name: String,
    pub color: AccountGroupColor,
    pub enabled: bool,
}

/// Account group list query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupListQuery {
    pub page: u32,
    pub page_size: PageSize,
    pub search: Option<String>,
    pub enabled: Option<bool>,
}

/// Complete account group summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupRecord {
    pub id: AccountGroupId,
    pub name: String,
    pub description: Option<String>,
    pub color: AccountGroupColor,
    pub enabled: bool,
    pub member_count: u64,
    pub provider_counts: BTreeMap<String, u64>,
    pub client_key_count: u64,
    pub account_summary: AccountGroupAccountSummary,
    pub capacity: AccountGroupCapacity,
    pub usage: AccountGroupUsage,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Paginated account groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupPage {
    pub config_revision: Revision,
    pub items: Vec<AccountGroupRecord>,
    pub total: u64,
    pub page: u32,
    pub page_size: u16,
}

/// Create an account group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateAccountGroup {
    pub name: String,
    pub description: Option<String>,
    pub color: AccountGroupColor,
}

/// Store-ready create command with a generated stable ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAccountGroup {
    pub id: AccountGroupId,
    pub name: String,
    pub description: Option<String>,
    pub color: AccountGroupColor,
}

/// Update an account group's descriptive fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateAccountGroup {
    pub id: AccountGroupId,
    pub name: String,
    pub description: Option<String>,
    pub color: AccountGroupColor,
}

/// Enable or disable an account group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetAccountGroupEnabled {
    pub id: AccountGroupId,
    pub enabled: bool,
}

/// Delete an account group that is not referenced by a client key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteAccountGroup {
    pub id: AccountGroupId,
}

/// Result of any account-group mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupMutation {
    pub config_revision: Revision,
    pub id: AccountGroupId,
    pub record: Option<AccountGroupRecord>,
}
