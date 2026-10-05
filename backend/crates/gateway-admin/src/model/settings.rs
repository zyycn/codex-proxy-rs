//! Runtime settings 与明文管理员 API Key 的语义模型

use std::{collections::BTreeMap, fmt};

use chrono::{DateTime, Utc};

use gateway_core::routing::{ProviderKind, PublicModelId, UpstreamModelId};

use super::Revision;

/// 客户端模型到上游模型的全局精确映射
pub type ModelMappings = BTreeMap<PublicModelId, UpstreamModelId>;

/// Provider-owned 请求画像选择；Provider ID 是唯一命名空间
pub type ProviderRequestProfiles =
    BTreeMap<ProviderKind, gateway_core::account::OpaqueProviderData>;

/// Provider-owned 请求画像的部分更新；`None` 只表示显式删除该 Provider 的历史选择
pub type ProviderRequestProfileUpdates =
    BTreeMap<ProviderKind, Option<gateway_core::account::OpaqueProviderData>>;

/// 账号调度策略；由 Core 拥有稳定值与 wire 映射
pub use gateway_core::account::RotationStrategy;

/// 完整运行设置事实
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettings {
    pub request_profiles: ProviderRequestProfiles,
    pub config_revision: Revision,
    pub request_location_enabled: bool,
    pub request_location: gateway_core::account::RequestLocation,
    pub model_mappings: ModelMappings,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: u32,
    pub request_interval_ms: u64,
    pub max_waiting_per_key: u32,
    pub max_waiting_per_account: u32,
    pub concurrency_wait_timeout_seconds: u32,
    pub openai_guardian_reserved_concurrency: u32,
    pub responses_max_decompressed_body_bytes: u64,
    pub smart_scheduling: gateway_core::account::SmartSchedulingConfig,
    pub rotation_strategy: RotationStrategy,
    pub min_codex_desktop_version: Option<String>,
    pub min_codex_cli_version: Option<String>,
    pub usage_retention_days: u32,
    pub ops_event_retention_days: u32,
    pub audit_retention_days: u32,
    pub account_auto_freeze_enabled: bool,
    pub account_auto_freeze_threshold: u32,
    pub account_auto_freeze_window_seconds: u64,
    pub account_auto_freeze_duration_seconds: u64,
    pub account_auto_freeze_probe_enabled: bool,
    pub account_auto_freeze_probe_model: Option<String>,
    pub account_auto_freeze_adaptive_concurrency: bool,
    pub account_warmup_enabled: bool,
    pub account_warmup_schedule_time: String,
    pub account_warmup_model: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// 原子替换运行设置的命令
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceRuntimeSettings {
    /// 读取设置时的版本；与写入在同一事务内比较，防止覆盖并发更新
    pub expected_revision: Revision,
    /// 只覆盖提交的 Provider；未提交项保留当前持久值
    pub request_profile_updates: ProviderRequestProfileUpdates,
    pub request_location_enabled: bool,
    pub request_location: gateway_core::account::RequestLocation,
    pub model_mappings: ModelMappings,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: u32,
    pub request_interval_ms: u64,
    pub max_waiting_per_key: u32,
    pub max_waiting_per_account: u32,
    pub concurrency_wait_timeout_seconds: u32,
    pub openai_guardian_reserved_concurrency: u32,
    pub responses_max_decompressed_body_bytes: u64,
    pub smart_scheduling: gateway_core::account::SmartSchedulingConfig,
    pub rotation_strategy: RotationStrategy,
    pub min_codex_desktop_version: Option<String>,
    pub min_codex_cli_version: Option<String>,
    pub usage_retention_days: u32,
    pub ops_event_retention_days: u32,
    pub audit_retention_days: u32,
    pub account_auto_freeze_enabled: bool,
    pub account_auto_freeze_threshold: u32,
    pub account_auto_freeze_window_seconds: u64,
    pub account_auto_freeze_duration_seconds: u64,
    pub account_auto_freeze_probe_enabled: bool,
    pub account_auto_freeze_probe_model: Option<String>,
    pub account_auto_freeze_adaptive_concurrency: bool,
    pub account_warmup_enabled: bool,
    pub account_warmup_schedule_time: String,
    pub account_warmup_model: Option<String>,
}

/// 明文管理员 API Key；按产品约束明文落库，但禁止 Debug 泄漏
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
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

    /// 仅供显式 regenerate 响应读取一次
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

/// 管理员 API Key 更新结果
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminApiKeyMutation {
    pub config_revision: Revision,
    pub exists: bool,
}

/// 新管理员 API Key 的一次性返回结果
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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

impl RuntimeSettings {
    pub fn client_profile(
        &self,
        provider: &str,
    ) -> Option<&gateway_core::account::OpaqueProviderData> {
        ProviderKind::new(provider.to_owned())
            .ok()
            .and_then(|provider| self.request_profiles.get(&provider))
    }
}

impl From<RuntimeSettings> for ReplaceRuntimeSettings {
    fn from(settings: RuntimeSettings) -> Self {
        Self {
            expected_revision: settings.config_revision,
            request_profile_updates: settings
                .request_profiles
                .into_iter()
                .map(|(provider, value)| (provider, Some(value)))
                .collect(),
            request_location_enabled: settings.request_location_enabled,
            request_location: settings.request_location,
            model_mappings: settings.model_mappings,
            refresh_margin_seconds: settings.refresh_margin_seconds,
            refresh_concurrency: settings.refresh_concurrency,
            max_concurrent_per_account: settings.max_concurrent_per_account,
            request_interval_ms: settings.request_interval_ms,
            max_waiting_per_key: settings.max_waiting_per_key,
            max_waiting_per_account: settings.max_waiting_per_account,
            concurrency_wait_timeout_seconds: settings.concurrency_wait_timeout_seconds,
            openai_guardian_reserved_concurrency: settings.openai_guardian_reserved_concurrency,
            responses_max_decompressed_body_bytes: settings.responses_max_decompressed_body_bytes,
            smart_scheduling: settings.smart_scheduling,
            rotation_strategy: settings.rotation_strategy,
            min_codex_desktop_version: settings.min_codex_desktop_version,
            min_codex_cli_version: settings.min_codex_cli_version,
            usage_retention_days: settings.usage_retention_days,
            ops_event_retention_days: settings.ops_event_retention_days,
            audit_retention_days: settings.audit_retention_days,
            account_auto_freeze_enabled: settings.account_auto_freeze_enabled,
            account_auto_freeze_threshold: settings.account_auto_freeze_threshold,
            account_auto_freeze_window_seconds: settings.account_auto_freeze_window_seconds,
            account_auto_freeze_duration_seconds: settings.account_auto_freeze_duration_seconds,
            account_auto_freeze_probe_enabled: settings.account_auto_freeze_probe_enabled,
            account_auto_freeze_probe_model: settings.account_auto_freeze_probe_model,
            account_auto_freeze_adaptive_concurrency: settings
                .account_auto_freeze_adaptive_concurrency,
            account_warmup_enabled: settings.account_warmup_enabled,
            account_warmup_schedule_time: settings.account_warmup_schedule_time,
            account_warmup_model: settings.account_warmup_model,
        }
    }
}
