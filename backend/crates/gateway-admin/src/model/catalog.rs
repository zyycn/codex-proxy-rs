//! Provider instance 目录的 Command 与 Result。

use chrono::{DateTime, Utc};

use gateway_core::routing::{ProviderInstanceId, ProviderKind};

use super::{PageSize, Revision};

/// 一个可管理的 Provider instance。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInstance {
    pub id: ProviderInstanceId,
    pub provider_kind: ProviderKind,
    pub name: String,
    pub base_url: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Provider instance 列表结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInstanceCatalog {
    pub config_revision: Revision,
    pub items: Vec<ProviderInstance>,
}

/// Provider instance 详情及其所属控制面 revision。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInstanceDetail {
    pub config_revision: Revision,
    pub item: ProviderInstance,
}

/// Provider instance 的稳定游标查询。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogListQuery {
    pub cursor: Option<ProviderInstanceId>,
    pub page_size: PageSize,
}

/// Provider instance 分页结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInstancePage {
    pub config_revision: Revision,
    pub items: Vec<ProviderInstance>,
    pub next_cursor: Option<ProviderInstanceId>,
}

/// 创建 Provider instance。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateProviderInstance {
    pub expected_config_revision: Revision,
    pub id: ProviderInstanceId,
    pub provider_kind: ProviderKind,
    pub name: String,
    pub base_url: String,
}

/// 修改 Provider instance 的公共字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateProviderInstance {
    pub expected_config_revision: Revision,
    pub id: ProviderInstanceId,
    pub name: String,
    pub base_url: String,
}

/// 修改 Provider instance 的启用状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetProviderInstanceEnabled {
    pub expected_config_revision: Revision,
    pub id: ProviderInstanceId,
    pub enabled: bool,
}

/// 删除 Provider instance。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteProviderInstance {
    pub expected_config_revision: Revision,
    pub id: ProviderInstanceId,
}

/// Provider instance 写入后的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInstanceMutation {
    pub config_revision: Revision,
    pub instance: Option<ProviderInstance>,
}
