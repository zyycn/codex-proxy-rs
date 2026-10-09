//! `runtime_settings` 单例与 config revision 的 PostgreSQL owner

use std::num::NonZeroU32;
use std::time::Duration;
use std::{collections::BTreeMap, fmt};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};

use gateway_core::account::RotationStrategy;
use gateway_core::provider_ports::{
    ProviderFreezePolicy, ProviderRefreshPolicy, ProviderRuntimePolicyPort, ProviderStoreError,
    ProviderStoreErrorKind, ProviderWarmupPolicy,
};

use gateway_admin::model::settings::RuntimeSettingsValues;

use crate::{Revision, StoreError, StoreResult, postgres_unavailable};

#[derive(Clone, PartialEq, Eq)]
pub struct RuntimeSettings {
    pub values: RuntimeSettingsValues,
    pub request_profiles:
        BTreeMap<gateway_core::routing::ProviderKind, gateway_core::account::OpaqueProviderData>,
    pub config_revision: Revision,
    pub admin_api_key: Option<String>,
    pub rotation_strategy: String,
    pub model_mappings: BTreeMap<String, String>,
    pub updated_at: DateTime<Utc>,
}

impl fmt::Debug for RuntimeSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeSettings")
            .field("request_profile_count", &self.request_profiles.len())
            .field("config_revision", &self.config_revision)
            .field(
                "admin_api_key",
                &self.admin_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("values", &self.values)
            .field("rotation_strategy", &self.rotation_strategy)
            .field("model_mappings", &self.model_mappings)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Clone)]
pub struct RuntimeSettingsUpdate {
    pub values: RuntimeSettingsValues,
    pub request_profile_updates: BTreeMap<
        gateway_core::routing::ProviderKind,
        Option<gateway_core::account::OpaqueProviderData>,
    >,
    pub rotation_strategy: String,
    pub model_mappings: BTreeMap<String, String>,
}

impl fmt::Debug for RuntimeSettingsUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeSettingsUpdate")
            .field("rotation_strategy", &self.rotation_strategy)
            .field(
                "request_location_enabled",
                &self.values.request_location_enabled,
            )
            .field("request_location", &self.values.request_location)
            .field("model_mappings", &self.model_mappings)
            .finish_non_exhaustive()
    }
}

impl RuntimeSettingsUpdate {
    pub fn validate(&self) -> StoreResult<()> {
        if self.values.validate().is_err()
            || gateway_admin::model::settings::validate_model_mappings(
                self.model_mappings
                    .iter()
                    .map(|(requested, upstream)| (requested.as_str(), upstream.as_str())),
            )
            .is_err()
            || RotationStrategy::parse(&self.rotation_strategy).is_none()
            || self.request_profile_updates.len() > 256
            || self
                .request_profile_updates
                .values()
                .flatten()
                .any(|profile| {
                    serde_json::to_vec(profile.expose_to_provider())
                        .map_or(true, |encoded| encoded.len() > 64 * 1024)
                })
        {
            return Err(StoreError::InvalidData {
                source: None,
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
            .map_err(|source| postgres_unavailable("begin runtime settings update", source))?;
        let revision = update_runtime_settings_in_transaction(&mut transaction, &update).await?;
        transaction
            .commit()
            .await
            .map_err(|source| postgres_unavailable("commit runtime settings update", source))?;
        Ok(revision)
    }
}

pub(crate) async fn load_runtime_settings_from_pool(pool: &PgPool) -> StoreResult<RuntimeSettings> {
    let row = sqlx::query_as::<_, RuntimeSettingsRow>(
            "select provider_request_profiles_json, config_revision, admin_api_key, refresh_margin_seconds, request_location_json, request_location_enabled, codex_privacy_policy_json,
                    refresh_concurrency, max_concurrent_per_account, request_interval_ms,
                    rotation_strategy, smart_scheduling_json, model_mappings_json, usage_retention_days, ops_event_retention_days,
                    audit_retention_days, min_codex_desktop_version,
                    min_codex_cli_version, updated_at, responses_max_decompressed_body_bytes, max_waiting_per_key, max_waiting_per_account, concurrency_wait_timeout_seconds, openai_guardian_reserved_concurrency, openai_account_affinity, max_account_rotations, openai_session_affinity_ttl_hours,
                    account_auto_freeze_enabled, account_auto_freeze_threshold,
                    account_auto_freeze_window_seconds, account_auto_freeze_duration_seconds,
                    account_auto_freeze_probe_enabled, account_auto_freeze_probe_model,
                    account_auto_freeze_adaptive_concurrency,
                    account_warmup_enabled, account_warmup_schedule_time, account_warmup_model
             from runtime_settings where id = 1",
        )
    .fetch_optional(pool)
    .await
    .map_err(|source| postgres_unavailable("load runtime settings", source))?
    .ok_or_else(|| StoreError::NotFound {
        source: None,
        entity: "runtime settings",
        id: "1".to_owned(),
    })?;
    runtime_settings_from_row(row)
}

impl ProviderRuntimePolicyPort for PgRuntimeSettingsRepository {
    fn claim_warmup_slot<'a>(
        &'a self,
        timezone: gateway_core::time::DeploymentTimeZone,
        slot: chrono::NaiveDateTime,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            let slot = timezone.resolve_local(slot).ok_or_else(|| {
                ProviderStoreError::new(ProviderStoreErrorKind::InvalidData, "resolve warmup slot")
            })?;
            // 执行游标只向前推进；设置保存不覆盖它，领取也不发布新的配置版本
            let claimed = sqlx::query(
                "update runtime_settings set account_warmup_cursor = $1
                 where id = 1 and (account_warmup_cursor is null or account_warmup_cursor < $1)",
            )
            .bind(slot)
            .execute(&self.pool)
            .await
            .map_err(|source| crate::provider_unavailable("claim warmup slot", source))?
            .rows_affected()
                == 1;
            Ok(claimed)
        })
    }
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
            .map_err(|source| crate::provider_unavailable("initialize Provider request profile", source))?;
            Ok(gateway_core::account::OpaqueProviderData::new(document.0))
        })
    }

    fn load_request_profile_configurations<'a>(
        &'a self,
        revision: gateway_core::routing::ConfigRevision,
        provider: &'a gateway_core::routing::ProviderKind,
    ) -> futures::future::BoxFuture<
        'a,
        Result<Vec<gateway_core::account::OpaqueProviderData>, ProviderStoreError>,
    > {
        Box::pin(async move {
            // 单条语句共享一个 MVCC 快照：先核对候选 revision，再只投影配置对象；
            // Client Key 明文与其它策略字段不会进入插件准备边界
            let rows = sqlx::query_as::<_, (i64, Option<sqlx::types::Json<serde_json::Value>>)>(
                "select settings.config_revision, profiles.profile
                 from runtime_settings settings
                 cross join lateral (
                   select settings.provider_request_profiles_json -> $1 as profile
                   union
                   select keys.provider_request_profiles_json -> $1
                   from client_api_keys keys
                   where keys.provider_request_profiles_json ? $1
                 ) profiles
                 where settings.id = 1",
            )
            .bind(provider.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|source| {
                crate::provider_unavailable("load Provider request profile configurations", source)
            })?;
            let expected_revision = i64::try_from(revision.get())
                .map_err(|_| provider_invalid("validate Provider request profile revision"))?;
            if rows.is_empty()
                || rows
                    .iter()
                    .any(|(actual_revision, _)| *actual_revision != expected_revision)
            {
                return Err(provider_conflict(
                    "validate Provider request profile revision",
                ));
            }
            rows.into_iter()
                .filter_map(|(_, profile)| profile)
                .map(|profile| {
                    let serde_json::Value::Object(profile) = profile.0 else {
                        return Err(provider_invalid("decode Provider request profile"));
                    };
                    Ok(gateway_core::account::OpaqueProviderData::new(profile))
                })
                .collect()
        })
    }

    fn load_refresh_policy(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<ProviderRefreshPolicy, ProviderStoreError>> {
        Box::pin(async move {
            let settings = RuntimeSettingsRepository::load_runtime_settings(self)
                .await
                .map_err(|source| crate::provider_unavailable("load refresh policy", source))?;
            let concurrency = NonZeroU32::new(settings.values.refresh_concurrency)
                .ok_or_else(|| provider_invalid("decode refresh policy"))?;
            ProviderRefreshPolicy::try_new(
                Duration::from_secs(settings.values.refresh_margin_seconds),
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
                .map_err(|source| crate::provider_unavailable("load freeze policy", source))?;
            ProviderFreezePolicy::try_new(
                settings.values.account_auto_freeze_enabled,
                settings.values.account_auto_freeze_threshold,
                settings.values.account_auto_freeze_window_seconds,
                settings.values.account_auto_freeze_duration_seconds,
                settings.values.account_auto_freeze_probe_enabled,
                settings.values.account_auto_freeze_probe_model,
                settings.values.account_auto_freeze_adaptive_concurrency,
            )
        })
    }

    fn load_warmup_policy(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<ProviderWarmupPolicy, ProviderStoreError>> {
        Box::pin(async move {
            let settings = RuntimeSettingsRepository::load_runtime_settings(self)
                .await
                .map_err(|source| crate::provider_unavailable("load warmup policy", source))?;
            ProviderWarmupPolicy::try_new(
                settings.values.account_warmup_enabled,
                settings.values.account_warmup_schedule_time,
                settings.values.account_warmup_model,
            )
        })
    }
}

pub(crate) async fn load_runtime_settings_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<RuntimeSettings> {
    let row = sqlx::query_as::<_, RuntimeSettingsRow>(
        "select provider_request_profiles_json, config_revision, admin_api_key, refresh_margin_seconds, request_location_json, request_location_enabled, codex_privacy_policy_json,
                refresh_concurrency, max_concurrent_per_account, request_interval_ms,
                rotation_strategy, smart_scheduling_json, model_mappings_json, usage_retention_days, ops_event_retention_days,
                audit_retention_days, min_codex_desktop_version,
                min_codex_cli_version, updated_at, responses_max_decompressed_body_bytes, max_waiting_per_key, max_waiting_per_account, concurrency_wait_timeout_seconds, openai_guardian_reserved_concurrency, openai_account_affinity, max_account_rotations, openai_session_affinity_ttl_hours,
                account_auto_freeze_enabled, account_auto_freeze_threshold,
                account_auto_freeze_window_seconds, account_auto_freeze_duration_seconds,
                account_auto_freeze_probe_enabled, account_auto_freeze_probe_model,
                account_auto_freeze_adaptive_concurrency,
                account_warmup_enabled, account_warmup_schedule_time, account_warmup_model
         from runtime_settings where id = 1",
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|source| postgres_unavailable("load runtime settings in transaction", source))?
    .ok_or_else(|| StoreError::NotFound {
        source: None,
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
        i64::try_from(update.values.refresh_margin_seconds).map_err(|_| invalid_numeric())?;
    let request_profile_updates = update
        .request_profile_updates
        .iter()
        .filter_map(|(provider, profile)| {
            profile.as_ref().map(|profile| {
                (
                    provider.as_str().to_owned(),
                    serde_json::Value::Object(profile.expose_to_provider().clone()),
                )
            })
        })
        .collect::<serde_json::Map<_, _>>();
    let request_profile_deletions = update
        .request_profile_updates
        .iter()
        .filter(|(_, profile)| profile.is_none())
        .map(|(provider, _)| provider.as_str().to_owned())
        .collect::<Vec<_>>();
    let next = sqlx::query_scalar::<_, i64>(
        "update runtime_settings
             set config_revision = config_revision + 1,
	                 refresh_margin_seconds = $1,
	                 refresh_concurrency = $2,
	                 max_concurrent_per_account = $3,
	                 request_interval_ms = $4,
	                 rotation_strategy = $5,
	                 model_mappings_json = $6,
	                 usage_retention_days = $7,
	                 ops_event_retention_days = $8,
	                 audit_retention_days = $9,
	                 min_codex_desktop_version = $10,
	                 min_codex_cli_version = $11,
                     max_waiting_per_key = $12,
                     max_waiting_per_account = $13,
                     concurrency_wait_timeout_seconds = $14,
                     account_auto_freeze_enabled = $15,
                     account_auto_freeze_threshold = $16,
                     account_auto_freeze_window_seconds = $17,
                     account_auto_freeze_duration_seconds = $18,
                     account_auto_freeze_probe_enabled = $19,
                     account_auto_freeze_probe_model = $20,
                     account_auto_freeze_adaptive_concurrency = $21,
                     request_location_json = $22,
                     request_location_enabled = $23,
                     responses_max_decompressed_body_bytes = $24,
	                 provider_request_profiles_json = (provider_request_profiles_json - $25::text[]) || $26::jsonb,
                     account_warmup_enabled = $27,
                     account_warmup_schedule_time = $28,
                     account_warmup_model = $29,
                     smart_scheduling_json = $30,
                     openai_guardian_reserved_concurrency = $31,
                     openai_account_affinity = $32,
                     max_account_rotations = $33,
                     openai_session_affinity_ttl_hours = $34,
                     codex_privacy_policy_json = $35,
	                 updated_at = now()
	             where id = 1
	             returning config_revision",
    )
    .bind(refresh_margin_seconds)
    .bind(i64::from(update.values.refresh_concurrency))
    .bind(i64::from(update.values.max_concurrent_per_account))
    .bind(i64::try_from(update.values.request_interval_ms).map_err(|_| invalid_numeric())?)
    .bind(&update.rotation_strategy)
    .bind(sqlx::types::Json(&update.model_mappings))
    .bind(i64::from(update.values.usage_retention_days))
    .bind(i64::from(update.values.ops_event_retention_days))
    .bind(i64::from(update.values.audit_retention_days))
    .bind(update.values.min_codex_desktop_version.as_deref())
    .bind(update.values.min_codex_cli_version.as_deref())
    .bind(i64::from(update.values.max_waiting_per_key))
    .bind(i64::from(update.values.max_waiting_per_account))
    .bind(i64::from(update.values.concurrency_wait_timeout_seconds))
    .bind(update.values.account_auto_freeze_enabled)
    .bind(i64::from(update.values.account_auto_freeze_threshold))
    .bind(i64::try_from(update.values.account_auto_freeze_window_seconds).map_err(|_| invalid_numeric())?)
    .bind(
        i64::try_from(update.values.account_auto_freeze_duration_seconds)
            .map_err(|_| invalid_numeric())?,
    )
    .bind(update.values.account_auto_freeze_probe_enabled)
    .bind(update.values.account_auto_freeze_probe_model.as_deref())
    .bind(update.values.account_auto_freeze_adaptive_concurrency)
    .bind(sqlx::types::Json(
        update.values.request_location
            .clone()
            .normalized()
            .map_err(|_| invalid_location())?,
    ))
    .bind(update.values.request_location_enabled)
    .bind(
        i64::try_from(update.values.responses_max_decompressed_body_bytes)
            .map_err(|_| invalid_numeric())?,
    )
    .bind(request_profile_deletions)
    .bind(sqlx::types::Json(request_profile_updates))
    .bind(update.values.account_warmup_enabled)
    .bind(&update.values.account_warmup_schedule_time)
    .bind(update.values.account_warmup_model.as_deref())
    .bind(sqlx::types::Json(update.values.smart_scheduling))
    .bind(i64::from(update.values.openai_guardian_reserved_concurrency))
    .bind(update.values.openai_account_affinity.as_str())
    .bind(i64::from(update.values.max_account_rotations))
    .bind(i64::from(update.values.openai_session_affinity_ttl_hours))
    .bind(sqlx::types::Json(&update.values.codex_privacy_policy))
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|source| postgres_unavailable("update runtime settings in transaction", source))?
    .ok_or_else(|| StoreError::NotFound {
        source: None,
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
    .map_err(|source| postgres_unavailable("bump config revision in transaction", source))?
    .ok_or_else(|| StoreError::NotFound {
        source: None,
        entity: "runtime settings",
        id: "1".to_owned(),
    })?;
    Revision::new(u64::try_from(next).map_err(|_| invalid_numeric())?)
}

/// 更新 admin_api_key 字段，config revision 由调用方 bump
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
    .map_err(|source| postgres_unavailable("update admin api key in transaction", source))?;
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
    smart_scheduling_json: sqlx::types::Json<gateway_core::account::SmartSchedulingConfig>,
    rotation_strategy: String,
    codex_privacy_policy_json:
        sqlx::types::Json<gateway_core::settings::privacy::CodexPrivacyPolicy>,
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
    openai_guardian_reserved_concurrency: i64,
    openai_account_affinity: String,
    max_account_rotations: i64,
    openai_session_affinity_ttl_hours: i64,
    responses_max_decompressed_body_bytes: i64,
    account_auto_freeze_enabled: bool,
    account_auto_freeze_threshold: i64,
    account_auto_freeze_window_seconds: i64,
    account_auto_freeze_duration_seconds: i64,
    account_auto_freeze_probe_enabled: bool,
    account_auto_freeze_probe_model: Option<String>,
    account_auto_freeze_adaptive_concurrency: bool,
    account_warmup_enabled: bool,
    account_warmup_schedule_time: String,
    account_warmup_model: Option<String>,
}

fn runtime_settings_from_row(row: RuntimeSettingsRow) -> StoreResult<RuntimeSettings> {
    let request_profiles = row
        .provider_request_profiles_json
        .0
        .into_iter()
        .map(|(provider, profile)| {
            let provider = gateway_core::routing::ProviderKind::new(provider)
                .map_err(|_| invalid_request_profile())?;
            Ok((
                provider,
                gateway_core::account::OpaqueProviderData::new(profile),
            ))
        })
        .collect::<StoreResult<BTreeMap<_, _>>>()?;
    Ok(RuntimeSettings {
        request_profiles,
        config_revision: Revision::new(to_u64(row.config_revision)?)?,
        admin_api_key: row.admin_api_key,
        rotation_strategy: row.rotation_strategy,
        model_mappings: row.model_mappings_json.0,
        updated_at: row.updated_at,
        values: gateway_admin::model::settings::RuntimeSettingsValues {
            refresh_margin_seconds: to_u64(row.refresh_margin_seconds)?,
            refresh_concurrency: to_u32(row.refresh_concurrency)?,
            max_concurrent_per_account: to_u32(row.max_concurrent_per_account)?,
            request_interval_ms: to_u64(row.request_interval_ms)?,
            smart_scheduling: row.smart_scheduling_json.0,
            codex_privacy_policy: row.codex_privacy_policy_json.0,
            request_location_enabled: row.request_location_enabled,
            request_location: row
                .request_location_json
                .0
                .normalized()
                .map_err(|_| invalid_location())?,
            usage_retention_days: to_u32(row.usage_retention_days)?,
            ops_event_retention_days: to_u32(row.ops_event_retention_days)?,
            audit_retention_days: to_u32(row.audit_retention_days)?,
            min_codex_desktop_version: row.min_codex_desktop_version,
            min_codex_cli_version: row.min_codex_cli_version,
            max_waiting_per_key: to_u32(row.max_waiting_per_key)?,
            max_waiting_per_account: to_u32(row.max_waiting_per_account)?,
            concurrency_wait_timeout_seconds: to_u32(row.concurrency_wait_timeout_seconds)?,
            openai_guardian_reserved_concurrency: to_u32(row.openai_guardian_reserved_concurrency)?,
            openai_account_affinity: gateway_core::account::AccountAffinity::parse(
                &row.openai_account_affinity,
            )
            .ok_or_else(|| StoreError::InvalidData {
                entity: "runtime settings",
                message: "invalid account affinity".to_owned(),
                source: None,
            })?,
            max_account_rotations: to_u32(row.max_account_rotations)?,
            openai_session_affinity_ttl_hours: to_u32(row.openai_session_affinity_ttl_hours)?,
            responses_max_decompressed_body_bytes: to_u64(
                row.responses_max_decompressed_body_bytes,
            )?,
            account_auto_freeze_enabled: row.account_auto_freeze_enabled,
            account_auto_freeze_threshold: to_u32(row.account_auto_freeze_threshold)?,
            account_auto_freeze_window_seconds: to_u64(row.account_auto_freeze_window_seconds)?,
            account_auto_freeze_duration_seconds: to_u64(row.account_auto_freeze_duration_seconds)?,
            account_auto_freeze_probe_enabled: row.account_auto_freeze_probe_enabled,
            account_auto_freeze_probe_model: row.account_auto_freeze_probe_model,
            account_auto_freeze_adaptive_concurrency: row.account_auto_freeze_adaptive_concurrency,
            account_warmup_enabled: row.account_warmup_enabled,
            account_warmup_schedule_time: row.account_warmup_schedule_time,
            account_warmup_model: row.account_warmup_model,
        },
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
        source: None,
        entity: "runtime settings",
        message: "request location is invalid".to_owned(),
    }
}

fn invalid_request_profile() -> StoreError {
    StoreError::InvalidData {
        source: None,
        entity: "runtime settings",
        message: "Provider request profile key is invalid".to_owned(),
    }
}

fn invalid_numeric() -> StoreError {
    StoreError::InvalidData {
        source: None,
        entity: "runtime settings",
        message: "numeric field is outside its supported range".to_owned(),
    }
}

fn provider_invalid(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::InvalidData, operation)
}

fn provider_conflict(operation: &'static str) -> ProviderStoreError {
    ProviderStoreError::new(ProviderStoreErrorKind::Conflict, operation)
}
