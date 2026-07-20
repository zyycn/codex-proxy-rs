//! Client API Key 的 Command、Result 与安全秘密类型。

use std::{fmt, num::NonZeroU16};

use chrono::{DateTime, Utc};

use gateway_core::{
    policy::{ClientApiKeyId, RateLimits},
    routing::ProviderKind,
};

use super::{AdminModelError, Revision};

/// Client Key 列表保持旧 HTTP 合同允许的完整非零 `u16` 页大小。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientKeyPageSize(NonZeroU16);

impl ClientKeyPageSize {
    /// 创建 1 至 65535 的 Client Key 页大小。
    ///
    /// # Errors
    ///
    /// `value` 为零时返回 [`AdminModelError::InvalidClientKeyPageSize`]。
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

/// Client Key 列表排序字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKeySortField {
    Name,
    Enabled,
    CreatedAt,
    LastUsedAt,
}

/// Client Key 列表排序方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

/// Client Key 列表排序规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientKeySort {
    pub field: ClientKeySortField,
    pub direction: SortDirection,
}

/// 与排序字段绑定的游标值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientKeyCursorValue {
    Name(String),
    Enabled(bool),
    CreatedAt(DateTime<Utc>),
    LastUsedAt(Option<DateTime<Utc>>),
}

/// Client Key 的稳定键集游标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKeyCursor {
    pub sort: ClientKeySort,
    pub value: ClientKeyCursorValue,
    pub id: ClientApiKeyId,
}

/// Client Key 列表查询。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKeyListQuery {
    pub cursor: Option<ClientKeyCursor>,
    pub page_size: ClientKeyPageSize,
    pub search: Option<String>,
    pub sort: ClientKeySort,
}

/// 不含完整明文 Key 的管理投影。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKeyRecord {
    pub id: ClientApiKeyId,
    pub name: String,
    pub label: Option<String>,
    pub provider_kind: ProviderKind,
    pub prefix: String,
    pub enabled: bool,
    pub limits: RateLimits,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Client Key 列表页。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKeyPage {
    pub config_revision: Revision,
    pub items: Vec<ClientKeyRecord>,
    pub total: u64,
    pub next_cursor: Option<ClientKeyCursor>,
}

/// 仅在创建或显式 reveal 时跨越管理边界的明文 Key。
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

/// API 提交的 Client Key 创建命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateClientKey {
    pub expected_config_revision: Revision,
    pub name: String,
    pub label: Option<String>,
    pub provider_kind: ProviderKind,
    pub limits: RateLimits,
}

/// 管理用例生成 ID 与明文后的持久化命令。
#[derive(Clone, PartialEq, Eq)]
pub struct NewClientKey {
    pub expected_config_revision: Revision,
    pub id: ClientApiKeyId,
    pub name: String,
    pub label: Option<String>,
    pub provider_kind: ProviderKind,
    pub limits: RateLimits,
    pub plaintext: String,
}

impl fmt::Debug for NewClientKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewClientKey")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("provider_kind", &self.provider_kind)
            .field("plaintext", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

/// 修改 Client Key 的公开策略字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateClientKey {
    pub expected_config_revision: Revision,
    pub id: ClientApiKeyId,
    pub name: String,
    pub label: Option<String>,
    pub provider_kind: ProviderKind,
    pub limits: RateLimits,
}

/// 修改 Client Key 启用状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetClientKeyEnabled {
    pub expected_config_revision: Revision,
    pub id: ClientApiKeyId,
    pub enabled: bool,
}

/// 删除 Client Key。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteClientKey {
    pub expected_config_revision: Revision,
    pub id: ClientApiKeyId,
}

/// Client Key 创建结果；完整明文仅存在于该一次性结果中。
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

/// Client Key 普通写操作结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKeyMutation {
    pub config_revision: Revision,
    pub record: Option<ClientKeyRecord>,
    pub id: ClientApiKeyId,
}
