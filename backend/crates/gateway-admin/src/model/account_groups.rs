//! 跨 Provider 账号分组的管理命令与查询投影

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use gateway_core::{
    account::{AccountStatusFacts, FastMode},
    routing::AccountGroupId,
};

use super::{PageSize, Revision, observability::DecimalAmount};

/// 账号分组持久化使用的标准 `#RRGGBBAA` 颜色
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

/// 当前观测时刻的分组成员可用性
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountGroupAccountSummary {
    pub available: u64,
    pub limited: u64,
    pub total: u64,
}

/// 根据可用账号与运行时租约计算的分组调度槽位
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountGroupCapacity {
    pub used_slots: Option<u64>,
    /// `None` 表示可用账号中存在不限制并发的账号
    pub total_slots: Option<u64>,
}

/// 分组账号成功且已向下游交付的请求美元费用
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupUsage {
    pub today_usd: DecimalAmount,
    /// 当前 usage retention 窗口内、按请求发生时 routing group 快照归属的累计成本
    pub retained_total_usd: DecimalAmount,
}

/// 当前页账号组 membership 对应的持久账号事实；运行态在 Admin query service 合并
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupMemberFact {
    pub group_id: AccountGroupId,
    pub account_id: String,
    pub status: AccountStatusFacts,
    pub total_slots: Option<u64>,
}

/// 账号与 Client Key 视图内嵌的轻量分组引用
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupRef {
    pub id: AccountGroupId,
    pub name: String,
    pub color: AccountGroupColor,
    pub enabled: bool,
}

/// 账号分组列表查询
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupListQuery {
    pub page: u32,
    pub page_size: PageSize,
    pub search: Option<String>,
    pub enabled: Option<bool>,
}

/// 完整账号分组摘要
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupRecord {
    pub fast_mode: FastMode,
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

/// 账号分组分页结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupPage {
    pub config_revision: Revision,
    pub items: Vec<AccountGroupRecord>,
    pub total: u64,
    pub page: u32,
    pub page_size: u16,
}

/// 创建账号分组
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateAccountGroup {
    pub fast_mode: FastMode,
    pub name: String,
    pub description: Option<String>,
    pub color: AccountGroupColor,
}

/// 已生成稳定标识、可直接提交存储的创建命令
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAccountGroup {
    pub fast_mode: FastMode,
    pub id: AccountGroupId,
    pub name: String,
    pub description: Option<String>,
    pub color: AccountGroupColor,
}

/// 更新账号分组的描述字段
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateAccountGroup {
    pub fast_mode: Option<FastMode>,
    pub id: AccountGroupId,
    pub name: String,
    pub description: Option<String>,
    pub color: AccountGroupColor,
}

/// 启用或停用账号分组
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetAccountGroupEnabled {
    pub id: AccountGroupId,
    pub enabled: bool,
}

/// 删除未被 Client Key 引用的账号分组
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteAccountGroup {
    pub id: AccountGroupId,
}

/// 账号分组变更结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGroupMutation {
    pub config_revision: Revision,
    pub id: AccountGroupId,
    pub record: Option<AccountGroupRecord>,
}
