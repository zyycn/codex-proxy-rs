//! `runtime_settings` 单例与 config revision 的 PostgreSQL owner。

use std::{collections::BTreeMap, fmt};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};

use crate::{ConflictKind, Revision, StoreError, StoreResult, postgres_unavailable};

#[derive(Clone, PartialEq, Eq)]
pub struct RuntimeSettings {
    pub config_revision: Revision,
    pub admin_api_key: Option<String>,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: u32,
    pub request_interval_ms: u64,
    pub rotation_strategy: String,
    pub provider_model_mappings: BTreeMap<String, BTreeMap<String, String>>,
    pub usage_retention_days: u32,
    pub ops_event_retention_days: u32,
    pub audit_retention_days: u32,
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
            .field("provider_model_mappings", &self.provider_model_mappings)
            .field("usage_retention_days", &self.usage_retention_days)
            .field("ops_event_retention_days", &self.ops_event_retention_days)
            .field("audit_retention_days", &self.audit_retention_days)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Clone)]
pub struct RuntimeSettingsUpdate {
    pub admin_api_key: Option<String>,
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: u32,
    pub request_interval_ms: u64,
    pub rotation_strategy: String,
    pub provider_model_mappings: BTreeMap<String, BTreeMap<String, String>>,
    pub usage_retention_days: u32,
    pub ops_event_retention_days: u32,
    pub audit_retention_days: u32,
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
            .field("provider_model_mappings", &self.provider_model_mappings)
            .finish_non_exhaustive()
    }
}

impl RuntimeSettingsUpdate {
    pub fn validate(&self) -> StoreResult<()> {
        if self.refresh_margin_seconds == 0
            || self.refresh_concurrency == 0
            || self.max_concurrent_per_account == 0
            || self.usage_retention_days < 31
            || self.ops_event_retention_days == 0
            || self.audit_retention_days == 0
            || !valid_model_mappings(&self.provider_model_mappings)
            || !matches!(
                self.rotation_strategy.as_str(),
                "smart" | "quota_reset_priority" | "round_robin" | "sticky"
            )
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

    async fn update_runtime_settings(
        &self,
        expected_revision: Revision,
        update: RuntimeSettingsUpdate,
    ) -> StoreResult<Revision>;
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
        let row = sqlx::query_as::<
            _,
            (
                i64,
                Option<String>,
                i64,
                i64,
                i64,
                i64,
                String,
                sqlx::types::Json<BTreeMap<String, BTreeMap<String, String>>>,
                i64,
                i64,
                i64,
                DateTime<Utc>,
            ),
        >(
            "select config_revision, admin_api_key, refresh_margin_seconds,
                    refresh_concurrency, max_concurrent_per_account, request_interval_ms,
                    rotation_strategy, provider_model_mappings_json, usage_retention_days, ops_event_retention_days,
                    audit_retention_days, updated_at
             from runtime_settings where id = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("load runtime settings"))?
        .ok_or_else(|| StoreError::NotFound {
            entity: "runtime settings",
            id: "1".to_owned(),
        })?;

        runtime_settings_from_row(row)
    }

    async fn update_runtime_settings(
        &self,
        expected_revision: Revision,
        update: RuntimeSettingsUpdate,
    ) -> StoreResult<Revision> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin runtime settings update"))?;
        let revision =
            update_runtime_settings_in_transaction(&mut transaction, expected_revision, &update)
                .await?;
        transaction
            .commit()
            .await
            .map_err(|_| postgres_unavailable("commit runtime settings update"))?;
        Ok(revision)
    }
}

pub(crate) async fn load_runtime_settings_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<RuntimeSettings> {
    let row = sqlx::query_as::<_, RuntimeSettingsRow>(
        "select config_revision, admin_api_key, refresh_margin_seconds,
                refresh_concurrency, max_concurrent_per_account, request_interval_ms,
                rotation_strategy, provider_model_mappings_json, usage_retention_days, ops_event_retention_days,
                audit_retention_days, updated_at
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
    expected_revision: Revision,
    update: &RuntimeSettingsUpdate,
) -> StoreResult<Revision> {
    update.validate()?;
    let next = sqlx::query_scalar::<_, i64>(
        "update runtime_settings
             set config_revision = config_revision + 1,
                 admin_api_key = $2,
                 refresh_margin_seconds = $3,
                 refresh_concurrency = $4,
                 max_concurrent_per_account = $5,
                 request_interval_ms = $6,
                 rotation_strategy = $7,
                 provider_model_mappings_json = $8,
                 usage_retention_days = $9,
                 ops_event_retention_days = $10,
                 audit_retention_days = $11,
                 updated_at = now()
             where id = 1 and config_revision = $1
             returning config_revision",
    )
    .bind(i64::try_from(expected_revision.get()).map_err(|_| invalid_numeric())?)
    .bind(update.admin_api_key.as_deref())
    .bind(i64::try_from(update.refresh_margin_seconds).map_err(|_| invalid_numeric())?)
    .bind(i64::from(update.refresh_concurrency))
    .bind(i64::from(update.max_concurrent_per_account))
    .bind(i64::try_from(update.request_interval_ms).map_err(|_| invalid_numeric())?)
    .bind(&update.rotation_strategy)
    .bind(sqlx::types::Json(&update.provider_model_mappings))
    .bind(i64::from(update.usage_retention_days))
    .bind(i64::from(update.ops_event_retention_days))
    .bind(i64::from(update.audit_retention_days))
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("update runtime settings in transaction"))?
    .ok_or_else(|| StoreError::Conflict {
        entity: "runtime settings",
        id: "1".to_owned(),
        kind: ConflictKind::StaleRevision,
    })?;
    Revision::new(u64::try_from(next).map_err(|_| invalid_numeric())?)
}

pub(crate) async fn bump_config_revision_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    expected_revision: Revision,
) -> StoreResult<Revision> {
    let next = sqlx::query_scalar::<_, i64>(
        "update runtime_settings
         set config_revision = config_revision + 1, updated_at = now()
         where id = 1 and config_revision = $1
         returning config_revision",
    )
    .bind(i64::try_from(expected_revision.get()).map_err(|_| invalid_numeric())?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("bump config revision in transaction"))?
    .ok_or_else(|| StoreError::Conflict {
        entity: "runtime settings",
        id: "1".to_owned(),
        kind: ConflictKind::StaleRevision,
    })?;
    Revision::new(u64::try_from(next).map_err(|_| invalid_numeric())?)
}

type RuntimeSettingsRow = (
    i64,
    Option<String>,
    i64,
    i64,
    i64,
    i64,
    String,
    sqlx::types::Json<BTreeMap<String, BTreeMap<String, String>>>,
    i64,
    i64,
    i64,
    DateTime<Utc>,
);

fn runtime_settings_from_row(row: RuntimeSettingsRow) -> StoreResult<RuntimeSettings> {
    Ok(RuntimeSettings {
        config_revision: Revision::new(to_u64(row.0)?)?,
        admin_api_key: row.1,
        refresh_margin_seconds: to_u64(row.2)?,
        refresh_concurrency: to_u32(row.3)?,
        max_concurrent_per_account: to_u32(row.4)?,
        request_interval_ms: to_u64(row.5)?,
        rotation_strategy: row.6,
        provider_model_mappings: row.7.0,
        usage_retention_days: to_u32(row.8)?,
        ops_event_retention_days: to_u32(row.9)?,
        audit_retention_days: to_u32(row.10)?,
        updated_at: row.11,
    })
}

fn to_u64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| invalid_numeric())
}

fn to_u32(value: i64) -> StoreResult<u32> {
    u32::try_from(value).map_err(|_| invalid_numeric())
}

fn invalid_numeric() -> StoreError {
    StoreError::InvalidData {
        entity: "runtime settings",
        message: "numeric field is outside its supported range".to_owned(),
    }
}

fn valid_model_mappings(mappings: &BTreeMap<String, BTreeMap<String, String>>) -> bool {
    mappings.len() <= 32
        && mappings.iter().all(|(provider, entries)| {
            valid_slug(provider, 64)
                && entries.len() <= 512
                && entries.iter().all(|(requested, upstream)| {
                    valid_model_name(requested, 256) && valid_model_name(upstream, 256)
                })
        })
}

fn valid_slug(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_model_name(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && !value.bytes().any(|byte| byte.is_ascii_control())
}
