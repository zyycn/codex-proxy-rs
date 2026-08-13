//! 终态配置表的一致性 `RuntimeSnapshot` 输入读取。

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_core::engine::credential::ProviderAccountId;
use gateway_core::routing::{
    AccountGroupId, ConfigRevision,
    snapshot::{
        SnapshotAccountGroupFacts, SnapshotAccountGroupMemberFacts, SnapshotClientPolicyFacts,
        SnapshotFacts, SnapshotProviderAccountFacts, SnapshotSettingsFacts, SnapshotStoreError,
        SnapshotStorePort,
    },
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::{Revision, StoreError, StoreResult, postgres_unavailable};

use super::ClientApiKeySnapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRuntimeSettings {
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: u32,
    pub request_interval_ms: u64,
    pub rotation_strategy: String,
    pub model_mappings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRevisionVectorEntry {
    pub provider_account_id: String,
    pub credential_revision: Revision,
    pub credential_observed_at: DateTime<Utc>,
    pub quota_observed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSnapshotData {
    pub config_revision: Revision,
    pub observed_current_revision: Revision,
    pub settings: SnapshotRuntimeSettings,
    pub client_api_keys: Vec<ClientApiKeySnapshot>,
    pub account_groups: Vec<SnapshotAccountGroupData>,
    pub provider_accounts: Vec<SnapshotProviderAccountData>,
    pub group_memberships: Vec<SnapshotGroupMembershipData>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAccountGroupData {
    pub id: AccountGroupId,
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotProviderAccountData {
    pub id: String,
    pub provider_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotGroupMembershipData {
    pub group_id: AccountGroupId,
    pub account_id: String,
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
        let client_api_keys = load_client_keys(&mut transaction).await?;
        let account_groups = load_account_groups(&mut transaction).await?;
        let provider_accounts = load_provider_accounts(&mut transaction).await?;
        let group_memberships = load_group_memberships(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|_| postgres_unavailable("commit runtime snapshot"))?;

        let observed_current_revision =
            RuntimeSnapshotRepository::current_config_revision(self).await?;
        Ok(RuntimeSnapshotData {
            config_revision,
            observed_current_revision,
            settings,
            client_api_keys,
            account_groups,
            provider_accounts,
            group_memberships,
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
            "select id, credential_revision, credential_observed_at, quota_observed_at
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
                    credential_observed_at: row.2,
                    quota_observed_at: row.3,
                })
            })
            .collect()
    }
}

impl SnapshotStorePort for PgRuntimeSnapshotRepository {
    fn load_snapshot_facts(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<SnapshotFacts, SnapshotStoreError>> {
        Box::pin(async move {
            let data = self
                .load_runtime_snapshot()
                .await
                .map_err(|_| SnapshotStoreError::unavailable())?;
            let config_revision = core_revision(data.config_revision)?;
            let observed_current_revision = core_revision(data.observed_current_revision)?;
            let settings = SnapshotSettingsFacts::new(
                data.settings.max_concurrent_per_account,
                data.settings.request_interval_ms,
                data.settings.rotation_strategy,
                data.settings.model_mappings,
            );
            let client_policies = data
                .client_api_keys
                .into_iter()
                .map(|key| {
                    SnapshotClientPolicyFacts::new(
                        key.id,
                        key.plaintext_key,
                        key.group_ids,
                        key.limits,
                    )
                })
                .collect();
            let account_groups = data
                .account_groups
                .into_iter()
                .map(|group| SnapshotAccountGroupFacts::new(group.id, group.name, group.enabled))
                .collect();
            let provider_accounts = data
                .provider_accounts
                .into_iter()
                .map(|account| {
                    ProviderAccountId::new(account.id)
                        .map(|id| SnapshotProviderAccountFacts::new(id, account.provider_kind))
                        .map_err(|_| SnapshotStoreError::unavailable())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let group_memberships = data
                .group_memberships
                .into_iter()
                .map(|membership| {
                    ProviderAccountId::new(membership.account_id)
                        .map(|account_id| {
                            SnapshotAccountGroupMemberFacts::new(membership.group_id, account_id)
                        })
                        .map_err(|_| SnapshotStoreError::unavailable())
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(SnapshotFacts::new(
                config_revision,
                observed_current_revision,
                settings,
                client_policies,
                account_groups,
                provider_accounts,
                group_memberships,
            ))
        })
    }

    fn current_config_revision(
        &self,
    ) -> futures::future::BoxFuture<'_, Result<ConfigRevision, SnapshotStoreError>> {
        Box::pin(async move {
            RuntimeSnapshotRepository::current_config_revision(self)
                .await
                .map_err(|_| SnapshotStoreError::unavailable())
                .and_then(core_revision)
        })
    }
}

fn core_revision(revision: Revision) -> Result<ConfigRevision, SnapshotStoreError> {
    ConfigRevision::new(revision.get()).map_err(|_| SnapshotStoreError::unavailable())
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
            sqlx::types::Json<BTreeMap<String, String>>,
        ),
    >(
        "select config_revision, refresh_margin_seconds, refresh_concurrency,
                max_concurrent_per_account, request_interval_ms, rotation_strategy,
                model_mappings_json
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
            model_mappings: row.6.0,
        },
    ))
}

async fn load_client_keys(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<Vec<ClientApiKeySnapshot>> {
    let rows = sqlx::query_as::<_, (String, String, Vec<String>, i64, i64)>(
        "select k.id, k.key,
                coalesce(array_agg(kg.account_group_id order by kg.account_group_id)
                  filter (where kg.account_group_id is not null), '{}') as group_ids,
                k.max_concurrency, k.requests_per_minute
         from client_api_keys k
         left join client_api_key_groups kg on kg.client_api_key_id = k.id
         where k.enabled
         group by k.id
         order by k.id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("load snapshot client policies"))?;
    rows.into_iter()
        .map(|row| ClientApiKeySnapshot::from_persisted(row.0, row.1, row.2, row.3, row.4))
        .collect()
}

async fn load_account_groups(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<Vec<SnapshotAccountGroupData>> {
    let rows = sqlx::query_as::<_, (String, String, bool)>(
        "select id, name, enabled from account_groups order by id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("load snapshot account groups"))?;
    rows.into_iter()
        .map(|(id, name, enabled)| {
            Ok(SnapshotAccountGroupData {
                id: AccountGroupId::new(id).map_err(|_| invalid("invalid account group id"))?,
                name,
                enabled,
            })
        })
        .collect()
}

async fn load_provider_accounts(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<Vec<SnapshotProviderAccountData>> {
    sqlx::query_as::<_, (String, String)>(
        "select id, provider_kind from provider_accounts order by id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("load snapshot provider accounts"))
    .map(|rows| {
        rows.into_iter()
            .map(|(id, provider_kind)| SnapshotProviderAccountData { id, provider_kind })
            .collect()
    })
}

async fn load_group_memberships(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<Vec<SnapshotGroupMembershipData>> {
    let rows = sqlx::query_as::<_, (String, String)>(
        "select account_group_id, provider_account_id
         from account_group_accounts order by account_group_id, provider_account_id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("load snapshot group memberships"))?;
    rows.into_iter()
        .map(|(group_id, account_id)| {
            Ok(SnapshotGroupMembershipData {
                group_id: AccountGroupId::new(group_id)
                    .map_err(|_| invalid("invalid membership group id"))?,
                account_id,
            })
        })
        .collect()
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
