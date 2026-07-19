//! 终态配置表的一致性 `RuntimeSnapshot` 输入读取。

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{Revision, StoreError, StoreResult, postgres_unavailable};

use super::{
    ClientApiKeySnapshot, ProviderAccountAvailability, ProviderAccountSummary,
    ProviderInstanceRecord,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRuntimeSettings {
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: u32,
    pub request_interval_ms: u64,
    pub rotation_strategy: String,
    pub provider_model_mappings: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRevisionVectorEntry {
    pub provider_account_id: String,
    pub credential_revision: Revision,
    pub availability_observed_at: DateTime<Utc>,
    pub quota_observed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSnapshotData {
    pub config_revision: Revision,
    pub observed_current_revision: Revision,
    pub settings: SnapshotRuntimeSettings,
    pub provider_instances: Vec<ProviderInstanceRecord>,
    pub provider_accounts: Vec<ProviderAccountSummary>,
    pub client_api_keys: Vec<ClientApiKeySnapshot>,
}

#[async_trait]
pub trait RuntimeSnapshotRepository: Send + Sync {
    async fn load_runtime_snapshot(&self) -> StoreResult<RuntimeSnapshotData>;
    async fn current_config_revision(&self) -> StoreResult<Revision>;
    async fn credential_revision_vector(&self) -> StoreResult<Vec<CredentialRevisionVectorEntry>>;
}

#[derive(Clone)]
pub struct PgRuntimeSnapshotRepository {
    pool: PgPool,
}

impl PgRuntimeSnapshotRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RuntimeSnapshotRepository for PgRuntimeSnapshotRepository {
    async fn load_runtime_snapshot(&self) -> StoreResult<RuntimeSnapshotData> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin runtime snapshot"))?;
        sqlx::query("set transaction isolation level repeatable read read only")
            .execute(&mut *transaction)
            .await
            .map_err(|_| postgres_unavailable("configure runtime snapshot transaction"))?;

        let (config_revision, settings) = load_settings(&mut transaction).await?;
        let provider_instances = load_instances(&mut transaction).await?;
        let provider_accounts = load_accounts(&mut transaction).await?;
        let client_api_keys = load_client_keys(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|_| postgres_unavailable("commit runtime snapshot"))?;

        let observed_current_revision = self.current_config_revision().await?;
        Ok(RuntimeSnapshotData {
            config_revision,
            observed_current_revision,
            settings,
            provider_instances,
            provider_accounts,
            client_api_keys,
        })
    }

    async fn current_config_revision(&self) -> StoreResult<Revision> {
        let revision = sqlx::query_scalar::<_, i64>(
            "select config_revision from runtime_settings where id = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("read current config revision"))?
        .ok_or_else(|| StoreError::NotFound {
            entity: "runtime settings",
            id: "1".to_owned(),
        })?;
        revision_from_i64(revision)
    }

    async fn credential_revision_vector(&self) -> StoreResult<Vec<CredentialRevisionVectorEntry>> {
        let rows = sqlx::query_as::<_, (String, i64, DateTime<Utc>, Option<DateTime<Utc>>)>(
            "select id, credential_revision, availability_observed_at, quota_observed_at
             from provider_accounts order by id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("read credential revision vector"))?;
        rows.into_iter()
            .map(|row| {
                Ok(CredentialRevisionVectorEntry {
                    provider_account_id: row.0,
                    credential_revision: revision_from_i64(row.1)?,
                    availability_observed_at: row.2,
                    quota_observed_at: row.3,
                })
            })
            .collect()
    }
}

async fn load_settings(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<(Revision, SnapshotRuntimeSettings)> {
    let row = sqlx::query_as::<
        _,
        (
            i64,
            i64,
            i64,
            i64,
            i64,
            String,
            sqlx::types::Json<BTreeMap<String, BTreeMap<String, String>>>,
        ),
    >(
        "select config_revision, refresh_margin_seconds, refresh_concurrency,
                max_concurrent_per_account, request_interval_ms, rotation_strategy,
                provider_model_mappings_json
         from runtime_settings where id = 1",
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("load snapshot settings"))?
    .ok_or_else(|| StoreError::NotFound {
        entity: "runtime settings",
        id: "1".to_owned(),
    })?;
    Ok((
        revision_from_i64(row.0)?,
        SnapshotRuntimeSettings {
            refresh_margin_seconds: to_u64(row.1)?,
            refresh_concurrency: to_u32(row.2)?,
            max_concurrent_per_account: to_u32(row.3)?,
            request_interval_ms: to_u64(row.4)?,
            rotation_strategy: row.5,
            provider_model_mappings: row.6.0,
        },
    ))
}

async fn load_instances(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<Vec<ProviderInstanceRecord>> {
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            String,
            bool,
            DateTime<Utc>,
            DateTime<Utc>,
        ),
    >(
        "select id, provider_kind, name, base_url, enabled, created_at, updated_at
         from provider_instances where enabled order by provider_kind, name, id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("load snapshot provider instances"))?;
    Ok(rows
        .into_iter()
        .map(|row| ProviderInstanceRecord {
            id: row.0,
            provider_kind: row.1,
            name: row.2,
            base_url: row.3,
            enabled: row.4,
            created_at: row.5,
            updated_at: row.6,
        })
        .collect())
}

async fn load_accounts(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<Vec<ProviderAccountSummary>> {
    let rows = sqlx::query(
        "select id, provider_instance_id, provider_kind, name, email, upstream_user_id,
                upstream_account_id, plan_type, credential_revision, has_refresh_token,
                access_token_expires_at, next_refresh_at, enabled, availability,
                availability_reason, cooldown_until, availability_observed_at,
                quota_observed_at, created_at, updated_at
         from provider_accounts where enabled order by provider_kind, provider_instance_id, id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("load snapshot provider accounts"))?;
    rows.into_iter()
        .map(|row| {
            let availability: String = snapshot_get(&row, "availability")?;
            Ok(ProviderAccountSummary {
                id: snapshot_get(&row, "id")?,
                provider_instance_id: snapshot_get(&row, "provider_instance_id")?,
                provider_kind: snapshot_get(&row, "provider_kind")?,
                name: snapshot_get(&row, "name")?,
                email: snapshot_get(&row, "email")?,
                upstream_user_id: snapshot_get(&row, "upstream_user_id")?,
                upstream_account_id: snapshot_get(&row, "upstream_account_id")?,
                plan_type: snapshot_get(&row, "plan_type")?,
                credential_revision: revision_from_i64(snapshot_get(&row, "credential_revision")?)?,
                has_refresh_token: snapshot_get(&row, "has_refresh_token")?,
                access_token_expires_at: snapshot_get(&row, "access_token_expires_at")?,
                next_refresh_at: snapshot_get(&row, "next_refresh_at")?,
                enabled: snapshot_get(&row, "enabled")?,
                availability: parse_availability(&availability)?,
                availability_reason: snapshot_get(&row, "availability_reason")?,
                cooldown_until: snapshot_get(&row, "cooldown_until")?,
                availability_observed_at: snapshot_get(&row, "availability_observed_at")?,
                quota_observed_at: snapshot_get(&row, "quota_observed_at")?,
                created_at: snapshot_get(&row, "created_at")?,
                updated_at: snapshot_get(&row, "updated_at")?,
            })
        })
        .collect()
}

async fn load_client_keys(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<Vec<ClientApiKeySnapshot>> {
    let rows = sqlx::query_as::<_, (String, String, String, i64, i64, i64)>(
        "select id, key, provider_kind, max_concurrency, requests_per_minute, tokens_per_minute
         from client_api_keys where enabled order by id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("load snapshot client policies"))?;
    rows.into_iter()
        .map(|row| ClientApiKeySnapshot::from_persisted(row.0, row.1, row.2, row.3, row.4, row.5))
        .collect()
}

fn parse_availability(value: &str) -> StoreResult<ProviderAccountAvailability> {
    match value {
        "unknown" => Ok(ProviderAccountAvailability::Unknown),
        "ready" => Ok(ProviderAccountAvailability::Ready),
        "cooldown" => Ok(ProviderAccountAvailability::Cooldown),
        "quota_exhausted" => Ok(ProviderAccountAvailability::QuotaExhausted),
        "expired" => Ok(ProviderAccountAvailability::Expired),
        "banned" => Ok(ProviderAccountAvailability::Banned),
        "invalid" => Ok(ProviderAccountAvailability::Invalid),
        _ => Err(invalid("unknown provider account availability")),
    }
}

fn snapshot_get<'r, T>(row: &'r sqlx::postgres::PgRow, column: &'static str) -> StoreResult<T>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get(column).map_err(|_| invalid(column))
}

fn revision_from_i64(value: i64) -> StoreResult<Revision> {
    Revision::new(to_u64(value)?)
}

fn to_u64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| invalid("numeric snapshot field is negative"))
}

fn to_u32(value: i64) -> StoreResult<u32> {
    u32::try_from(value).map_err(|_| invalid("numeric snapshot field is outside u32"))
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "runtime snapshot",
        message: message.to_owned(),
    }
}
