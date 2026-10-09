//! Runtime settings 与明文管理员 API Key 的语义模型

use std::{collections::BTreeMap, fmt};

use chrono::{DateTime, Utc};

use gateway_core::routing::{ProviderKind, PublicModelId, UpstreamModelId};

use super::Revision;

/// 客户端模型到上游模型的全局精确映射
pub type ModelMappings = BTreeMap<PublicModelId, UpstreamModelId>;

/// 全局模型映射的数量与文本约束；调用方保留各自的键值表示
pub fn validate_model_mappings<'a>(
    mappings: impl ExactSizeIterator<Item = (&'a str, &'a str)>,
) -> Result<(), &'static str> {
    if mappings.len() > 512 {
        return Err("model_mappings");
    }
    for (requested, upstream) in mappings {
        for value in [requested, upstream] {
            if value.is_empty()
                || value.len() > 256
                || value.bytes().any(|byte| byte.is_ascii_control())
            {
                return Err("model_mappings");
            }
        }
    }
    Ok(())
}

/// Provider-owned 请求画像选择；Provider ID 是唯一命名空间
pub type ProviderRequestProfiles =
    BTreeMap<ProviderKind, gateway_core::account::OpaqueProviderData>;

/// Provider-owned 请求画像的部分更新；`None` 只表示显式删除该 Provider 的历史选择
pub type ProviderRequestProfileUpdates =
    BTreeMap<ProviderKind, Option<gateway_core::account::OpaqueProviderData>>;

/// 账号调度策略；由 Core 拥有稳定值与 wire 映射
pub use gateway_core::account::RotationStrategy;

/// 运行设置的共同值；版本、Provider 画像更新和数据库秘密由外层类型拥有
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RuntimeSettingsValues {
    pub codex_privacy_policy: gateway_core::settings::privacy::CodexPrivacyPolicy,
    pub request_location_enabled: bool,
    pub request_location: gateway_core::account::RequestLocation,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: u32,
    pub request_interval_ms: u64,
    pub max_waiting_per_key: u32,
    pub max_waiting_per_account: u32,
    pub concurrency_wait_timeout_seconds: u32,
    pub openai_guardian_reserved_concurrency: u32,
    pub openai_account_affinity: gateway_core::account::AccountAffinity,
    pub max_account_rotations: u32,
    pub openai_session_affinity_ttl_hours: u32,
    pub responses_max_decompressed_body_bytes: u64,
    pub smart_scheduling: gateway_core::account::SmartSchedulingConfig,
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

/// 完整运行设置事实
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettings {
    #[serde(flatten)]
    pub values: RuntimeSettingsValues,
    pub request_profiles: ProviderRequestProfiles,
    pub config_revision: Revision,
    pub model_mappings: ModelMappings,
    pub rotation_strategy: RotationStrategy,
    pub updated_at: DateTime<Utc>,
}

/// 原子替换运行设置的命令
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceRuntimeSettings {
    #[serde(flatten)]
    pub values: RuntimeSettingsValues,
    /// 读取设置时的版本；与写入在同一事务内比较，防止覆盖并发更新
    pub expected_revision: Revision,
    /// 只覆盖提交的 Provider；未提交项保留当前持久值
    pub request_profile_updates: ProviderRequestProfileUpdates,
    pub model_mappings: ModelMappings,
    pub rotation_strategy: RotationStrategy,
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
            values: settings.values,
            model_mappings: settings.model_mappings,
            rotation_strategy: settings.rotation_strategy,
        }
    }
}

impl RuntimeSettingsValues {
    /// 校验共同事实，返回字段名供边界转换错误；SQL 类型转换仍由 Store 处理
    pub fn validate(&self) -> Result<(), &'static str> {
        self.request_location
            .validate()
            .map_err(|_| "request_location")?;
        gateway_core::settings::validate_request_limits(
            self.max_waiting_per_key,
            self.max_waiting_per_account,
            self.concurrency_wait_timeout_seconds,
            self.max_account_rotations,
            self.openai_session_affinity_ttl_hours,
        )?;
        gateway_core::settings::response_body_limit(self.responses_max_decompressed_body_bytes)?;
        gateway_core::settings::client_min_versions(
            self.min_codex_desktop_version.as_deref(),
            self.min_codex_cli_version.as_deref(),
        )?;
        for (valid, field) in [
            (
                self.refresh_margin_seconds > 0
                    && i64::try_from(self.refresh_margin_seconds).is_ok(),
                "refresh_margin_seconds",
            ),
            (self.refresh_concurrency > 0, "refresh_concurrency"),
            (
                i64::try_from(self.request_interval_ms).is_ok(),
                "request_interval_ms",
            ),
        ] {
            if !valid {
                return Err(field);
            }
        }
        super::retention::RetentionPolicy::validate_values(
            self.usage_retention_days,
            self.ops_event_retention_days,
            self.audit_retention_days,
        )?;
        gateway_core::provider_ports::ProviderFreezePolicy::validate_values(
            self.account_auto_freeze_threshold,
            self.account_auto_freeze_window_seconds,
            self.account_auto_freeze_duration_seconds,
            self.account_auto_freeze_probe_model.as_deref(),
        )?;
        gateway_core::provider_ports::ProviderWarmupPolicy::validate_values(
            self.account_warmup_enabled,
            &self.account_warmup_schedule_time,
            self.account_warmup_model.as_deref(),
        )?;
        Ok(())
    }
}
