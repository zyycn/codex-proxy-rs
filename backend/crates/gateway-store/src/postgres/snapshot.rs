//! 终态配置表的一致性 `RuntimeSnapshot` 输入读取

use std::collections::BTreeMap;

use async_trait::async_trait;
use gateway_core::account::{FastMode, ProviderAccountId};
use gateway_core::routing::{
    AccountGroupId, ConfigRevision,
    snapshot::{
        SnapshotAccountGroupFacts, SnapshotAccountGroupMemberFacts, SnapshotClientPolicyFacts,
        SnapshotFacts, SnapshotProviderAccountFacts, SnapshotStoreError, SnapshotStorePort,
    },
};
use gateway_core::settings::SettingsValues;
use sqlx::{PgPool, Postgres, Transaction};

use crate::{Revision, StoreError, StoreResult, postgres_unavailable};

use super::ClientApiKeySnapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSnapshotData {
    pub config_revision: Revision,
    pub observed_current_revision: Revision,
    pub settings: SettingsValues,
    pub client_api_keys: Vec<ClientApiKeySnapshot>,
    pub account_groups: Vec<SnapshotAccountGroupData>,
    pub provider_accounts: Vec<SnapshotProviderAccountData>,
    pub group_memberships: Vec<SnapshotGroupMembershipData>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAccountGroupData {
    pub fast_mode: FastMode,
    pub id: AccountGroupId,
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotProviderAccountData {
    pub id: String,
    pub provider_kind: String,
    pub model_access: gateway_core::account::AccountModelAccess,
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
            .map_err(|source| postgres_unavailable("begin runtime snapshot", source))?;
        sqlx::query("set transaction isolation level repeatable read read only")
            .execute(&mut *transaction)
            .await
            .map_err(|source| {
                postgres_unavailable("configure runtime snapshot transaction", source)
            })?;

        let (config_revision, settings) = load_settings(&mut transaction).await?;
        let client_api_keys = load_client_keys(&mut transaction).await?;
        let account_groups = load_account_groups(&mut transaction).await?;
        let provider_accounts = load_provider_accounts(&mut transaction).await?;
        let group_memberships = load_group_memberships(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|source| postgres_unavailable("commit runtime snapshot", source))?;

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
        .map_err(|source| postgres_unavailable("read current config revision", source))?
        .ok_or_else(|| StoreError::NotFound {
            source: None,
            entity: "runtime settings",
            id: "1".to_owned(),
        })?;
        revision_from_i64(revision)
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
            let settings = data.settings;
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
                    .with_request_profiles(key.request_profiles)
                })
                .collect();
            let account_groups = data
                .account_groups
                .into_iter()
                .map(|group| {
                    SnapshotAccountGroupFacts::new(group.id, group.name, group.enabled)
                        .with_fast_mode(group.fast_mode)
                })
                .collect();
            let provider_accounts = data
                .provider_accounts
                .into_iter()
                .map(|account| {
                    ProviderAccountId::new(account.id)
                        .map(|id| {
                            SnapshotProviderAccountFacts::new(id, account.provider_kind)
                                .with_model_access(account.model_access)
                        })
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

#[derive(sqlx::FromRow)]
struct SnapshotSettingsRow {
    pricing_synced_json: sqlx::types::Json<gateway_core::metering::PricingOverrides>,
    pricing_overrides_json: sqlx::types::Json<gateway_core::metering::PricingOverrides>,
    config_revision: i64,
    max_concurrent_per_account: i64,
    request_interval_ms: i64,
    smart_scheduling_json: sqlx::types::Json<gateway_core::account::SmartSchedulingConfig>,
    rotation_strategy: String,
    model_mappings_json: sqlx::types::Json<BTreeMap<String, String>>,
    min_codex_desktop_version: Option<String>,
    min_codex_cli_version: Option<String>,
    max_waiting_per_key: i64,
    max_waiting_per_account: i64,
    concurrency_wait_timeout_seconds: i64,
    openai_guardian_reserved_concurrency: i64,
    openai_account_affinity: String,
    max_account_rotations: i64,
    openai_session_affinity_ttl_hours: i64,
    request_location_json: sqlx::types::Json<gateway_core::account::RequestLocation>,
    codex_privacy_policy_json:
        sqlx::types::Json<gateway_core::settings::privacy::CodexPrivacyPolicy>,
    request_location_enabled: bool,
    responses_max_decompressed_body_bytes: i64,
    provider_request_profiles_json:
        sqlx::types::Json<BTreeMap<String, serde_json::Map<String, serde_json::Value>>>,
}

async fn load_settings(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<(Revision, SettingsValues)> {
    let row = sqlx::query_as::<_, SnapshotSettingsRow>(
        "select config_revision, max_concurrent_per_account, request_interval_ms, rotation_strategy, smart_scheduling_json, model_mappings_json, min_codex_desktop_version, min_codex_cli_version, max_waiting_per_key, max_waiting_per_account, concurrency_wait_timeout_seconds, openai_guardian_reserved_concurrency, openai_account_affinity, max_account_rotations, openai_session_affinity_ttl_hours, request_location_json, request_location_enabled, codex_privacy_policy_json, responses_max_decompressed_body_bytes, provider_request_profiles_json, pricing_overrides_json, pricing_synced_json from runtime_settings where id = 1",
    )
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|source| postgres_unavailable("load snapshot settings", source))?
    .ok_or_else(|| StoreError::NotFound {
        source: None,
        entity: "runtime settings",
        id: "1".to_owned(),
    })?;
    Ok((
        revision_from_i64(row.config_revision)?,
        SettingsValues::new(
            to_u32(row.max_concurrent_per_account)?,
            to_u64(row.request_interval_ms)?,
            row.rotation_strategy,
            row.model_mappings_json.0,
            row.min_codex_desktop_version,
            row.min_codex_cli_version,
        )
        .with_responses_max_decompressed_body_bytes(to_u64(
            row.responses_max_decompressed_body_bytes,
        )?)
        .with_openai_guardian_reserved_concurrency(to_u32(
            row.openai_guardian_reserved_concurrency,
        )?)
        .with_openai_account_affinity(
            gateway_core::account::AccountAffinity::parse(&row.openai_account_affinity)
                .ok_or_else(|| invalid("invalid account affinity"))?,
        )
        .with_max_account_rotations(to_u32(row.max_account_rotations)?)
        .with_openai_session_affinity_ttl_hours(to_u32(row.openai_session_affinity_ttl_hours)?)
        .with_smart_scheduling(row.smart_scheduling_json.0)
        .with_request_profiles(decode_request_profiles(
            row.provider_request_profiles_json.0,
        )?)
        .with_pricing({
            super::pricing::validate_pricing(&row.pricing_synced_json.0)?;
            super::pricing::validate_pricing(&row.pricing_overrides_json.0)?;
            gateway_core::metering::merge_pricing(
                row.pricing_synced_json.0,
                &row.pricing_overrides_json.0,
            )
        })
        .with_request_location(row.request_location_json.0, row.request_location_enabled)
        .with_codex_privacy_policy(row.codex_privacy_policy_json.0)
        .with_concurrency_queues(
            to_u32(row.max_waiting_per_key)?,
            to_u32(row.max_waiting_per_account)?,
            to_u32(row.concurrency_wait_timeout_seconds)?,
        ),
    ))
}

async fn load_client_keys(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<Vec<ClientApiKeySnapshot>> {
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            Vec<String>,
            i64,
            i64,
            sqlx::types::Json<BTreeMap<String, serde_json::Map<String, serde_json::Value>>>,
        ),
    >(
        "select k.id, k.key,
                coalesce(array_agg(kg.account_group_id order by kg.account_group_id)
                  filter (where kg.account_group_id is not null), '{}') as group_ids,
                k.max_concurrency, k.requests_per_minute, k.provider_request_profiles_json
         from client_api_keys k
         left join client_api_key_groups kg on kg.client_api_key_id = k.id
         where k.enabled
         group by k.id
         order by k.id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(|source| postgres_unavailable("load snapshot client policies", source))?;
    rows.into_iter()
        .map(|row| {
            let mut key = ClientApiKeySnapshot::from_persisted(row.0, row.1, row.2, row.3, row.4)?;
            key.request_profiles = decode_request_profiles(row.5.0)?;
            Ok(key)
        })
        .collect()
}

async fn load_account_groups(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<Vec<SnapshotAccountGroupData>> {
    let rows = sqlx::query_as::<_, (String, String, bool, String)>(
        "select id, name, enabled, fast_mode from account_groups order by id",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(|source| postgres_unavailable("load snapshot account groups", source))?;
    rows.into_iter()
        .map(|(id, name, enabled, fast_mode)| {
            Ok(SnapshotAccountGroupData {
                fast_mode: FastMode::parse(&fast_mode)
                    .ok_or_else(|| invalid("invalid fast_mode"))?,
                id: AccountGroupId::new(id)
                    .map_err(|source| invalid("invalid account group id").with_source(source))?,
                name,
                enabled,
            })
        })
        .collect()
}

async fn load_provider_accounts(
    transaction: &mut Transaction<'_, Postgres>,
) -> StoreResult<Vec<SnapshotProviderAccountData>> {
    sqlx::query_as::<
        _,
        (
            String,
            String,
            sqlx::types::Json<gateway_core::account::AccountModelAccess>,
        ),
    >("select id, provider_kind, model_access_json from provider_accounts order by id")
    .fetch_all(&mut **transaction)
    .await
    .map_err(|source| postgres_unavailable("load snapshot provider accounts", source))
    .map(|rows| {
        rows.into_iter()
            .map(
                |(id, provider_kind, model_access)| SnapshotProviderAccountData {
                    id,
                    provider_kind,
                    model_access: model_access.0,
                },
            )
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
    .map_err(|source| postgres_unavailable("load snapshot group memberships", source))?;
    rows.into_iter()
        .map(|(group_id, account_id)| {
            Ok(SnapshotGroupMembershipData {
                group_id: AccountGroupId::new(group_id)
                    .map_err(|source| invalid("invalid membership group id").with_source(source))?,
                account_id,
            })
        })
        .collect()
}

fn revision_from_i64(value: i64) -> StoreResult<Revision> {
    Revision::new(to_u64(value)?)
}

fn to_u64(value: i64) -> StoreResult<u64> {
    u64::try_from(value)
        .map_err(|source| invalid("numeric snapshot field is negative").with_source(source))
}

fn to_u32(value: i64) -> StoreResult<u32> {
    u32::try_from(value)
        .map_err(|source| invalid("numeric snapshot field is outside u32").with_source(source))
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        source: None,
        entity: "runtime snapshot",
        message: message.to_owned(),
    }
}

fn decode_request_profiles(
    profiles: BTreeMap<String, serde_json::Map<String, serde_json::Value>>,
) -> StoreResult<
    BTreeMap<gateway_core::routing::ProviderKind, gateway_core::account::OpaqueProviderData>,
> {
    profiles
        .into_iter()
        .map(|(kind, document)| {
            Ok((
                gateway_core::routing::ProviderKind::new(kind).map_err(|source| {
                    invalid("invalid request profile provider").with_source(source)
                })?,
                gateway_core::account::OpaqueProviderData::new(document),
            ))
        })
        .collect()
}
