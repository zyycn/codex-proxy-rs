//! `runtime_settings` 单例与 config revision 的 PostgreSQL owner。

use std::num::NonZeroU32;
use std::time::Duration;
use std::{collections::BTreeMap, fmt};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};

use gateway_core::account::RotationStrategy;
use gateway_core::policy::CodexClientVersion;
use gateway_core::provider_ports::{
    ProviderFreezePolicy, ProviderRefreshPolicy, ProviderRuntimePolicyPort, ProviderStoreError,
    ProviderStoreErrorKind,
};

use crate::{Revision, StoreError, StoreResult, postgres_unavailable};

#[derive(Clone, PartialEq, Eq)]
pub struct RuntimeSettings {
    pub openai_client_profile: Option<gateway_core::account::OpaqueProviderData>,
    pub xai_client_profile: Option<gateway_core::account::OpaqueProviderData>,
    pub config_revision: Revision,
    pub admin_api_key: Option<String>,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: u32,
    pub request_interval_ms: u64,
    pub max_waiting_per_key: u32,
    pub max_waiting_per_account: u32,
    pub concurrency_wait_timeout_seconds: u32,
    pub responses_max_decompressed_body_bytes: u64,
    pub rotation_strategy: String,
    pub request_location_enabled: bool,
    pub request_location: gateway_core::account::RequestLocation,
    pub model_mappings: BTreeMap<String, String>,
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
    pub updated_at: DateTime<Utc>,
}

impl fmt::Debug for RuntimeSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeSettings")
            .field("config_revision", &self.config_revision)
            .field(
                "admin_api_key",
                &self.admin_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("refresh_margin_seconds", &self.refresh_margin_seconds)
            .field("refresh_concurrency", &self.refresh_concurrency)
            .field(
                "max_concurrent_per_account",
                &self.max_concurrent_per_account,
            )
            .field("request_interval_ms", &self.request_interval_ms)
            .field("rotation_strategy", &self.rotation_strategy)
            .field("request_location_enabled", &self.request_location_enabled)
            .field("request_location", &self.request_location)
            .field("model_mappings", &self.model_mappings)
            .field("min_codex_desktop_version", &self.min_codex_desktop_version)
            .field("min_codex_cli_version", &self.min_codex_cli_version)
            .field("usage_retention_days", &self.usage_retention_days)
            .field("ops_event_retention_days", &self.ops_event_retention_days)
            .field("audit_retention_days", &self.audit_retention_days)
            .field(
                "account_auto_freeze_enabled",
                &self.account_auto_freeze_enabled,
            )
            .field(
                "account_auto_freeze_threshold",
                &self.account_auto_freeze_threshold,
            )
            .field(
                "account_auto_freeze_window_seconds",
                &self.account_auto_freeze_window_seconds,
            )
            .field(
                "account_auto_freeze_duration_seconds",
                &self.account_auto_freeze_duration_seconds,
            )
            .field(
                "account_auto_freeze_probe_enabled",
                &self.account_auto_freeze_probe_enabled,
            )
            .field(
                "account_auto_freeze_probe_model",
                &self.account_auto_freeze_probe_model,
            )
            .field(
                "account_auto_freeze_adaptive_concurrency",
                &self.account_auto_freeze_adaptive_concurrency,
            )
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Clone)]
pub struct RuntimeSettingsUpdate {
    pub openai_client_profile: Option<gateway_core::account::OpaqueProviderData>,
    pub xai_client_profile: Option<gateway_core::account::OpaqueProviderData>,
    pub admin_api_key: Option<String>,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: u32,
    pub request_interval_ms: u64,
    pub max_waiting_per_key: u32,
    pub max_waiting_per_account: u32,
    pub concurrency_wait_timeout_seconds: u32,
    pub responses_max_decompressed_body_bytes: u64,
    pub rotation_strategy: String,
    pub request_location_enabled: bool,
    pub request_location: gateway_core::account::RequestLocation,
    pub model_mappings: BTreeMap<String, String>,
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
}

impl fmt::Debug for RuntimeSettingsUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeSettingsUpdate")
            .field(
                "admin_api_key",
                &self.admin_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("rotation_strategy", &self.rotation_strategy)
            .field("request_location_enabled", &self.request_location_enabled)
            .field("request_location", &self.request_location)
            .field("model_mappings", &self.model_mappings)
            .finish_non_exhaustive()
    }
}

impl RuntimeSettingsUpdate {
    pub fn validate(&self) -> StoreResult<()> {
        if self.request_location.validate().is_err()
            || self.responses_max_decompressed_body_bytes == 0
            || isize::try_from(self.responses_max_decompressed_body_bytes).is_err()
            || self.refresh_margin_seconds == 0
            || self.refresh_concurrency == 0
            || self.max_concurrent_per_account == 0
            || self.max_waiting_per_key > 1_000
            || self.max_waiting_per_account > 1_000
            || !(1..=120).contains(&self.concurrency_wait_timeout_seconds)
            || self.usage_retention_days < 31
            || self.ops_event_retention_days == 0
            || self.audit_retention_days == 0
            || !(2..=1_000).contains(&self.account_auto_freeze_threshold)
            || !(60..=3_600).contains(&self.account_auto_freeze_window_seconds)
            || !(300..=604_800).contains(&self.account_auto_freeze_duration_seconds)
            || !valid_model_mappings(&self.model_mappings)
            || !valid_client_version(self.min_codex_desktop_version.as_deref())
            || !valid_client_version(self.min_codex_cli_version.as_deref())
            || !valid_probe_model(self.account_auto_freeze_probe_model.as_deref())
            || RotationStrategy::parse(&self.rotation_strategy).is_none()
        {
            return Err(StoreError::InvalidData {
                entity: "runtime settings",
                message: "settings violate the frozen runtime constraints".to_owned(),
            });
        }
        Ok(())
    }
}

#[async_trait]
pub trait RuntimeSettingsRepository: Send + Sync {
    async fn load_runtime_settings(&self) -> StoreResult<RuntimeSettings>;

    async fn update_runtime_settings(&self, update: RuntimeSettingsUpdate)
    -> StoreResult<Revision>;
}

#[derive(Clone)]
pub struct PgRuntimeSettingsRepository {
    pool: PgPool,
}

impl PgRuntimeSettingsRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RuntimeSettingsRepository for PgRuntimeSettingsRepository {
    async fn load_runtime_settings(&self) -> StoreResult<RuntimeSettings> {
        load_runtime_settings_from_pool(&self.pool).await
    }

    async fn update_runtime_settings(
        &self,
        update: RuntimeSettingsUpdate,
    ) -> StoreResult<Revision> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin runtime settings update"))?;
        let revision = update_runtime_settings_in_transaction(&mut transaction, &update).await?;
        transaction
            .commit()
            .await
            .map_err(|_| postgres_unavailable("commit runtime settings update"))?;
        Ok(revision)
    }
}

pub(crate) async fn load_runtime_settings_from_pool(pool: &PgPool) -> StoreResult<RuntimeSettings> {
    let row = sqlx::query_as::<_, RuntimeSettingsRow>(
            "select provider_request_profiles_json, config_revision, admin_api_key, refresh_margin_seconds, request_location_json, request_location_enabled,
                    refresh_concurrency, max_concurrent_per_account, request_interval_ms,
                    rotation_strategy, model_mappings_json, usage_retention_days, ops_event_retention_days,
                    audit_retention_days, min_codex_desktop_version,
                    min_codex_cli_version, updated_at, responses_max_decompressed_body_bytes, max_waiting_per_key, max_waiting_per_account, concurrency_wait_timeout_seconds,
                    account_auto_freeze_enabled, account_auto_freeze_threshold,
                    account_auto_freeze_window_seconds, account_auto_freeze_duration_seconds,
                    account_auto_freeze_probe_enabled, account_auto_freeze_probe_model,
                    account_auto_freeze_adaptive_concurrency
             from runtime_settings where id = 1",
        )
    .fetch_optional(pool)
    .await
    .map_err(|_| postgres_unavailable("load runtime settings"))?
    .ok_or_else(|| StoreError::NotFound {
        entity: "runtime settings",
        id: "1".to_owned(),
    })?;
    runtime_settings_from_row(row)
}

impl ProviderRuntimePolicyPort for PgRuntimeSettingsRepository {
    fn initialize_request_profile<'a>(
        &'a self,
        provider: &'a gateway_core::routing::ProviderKind,
        initial: gateway_core::account::OpaqueProviderData,
    ) -> futures::future::BoxFuture<
        'a,
        Result<gateway_core::account::OpaqueProviderData, ProviderStoreError>,
    > {
        Box::pin(async move {
            let document = sqlx::query_scalar::<_, sqlx::types::Json<serde_json::Map<String, serde_json::Value>>>(
                "update runtime_settings set
                    provider_request_profiles_json = case when provider_request_profiles_json ? $1 then provider_request_profiles_json
                        else jsonb_set(provider_request_profiles_json, array[$1], $2) end,
                    config_revision = config_revision + case when provider_request_profiles_json ? $1 then 0 else 1 end
                 where id = 1 returning provider_request_profiles_json -> $1"
            )
            .bind(provider.as_str())
            .bind(sqlx::types::Json(initial.expose_to_provider()))
            .fetch_one(&self.pool).await
            .map_err(|_| provider_unavailable("initialize Provider request profile"))?;
            Ok(gateway_core::account::OpaqueProviderData::new(document.0))
        })
    }

    fn load_refresh_policy(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<ProviderRefreshPolicy, ProviderStoreError>> {
        Box::pin(async move {
            let settings = RuntimeSettingsRepository::load_runtime_settings(self)
                .await
                .map_err(|_| provider_unavailable("load refresh policy"))?;
            let concurrency = NonZeroU32::new(settings.refresh_concurrency)
                .ok_or_else(|| provider_invalid("decode refresh policy"))?;
            ProviderRefreshPolicy::try_new(
                Duration::from_secs(settings.refresh_margin_seconds),
                concurrency,
            )
        })
    }

    fn load_freeze_policy(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<ProviderFreezePolicy, ProviderStoreError>> {
        Box::pin(async move {
            let settings = RuntimeSettingsRepository::load_runtime_settings(self)
                .await
                .map_err(|_| provider_unavailable("load freeze policy"))?;
            ProviderFreezePolicy::try_new(
                settings.account_auto_freeze_enabled,
                settings.account_auto_freeze_threshold,
                settings.account_auto_freeze_window_seconds,
                settings.account_auto_freeze_duration_seconds,
                settings.account_auto_freeze_probe_enabled,
                settings.account_auto_freeze_probe_model,
                settings.account_auto_freeze_adaptive_concurrency,
            )
        })
    }
}

pub(crate) async fn load_runtime_settings_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<RuntimeSettings> {
    let row = sqlx::query_as::<_, RuntimeSettingsRow>(
        "select provider_request_profiles_json, config_revision, admin_api_key, refresh_margin_seconds, request_location_json, request_location_enabled,
                refresh_concurrency, max_concurrent_per_account, request_interval_ms,
                rotation_strategy, model_mappings_json, usage_retention_days, ops_event_retention_days,
                audit_retention_days, min_codex_desktop_version,
                min_codex_cli_version, updated_at, responses_max_decompressed_body_bytes, max_waiting_per_key, max_waiting_per_account, concurrency_wait_timeout_seconds,
                account_auto_freeze_enabled, account_auto_freeze_threshold,
                account_auto_freeze_window_seconds, account_auto_freeze_duration_seconds,
                account_auto_freeze_probe_enabled, account_auto_freeze_probe_model,
                account_auto_freeze_adaptive_concurrency
         from runtime_settings where id = 1",
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("load runtime settings in transaction"))?
    .ok_or_else(|| StoreError::NotFound {
        entity: "runtime settings",
        id: "1".to_owned(),
    })?;
    runtime_settings_from_row(row)
}

pub(crate) async fn update_runtime_settings_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    update: &RuntimeSettingsUpdate,
) -> StoreResult<Revision> {
    update.validate()?;
    let refresh_margin_seconds =
        i64::try_from(update.refresh_margin_seconds).map_err(|_| invalid_numeric())?;
    let next = sqlx::query_scalar::<_, i64>(
        "update runtime_settings
             set config_revision = config_revision + 1,
	                 admin_api_key = $1,
	                 refresh_margin_seconds = $2,
	                 refresh_concurrency = $3,
	                 max_concurrent_per_account = $4,
	                 request_interval_ms = $5,
	                 rotation_strategy = $6,
	                 model_mappings_json = $7,
	                 usage_retention_days = $8,
	                 ops_event_retention_days = $9,
	                 audit_retention_days = $10,
	                 min_codex_desktop_version = $11,
	                 min_codex_cli_version = $12,
                     max_waiting_per_key = $13,
                     max_waiting_per_account = $14,
                     concurrency_wait_timeout_seconds = $15,
                     account_auto_freeze_enabled = $16,
                     account_auto_freeze_threshold = $17,
                     account_auto_freeze_window_seconds = $18,
                     account_auto_freeze_duration_seconds = $19,
                     account_auto_freeze_probe_enabled = $20,
                     account_auto_freeze_probe_model = $21,
                     account_auto_freeze_adaptive_concurrency = $22,
                     request_location_json = $23,
                     request_location_enabled = $24,
                     responses_max_decompressed_body_bytes = $25,
                     provider_request_profiles_json = provider_request_profiles_json
                         || case when $26::jsonb is null then '{}'::jsonb else jsonb_build_object('openai', $26::jsonb) end
                         || case when $27::jsonb is null then '{}'::jsonb else jsonb_build_object('xai', $27::jsonb) end,
	                 updated_at = now()
	             where id = 1
	             returning config_revision",
    )
    .bind(update.admin_api_key.as_deref())
    .bind(refresh_margin_seconds)
    .bind(i64::from(update.refresh_concurrency))
    .bind(i64::from(update.max_concurrent_per_account))
    .bind(i64::try_from(update.request_interval_ms).map_err(|_| invalid_numeric())?)
    .bind(&update.rotation_strategy)
    .bind(sqlx::types::Json(&update.model_mappings))
    .bind(i64::from(update.usage_retention_days))
    .bind(i64::from(update.ops_event_retention_days))
    .bind(i64::from(update.audit_retention_days))
    .bind(update.min_codex_desktop_version.as_deref())
    .bind(update.min_codex_cli_version.as_deref())
    .bind(i64::from(update.max_waiting_per_key))
    .bind(i64::from(update.max_waiting_per_account))
    .bind(i64::from(update.concurrency_wait_timeout_seconds))
    .bind(update.account_auto_freeze_enabled)
    .bind(i64::from(update.account_auto_freeze_threshold))
    .bind(i64::try_from(update.account_auto_freeze_window_seconds).map_err(|_| invalid_numeric())?)
    .bind(
        i64::try_from(update.account_auto_freeze_duration_seconds)
            .map_err(|_| invalid_numeric())?,
    )
    .bind(update.account_auto_freeze_probe_enabled)
    .bind(update.account_auto_freeze_probe_model.as_deref())
    .bind(update.account_auto_freeze_adaptive_concurrency)
    .bind(sqlx::types::Json(
        update
            .request_location
            .clone()
            .normalized()
            .map_err(|_| invalid_location())?,
    ))
    .bind(update.request_location_enabled)
    .bind(
        i64::try_from(update.responses_max_decompressed_body_bytes)
            .map_err(|_| invalid_numeric())?,
    )
    .bind(update.openai_client_profile.as_ref().map(|profile| sqlx::types::Json(profile.expose_to_provider())))
    .bind(update.xai_client_profile.as_ref().map(|profile| sqlx::types::Json(profile.expose_to_provider())))
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("update runtime settings in transaction"))?
    .ok_or_else(|| StoreError::NotFound {
        entity: "runtime settings",
        id: "1".to_owned(),
    })?;
    Revision::new(u64::try_from(next).map_err(|_| invalid_numeric())?)
}

pub(crate) async fn bump_config_revision_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<Revision> {
    let next = sqlx::query_scalar::<_, i64>(
        "update runtime_settings
         set config_revision = config_revision + 1, updated_at = now()
         where id = 1
         returning config_revision",
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("bump config revision in transaction"))?
    .ok_or_else(|| StoreError::NotFound {
        entity: "runtime settings",
        id: "1".to_owned(),
    })?;
    Revision::new(u64::try_from(next).map_err(|_| invalid_numeric())?)
}

/// 更新 admin_api_key 字段，config revision 由调用方 bump。
pub(crate) async fn update_admin_api_key_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    admin_api_key: Option<String>,
) -> StoreResult<()> {
    sqlx::query(
        "update runtime_settings
         set admin_api_key = $1,
             updated_at = now()
         where id = 1",
    )
    .bind(admin_api_key.as_deref())
    .execute(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("update admin api key in transaction"))?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct RuntimeSettingsRow {
    provider_request_profiles_json: sqlx::types::Json<
        std::collections::BTreeMap<String, serde_json::Map<String, serde_json::Value>>,
    >,
    config_revision: i64,
    admin_api_key: Option<String>,
    refresh_margin_seconds: i64,
    refresh_concurrency: i64,
    max_concurrent_per_account: i64,
    request_interval_ms: i64,
    rotation_strategy: String,
    request_location_enabled: bool,
    request_location_json: sqlx::types::Json<gateway_core::account::RequestLocation>,
    model_mappings_json: sqlx::types::Json<BTreeMap<String, String>>,
    usage_retention_days: i64,
    ops_event_retention_days: i64,
    audit_retention_days: i64,
    min_codex_desktop_version: Option<String>,
    min_codex_cli_version: Option<String>,
    updated_at: DateTime<Utc>,
    max_waiting_per_key: i64,
    max_waiting_per_account: i64,
    concurrency_wait_timeout_seconds: i64,
    responses_max_decompressed_body_bytes: i64,
    account_auto_freeze_enabled: bool,
    account_auto_freeze_threshold: i64,
    account_auto_freeze_window_seconds: i64,
    account_auto_freeze_duration_seconds: i64,
    account_auto_freeze_probe_enabled: bool,
    account_auto_freeze_probe_model: Option<String>,
    account_auto_freeze_adaptive_concurrency: bool,
}

fn runtime_settings_from_row(mut row: RuntimeSettingsRow) -> StoreResult<RuntimeSettings> {
    Ok(RuntimeSettings {
        openai_client_profile: row
            .provider_request_profiles_json
            .0
            .remove("openai")
            .map(gateway_core::account::OpaqueProviderData::new),
        xai_client_profile: row
            .provider_request_profiles_json
            .0
            .remove("xai")
            .map(gateway_core::account::OpaqueProviderData::new),
        config_revision: Revision::new(to_u64(row.config_revision)?)?,
        admin_api_key: row.admin_api_key,
        refresh_margin_seconds: to_u64(row.refresh_margin_seconds)?,
        refresh_concurrency: to_u32(row.refresh_concurrency)?,
        max_concurrent_per_account: to_u32(row.max_concurrent_per_account)?,
        request_interval_ms: to_u64(row.request_interval_ms)?,
        rotation_strategy: row.rotation_strategy,
        request_location_enabled: row.request_location_enabled,
        request_location: row
            .request_location_json
            .0
            .normalized()
            .map_err(|_| invalid_location())?,
        model_mappings: row.model_mappings_json.0,
        usage_retention_days: to_u32(row.usage_retention_days)?,
        ops_event_retention_days: to_u32(row.ops_event_retention_days)?,
        audit_retention_days: to_u32(row.audit_retention_days)?,
        min_codex_desktop_version: row.min_codex_desktop_version,
        min_codex_cli_version: row.min_codex_cli_version,
        updated_at: row.updated_at,
        max_waiting_per_key: to_u32(row.max_waiting_per_key)?,
        max_waiting_per_account: to_u32(row.max_waiting_per_account)?,
        concurrency_wait_timeout_seconds: to_u32(row.concurrency_wait_timeout_seconds)?,
        responses_max_decompressed_body_bytes: to_u64(row.responses_max_decompressed_body_bytes)?,
        account_auto_freeze_enabled: row.account_auto_freeze_enabled,
        account_auto_freeze_threshold: to_u32(row.account_auto_freeze_threshold)?,
        account_auto_freeze_window_seconds: to_u64(row.account_auto_freeze_window_seconds)?,
        account_auto_freeze_duration_seconds: to_u64(row.account_auto_freeze_duration_seconds)?,
        account_auto_freeze_probe_enabled: row.account_auto_freeze_probe_enabled,
        account_auto_freeze_probe_model: row.account_auto_freeze_probe_model,
        account_auto_freeze_adaptive_concurrency: row.account_auto_freeze_adaptive_concurrency,
    })
}

fn to_u64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| invalid_numeric())
}

fn to_u32(value: i64) -> StoreResult<u32> {
    u32::try_from(value).map_err(|_| invalid_numeric())
}

fn invalid_location() -> StoreError {
    StoreError::InvalidData {
        entity: "runtime settings",
        message: "request location is invalid".to_owned(),
    }
}

fn invalid_numeric() -> StoreError {
    StoreError::InvalidData {
        entity: "runtime settings",
        message: "numeric field is outside its supported range".to_owned(),
    }
}

fn provider_unavailable(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::Unavailable, operation)
}

fn provider_invalid(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::InvalidData, operation)
}

fn valid_model_mappings(mappings: &BTreeMap<String, String>) -> bool {
    mappings.len() <= 512
        && mappings.iter().all(|(requested, upstream)| {
            valid_model_name(requested, 256) && valid_model_name(upstream, 256)
        })
}

fn valid_model_name(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn valid_client_version(value: Option<&str>) -> bool {
    value.is_none_or(|value| CodexClientVersion::parse(value).is_ok())
}

fn valid_probe_model(value: Option<&str>) -> bool {
    value.is_none_or(|value| {
        !value.is_empty()
            && value.len() <= 128
            && value == value.trim()
            && !value.bytes().any(|byte| byte.is_ascii_control())
    })
}
