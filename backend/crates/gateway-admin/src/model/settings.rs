//! Runtime settings 与明文管理员 API Key 的语义模型。

use std::{collections::BTreeMap, fmt};

use chrono::{DateTime, Utc};

use gateway_core::routing::{ProviderKind, PublicModelId, UpstreamModelId};

use super::Revision;

/// Provider → 客户端模型 → 上游模型的精确映射。
pub type ProviderModelMappings = BTreeMap<ProviderKind, BTreeMap<PublicModelId, UpstreamModelId>>;

/// 账号调度策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationStrategy {
    Smart,
    QuotaResetPriority,
    RoundRobin,
    Sticky,
}

/// 完整运行设置事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSettings {
    pub config_revision: Revision,
    pub provider_model_mappings: ProviderModelMappings,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: u32,
    pub request_interval_ms: u64,
    pub rotation_strategy: RotationStrategy,
    pub usage_retention_days: u32,
    pub ops_event_retention_days: u32,
    pub audit_retention_days: u32,
    pub updated_at: DateTime<Utc>,
}

/// 原子替换运行设置的命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceRuntimeSettings {
    pub expected_config_revision: Revision,
    pub provider_model_mappings: ProviderModelMappings,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: u32,
    pub request_interval_ms: u64,
    pub rotation_strategy: RotationStrategy,
    pub usage_retention_days: u32,
    pub ops_event_retention_days: u32,
    pub audit_retention_days: u32,
}

/// 明文管理员 API Key；按产品约束明文落库，但禁止 Debug 泄漏。
#[derive(Clone, PartialEq, Eq)]
pub struct AdminApiKey(String);

impl AdminApiKey {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn expose_for_auth(&self) -> &str {
        &self.0
    }

    /// 仅供显式 regenerate 响应读取一次。
    #[must_use]
    pub fn expose_for_response(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AdminApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AdminApiKey([REDACTED])")
    }
}

/// 管理员 API Key 更新结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminApiKeyMutation {
    pub config_revision: Revision,
    pub exists: bool,
}

/// 新管理员 API Key 的一次性返回结果。
pub struct RegeneratedAdminApiKey {
    pub mutation: AdminApiKeyMutation,
    pub key: AdminApiKey,
}

impl fmt::Debug for RegeneratedAdminApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegeneratedAdminApiKey")
            .field("mutation", &self.mutation)
            .field("key", &"[REDACTED]")
            .finish()
    }
}
