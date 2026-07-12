//! 运行时设置 PostgreSQL 存取。

use std::collections::BTreeMap;

use chrono::Utc;
use sqlx::{PgPool, Row, postgres::PgRow};

use super::types::{ManagementApiKeyStatus, SettingsError, SettingsSnapshot};

const RUNTIME_SETTINGS_ID: i64 = 1;

#[derive(Clone)]
pub struct PgSettingsStore {
    pool: PgPool,
}

impl PgSettingsStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn load_or_initialize(
        &self,
        defaults: &SettingsSnapshot,
    ) -> Result<SettingsSnapshot, SettingsError> {
        self.ensure(defaults).await?;
        self.load().await
    }

    pub async fn save(&self, settings: &SettingsSnapshot) -> Result<(), SettingsError> {
        sqlx::query(
            r"
insert into runtime_settings (
  id, model_aliases_json, refresh_margin_seconds, refresh_concurrency,
  max_concurrent_per_account, request_interval_ms, rotation_strategy, updated_at
) values ($1, $2, $3, $4, $5, $6, $7, $8)
on conflict(id) do update set
  model_aliases_json = excluded.model_aliases_json,
  refresh_margin_seconds = excluded.refresh_margin_seconds,
  refresh_concurrency = excluded.refresh_concurrency,
  max_concurrent_per_account = excluded.max_concurrent_per_account,
  request_interval_ms = excluded.request_interval_ms,
  rotation_strategy = excluded.rotation_strategy,
  updated_at = excluded.updated_at",
        )
        .bind(RUNTIME_SETTINGS_ID)
        .bind(sqlx::types::Json(&settings.model_aliases))
        .bind(to_i64(
            "refreshMarginSeconds",
            settings.refresh_margin_seconds,
        )?)
        .bind(i64::from(settings.refresh_concurrency))
        .bind(to_i64(
            "maxConcurrentPerAccount",
            settings.max_concurrent_per_account,
        )?)
        .bind(to_i64("requestIntervalMs", settings.request_interval_ms)?)
        .bind(&settings.rotation_strategy)
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn admin_api_key_status(&self) -> Result<ManagementApiKeyStatus, SettingsError> {
        let key_hash = self.load_admin_api_key_hash().await?;
        Ok(ManagementApiKeyStatus {
            exists: key_hash.is_some_and(|hash| !hash.is_empty()),
        })
    }

    pub async fn load_admin_api_key_hash(&self) -> Result<Option<String>, SettingsError> {
        Ok(sqlx::query_scalar::<_, Option<String>>(
            "select admin_api_key_hash from runtime_settings where id = $1",
        )
        .bind(RUNTIME_SETTINGS_ID)
        .fetch_optional(&self.pool)
        .await?
        .flatten())
    }

    pub async fn set_admin_api_key_hash(&self, key_hash: &str) -> Result<(), SettingsError> {
        sqlx::query(
            "update runtime_settings
             set admin_api_key_hash = $1, updated_at = $2
             where id = $3",
        )
        .bind(key_hash)
        .bind(Utc::now())
        .bind(RUNTIME_SETTINGS_ID)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn clear_admin_api_key_hash(&self) -> Result<(), SettingsError> {
        sqlx::query(
            "update runtime_settings
             set admin_api_key_hash = null, updated_at = $1
             where id = $2",
        )
        .bind(Utc::now())
        .bind(RUNTIME_SETTINGS_ID)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn ensure(&self, settings: &SettingsSnapshot) -> Result<(), SettingsError> {
        sqlx::query(
            r"
insert into runtime_settings (
  id, model_aliases_json, refresh_margin_seconds, refresh_concurrency,
  max_concurrent_per_account, request_interval_ms, rotation_strategy, updated_at
) values ($1, $2, $3, $4, $5, $6, $7, $8)
on conflict (id) do nothing",
        )
        .bind(RUNTIME_SETTINGS_ID)
        .bind(sqlx::types::Json(&settings.model_aliases))
        .bind(to_i64(
            "refreshMarginSeconds",
            settings.refresh_margin_seconds,
        )?)
        .bind(i64::from(settings.refresh_concurrency))
        .bind(to_i64(
            "maxConcurrentPerAccount",
            settings.max_concurrent_per_account,
        )?)
        .bind(to_i64("requestIntervalMs", settings.request_interval_ms)?)
        .bind(&settings.rotation_strategy)
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn load(&self) -> Result<SettingsSnapshot, SettingsError> {
        let row = sqlx::query(
            "select model_aliases_json, refresh_margin_seconds, refresh_concurrency,
                    max_concurrent_per_account, request_interval_ms, rotation_strategy
             from runtime_settings where id = $1",
        )
        .bind(RUNTIME_SETTINGS_ID)
        .fetch_one(&self.pool)
        .await?;
        snapshot_from_row(&row)
    }
}

fn snapshot_from_row(row: &PgRow) -> Result<SettingsSnapshot, SettingsError> {
    let aliases: sqlx::types::Json<BTreeMap<String, String>> = row.get("model_aliases_json");
    Ok(SettingsSnapshot {
        model_aliases: aliases.0,
        refresh_margin_seconds: positive_u64(
            "refreshMarginSeconds",
            row.get("refresh_margin_seconds"),
        )?,
        refresh_concurrency: positive_u32("refreshConcurrency", row.get("refresh_concurrency"))?,
        max_concurrent_per_account: positive_usize(
            "maxConcurrentPerAccount",
            row.get("max_concurrent_per_account"),
        )?,
        request_interval_ms: nonnegative_u64("requestIntervalMs", row.get("request_interval_ms"))?,
        rotation_strategy: row.get("rotation_strategy"),
    })
}

fn to_i64(field: &'static str, value: impl TryInto<i64>) -> Result<i64, SettingsError> {
    value
        .try_into()
        .map_err(|_| stored_field_error(field, "out of range"))
}

fn positive_u64(field: &'static str, value: i64) -> Result<u64, SettingsError> {
    if value <= 0 {
        return Err(stored_field_error(field, "must be greater than 0"));
    }
    u64::try_from(value).map_err(|_| stored_field_error(field, "out of range"))
}

fn positive_u32(field: &'static str, value: i64) -> Result<u32, SettingsError> {
    if value <= 0 {
        return Err(stored_field_error(field, "must be greater than 0"));
    }
    u32::try_from(value).map_err(|_| stored_field_error(field, "out of range"))
}

fn positive_usize(field: &'static str, value: i64) -> Result<usize, SettingsError> {
    if value <= 0 {
        return Err(stored_field_error(field, "must be greater than 0"));
    }
    usize::try_from(value).map_err(|_| stored_field_error(field, "out of range"))
}

fn nonnegative_u64(field: &'static str, value: i64) -> Result<u64, SettingsError> {
    if value < 0 {
        return Err(stored_field_error(
            field,
            "must be greater than or equal to 0",
        ));
    }
    u64::try_from(value).map_err(|_| stored_field_error(field, "out of range"))
}

fn stored_field_error(field: impl Into<String>, message: impl Into<String>) -> SettingsError {
    SettingsError::StoredField {
        field: field.into(),
        message: message.into(),
    }
}
