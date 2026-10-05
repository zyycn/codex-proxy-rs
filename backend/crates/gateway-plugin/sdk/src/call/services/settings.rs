//! 宿主设置服务的操作标识与请求、响应数据合同
//!
//! 从宿主设置类型与 service/settings.rs 生成；更新命令见 SDK 维护说明

use super::Operation;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
};
pub type ProviderRequestProfiles = BTreeMap<String, serde_json::Map<String, serde_json::Value>>;
pub type ProviderRequestProfileUpdates =
    BTreeMap<String, Option<serde_json::Map<String, serde_json::Value>>>;
pub type ModelMappings = BTreeMap<String, String>;
pub type Revision = NonZeroU64;
pub type RotationStrategy = String;
pub type PricingOverrides = BTreeMap<String, BTreeMap<String, ModelPriceOverride>>;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettings {
    pub request_profiles: ProviderRequestProfiles,
    pub config_revision: Revision,
    pub request_location_enabled: bool,
    pub request_location: RequestLocation,
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
    pub smart_scheduling: SmartSchedulingConfig,
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
    pub updated_at: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceRuntimeSettings {
    /// 读取设置时的版本；与写入在同一事务内比较，防止覆盖并发更新
    pub expected_revision: Revision,
    /// 只覆盖提交的 Provider；未提交项保留当前持久值
    pub request_profile_updates: ProviderRequestProfileUpdates,
    pub request_location_enabled: bool,
    pub request_location: RequestLocation,
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
    pub smart_scheduling: SmartSchedulingConfig,
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
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminApiKeyMutation {
    pub config_revision: Revision,
    pub exists: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegeneratedAdminApiKey {
    pub mutation: AdminApiKeyMutation,
    pub key: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MutationActor {
    AdminSession { admin_user_id: String },
    AdminApiKey,
    System,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MutationContext {
    pub actor: MutationActor,
    pub request_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestLocation {
    pub country: String,
    pub region: String,
    pub city: String,
    pub timezone: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SmartSchedulingConfig {
    pub load_weight: f64,
    pub quota_weight: f64,
    pub health_weight: f64,
    pub latency_weight: f64,
    pub reset_weight: f64,
    pub queue_weight: f64,
    pub prefer_higher_weight: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenPriceOverride {
    pub input: String,
    pub output: String,
    pub cache_read: String,
    pub cache_write: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelPriceOverride {
    pub multiplier_bps: u32,
    pub bands: BTreeMap<String, TokenPriceOverride>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingCatalog {
    pub defaults: PricingOverrides,
    pub overrides: PricingOverrides,
    pub synced: PricingOverrides,
    pub synced_at: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingSyncPreview {
    pub prices: PricingOverrides,
    pub skipped: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncPricing {
    pub preview: PricingSyncPreview,
    pub models: BTreeMap<String, BTreeSet<String>>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PricingChange {
    Replace(ModelPriceOverride),
    Multiplier(u32),
    Reset,
    Delete,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePricing {
    pub provider: String,
    pub models: Vec<String>,
    pub change: PricingChange,
}
pub struct Load;
impl Operation for Load {
    const NAME: &'static str = "settings.load";
    type Input = ();
    type Output = RuntimeSettings;
}
pub struct Replace;
impl Operation for Replace {
    const NAME: &'static str = "settings.replace";
    type Input = (MutationContext, ReplaceRuntimeSettings);
    type Output = RuntimeSettings;
}
pub struct ApiKeyExists;
impl Operation for ApiKeyExists {
    const NAME: &'static str = "settings.admin_api_key_exists";
    type Input = ();
    type Output = bool;
}
pub struct RegenerateApiKey;
impl Operation for RegenerateApiKey {
    const NAME: &'static str = "settings.regenerate_admin_api_key";
    type Input = MutationContext;
    type Output = RegeneratedAdminApiKey;
}
pub struct DeleteApiKey;
impl Operation for DeleteApiKey {
    const NAME: &'static str = "settings.delete_admin_api_key";
    type Input = MutationContext;
    type Output = AdminApiKeyMutation;
}
pub struct Pricing;
impl Operation for Pricing {
    const NAME: &'static str = "settings.pricing";
    type Input = ();
    type Output = PricingCatalog;
}
pub struct PreviewPricingSync;
impl Operation for PreviewPricingSync {
    const NAME: &'static str = "settings.preview_pricing_sync";
    type Input = ();
    type Output = PricingSyncPreview;
}
pub struct Sync;
impl Operation for Sync {
    const NAME: &'static str = "settings.sync_pricing";
    type Input = (MutationContext, SyncPricing);
    type Output = ();
}
pub struct Update;
impl Operation for Update {
    const NAME: &'static str = "settings.update_pricing";
    type Input = (MutationContext, UpdatePricing);
    type Output = ();
}
pub struct ClientProfileOptions;
impl Operation for ClientProfileOptions {
    const NAME: &'static str = "settings.client_profile_options";
    type Input = String;
    type Output = serde_json::Map<String, serde_json::Value>;
}
pub struct PreviewClientProfile;
impl Operation for PreviewClientProfile {
    const NAME: &'static str = "settings.preview_client_profile";
    type Input = (String, Option<serde_json::Map<String, serde_json::Value>>);
    type Output = serde_json::Map<String, serde_json::Value>;
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
