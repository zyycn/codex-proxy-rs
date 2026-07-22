//! 明文 `provider_accounts` 与凭证 revision CAS 的唯一 PostgreSQL owner。

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use gateway_admin::{
    model::{
        MutationContext, Revision as AdminRevision,
        accounts::{
            AccountAvailability as AdminAccountAvailability, AccountCost,
            AccountListQuery as AdminAccountListQuery, AccountModelUsage, AccountPage,
            AccountRecord, AccountSort as AdminAccountSort,
            AccountSortField as AdminAccountSortField, AccountStatus as AdminAccountStatus,
            AccountSummary, AccountUsage, DeleteAccounts, SetAccountEnabled,
            SortDirection as AdminSortDirection,
        },
        observability::{
            CostCoverage as AdminCostCoverage, DecimalAmount as AdminDecimalAmount, TimeRange,
        },
        provider_credentials::{
            AuthorizationCommit, AuthorizationCredentialCommit, CredentialCursor,
            CredentialDetails, CredentialImportCommit, CredentialImportResult, CredentialListQuery,
            CredentialListWindow, CredentialMutationResult, CredentialPage,
            CredentialRotationCommit, PreparedCredentialCreate, PreparedCredentialImport,
            PreparedCredentialRotationFacts, ProviderDocument, ProviderExportCredentialInput,
        },
    },
    ports::store::{AccountStore, AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
};
use sqlx::{PgPool, Postgres, Row, Transaction};

use gateway_core::engine::credential::{
    AccountAvailability as CoreAccountAvailability, AccountStateChange, CredentialCasOutcome,
    CredentialCasUpdate, CredentialRevision as CoreCredentialRevision, LoadedCredential,
    NewProviderAccount as CoreNewProviderAccount, OpaqueProviderData, PlaintextCredential,
    ProviderAccount as CoreProviderAccount, ProviderAccountId as CoreProviderAccountId,
    ProviderAccountStore, ProviderAccountUpdate as CoreProviderAccountUpdate, QuotaObservation,
    QuotaWriteOutcome,
};
use gateway_core::error::{StoreError as CoreStoreError, StoreErrorKind as CoreStoreErrorKind};
use gateway_core::routing::ProviderKind;

use crate::{
    ConflictKind, JsonObject, Revision, StoreError, StoreResult, admin_revision, admin_store_error,
    mutation_audit, postgres_unavailable, require_nonempty, store_revision,
};

use super::{
    AdminAuditEvent, ControlPlaneRepository, CurrencyCostTotal, ObservabilityRange,
    ObservabilityRepository, PgControlPlaneRepository, PgObservabilityRepository,
    ProviderAccountModelUsageObservation, ProviderAccountUsageObservation,
    ProviderAccountUsageQuery, append_admin_audit_event_in_transaction,
    bump_config_revision_in_transaction,
};

const ENTITY: &str = "provider account";
const CREDENTIALS_MAX_BYTES: usize = 256 * 1024;
const QUOTA_MAX_BYTES: usize = 128 * 1024;
const MAX_ADMIN_IMPORT_BATCH: usize = 200;
const ADMIN_USAGE_CHUNK_SIZE: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountAdminScope {
    pub provider_kind: String,
}

impl ProviderAccountAdminScope {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "provider_kind", &self.provider_kind)
    }

    fn contains(&self, account: &NewProviderAccount) -> bool {
        account.provider_kind == self.provider_kind
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAccountAvailability {
    Unknown,
    Ready,
    Cooldown,
    QuotaExhausted,
    Expired,
    Banned,
    Invalid,
}

impl ProviderAccountAvailability {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Ready => "ready",
            Self::Cooldown => "cooldown",
            Self::QuotaExhausted => "quota_exhausted",
            Self::Expired => "expired",
            Self::Banned => "banned",
            Self::Invalid => "invalid",
        }
    }

    fn parse(value: &str) -> StoreResult<Self> {
        match value {
            "unknown" => Ok(Self::Unknown),
            "ready" => Ok(Self::Ready),
            "cooldown" => Ok(Self::Cooldown),
            "quota_exhausted" => Ok(Self::QuotaExhausted),
            "expired" => Ok(Self::Expired),
            "banned" => Ok(Self::Banned),
            "invalid" => Ok(Self::Invalid),
            _ => Err(invalid("unknown availability value")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountSummary {
    pub id: String,
    pub provider_kind: String,
    pub name: String,
    pub email: Option<String>,
    pub upstream_user_id: String,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    pub credential_revision: Revision,
    pub has_refresh_token: bool,
    pub access_token_expires_at: DateTime<Utc>,
    pub next_refresh_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub availability: ProviderAccountAvailability,
    pub availability_reason: Option<String>,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub availability_observed_at: DateTime<Utc>,
    pub quota_observed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, PartialEq)]
pub struct ProviderAccountRecord {
    pub summary: ProviderAccountSummary,
    pub provider_credentials_json: JsonObject,
    pub provider_quota_json: Option<JsonObject>,
}

impl fmt::Debug for ProviderAccountRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderAccountRecord")
            .field("summary", &self.summary)
            .field("provider_credentials_json", &"[REDACTED]")
            .field(
                "provider_quota_json",
                &self.provider_quota_json.as_ref().map(|_| "[PROVIDER JSON]"),
            )
            .finish()
    }
}

#[derive(Clone)]
pub struct NewProviderAccount {
    pub id: String,
    pub provider_kind: String,
    pub name: String,
    pub email: Option<String>,
    pub upstream_user_id: String,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    pub provider_credentials_json: JsonObject,
    pub has_refresh_token: bool,
    pub access_token_expires_at: DateTime<Utc>,
    pub next_refresh_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub availability: ProviderAccountAvailability,
    pub availability_reason: Option<String>,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub availability_observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateProviderAccount {
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

impl fmt::Debug for NewProviderAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewProviderAccount")
            .field("id", &self.id)
            .field("provider_kind", &self.provider_kind)
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("availability", &self.availability)
            .field("cooldown_until", &self.cooldown_until)
            .field("provider_credentials_json", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl NewProviderAccount {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "id", &self.id)?;
        require_nonempty(ENTITY, "provider_kind", &self.provider_kind)?;
        require_nonempty(ENTITY, "name", &self.name)?;
        require_nonempty(ENTITY, "upstream_user_id", &self.upstream_user_id)?;
        if !self.has_refresh_token && self.next_refresh_at.is_some() {
            return Err(invalid("next_refresh_at requires a refresh token"));
        }
        if (self.availability == ProviderAccountAvailability::Cooldown)
            != self.cooldown_until.is_some()
        {
            return Err(invalid(
                "cooldown_until must be present exactly for cooldown availability",
            ));
        }
        validate_object_size(
            "provider_credentials_json",
            &self.provider_credentials_json,
            CREDENTIALS_MAX_BYTES,
        )
    }
}

#[derive(Clone)]
pub struct ImportProviderAccounts {
    pub scope: ProviderAccountAdminScope,
    pub accounts: Vec<NewProviderAccount>,
    pub audit: AdminAuditEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountAdminImport {
    pub config_revision: Revision,
    pub account_ids: Vec<String>,
}

impl fmt::Debug for ImportProviderAccounts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImportProviderAccounts")
            .field("scope", &self.scope)
            .field(
                "account_ids",
                &self
                    .accounts
                    .iter()
                    .map(|account| account.id.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("audit", &self.audit)
            .finish()
    }
}

impl ImportProviderAccounts {
    pub fn validate(&self) -> StoreResult<()> {
        self.scope.validate()?;
        if self.accounts.is_empty() || self.accounts.len() > MAX_ADMIN_IMPORT_BATCH {
            return Err(invalid(
                "admin import batch must contain between 1 and 200 accounts",
            ));
        }
        let mut ids = BTreeSet::new();
        for account in &self.accounts {
            account.validate()?;
            if !self.scope.contains(account) {
                return Err(invalid(
                    "imported account is outside the Provider admin scope",
                ));
            }
            if !ids.insert(account.id.as_str()) {
                return Err(invalid("admin import batch contains duplicate account IDs"));
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct RotateProviderAccount {
    pub scope: ProviderAccountAdminScope,
    pub profile: UpdateProviderAccount,
    pub credential: ProviderCredentialUpdate,
    pub audit: AdminAuditEvent,
}

impl fmt::Debug for RotateProviderAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RotateProviderAccount")
            .field("scope", &self.scope)
            .field("profile", &self.profile)
            .field("credential", &self.credential)
            .field("audit", &self.audit)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct SetProviderAccountEnabled {
    pub scope: ProviderAccountAdminScope,
    pub account_id: String,
    pub enabled: bool,
    pub audit: AdminAuditEvent,
}

#[derive(Debug, Clone)]
pub struct DeleteProviderAccounts {
    pub scope: ProviderAccountAdminScope,
    pub account_ids: Vec<String>,
    pub audit: AdminAuditEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderAccountAdminRotation {
    pub config_revision: Revision,
    pub credential_revision: Revision,
}

#[derive(Clone)]
pub struct ProviderCredentialUpdate {
    pub account_id: String,
    pub expected_revision: Revision,
    pub provider_credentials_json: JsonObject,
    pub has_refresh_token: bool,
    pub access_token_expires_at: DateTime<Utc>,
    pub next_refresh_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for ProviderCredentialUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderCredentialUpdate")
            .field("account_id", &self.account_id)
            .field("expected_revision", &self.expected_revision)
            .field("provider_credentials_json", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderAccountObservation {
    pub account_id: String,
    pub availability: ProviderAccountAvailability,
    pub availability_reason: Option<String>,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub provider_quota_json: Option<JsonObject>,
    pub availability_observed_at: DateTime<Utc>,
    pub quota_observed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountStateUpdate {
    pub account_id: String,
    pub expected_revision: Revision,
    pub availability: ProviderAccountAvailability,
    pub availability_reason: Option<String>,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub availability_observed_at: DateTime<Utc>,
}

impl ProviderAccountStateUpdate {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "account_id", &self.account_id)?;
        if (self.availability == ProviderAccountAvailability::Cooldown)
            != self.cooldown_until.is_some()
        {
            return Err(invalid(
                "cooldown availability and cooldown_until must agree",
            ));
        }
        Ok(())
    }
}

impl ProviderAccountObservation {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "account_id", &self.account_id)?;
        if (self.availability == ProviderAccountAvailability::Cooldown)
            != self.cooldown_until.is_some()
        {
            return Err(invalid(
                "cooldown availability and cooldown_until must agree",
            ));
        }
        if self.provider_quota_json.is_some() != self.quota_observed_at.is_some() {
            return Err(invalid("quota JSON and quota_observed_at must agree"));
        }
        if let Some(quota) = &self.provider_quota_json {
            validate_object_size("provider_quota_json", quota, QUOTA_MAX_BYTES)?;
        }
        Ok(())
    }
}

#[async_trait]
pub trait ProviderAccountRepository: Send + Sync {
    async fn load_provider_account(&self, id: &str) -> StoreResult<Option<ProviderAccountRecord>>;
    async fn list_provider_accounts(
        &self,
        provider_kind: Option<&str>,
        include_disabled: bool,
    ) -> StoreResult<Vec<ProviderAccountSummary>>;
    async fn insert_provider_account(&self, account: NewProviderAccount) -> StoreResult<()>;
    async fn update_provider_account(&self, account: UpdateProviderAccount) -> StoreResult<bool>;
    async fn compare_and_swap_credentials(
        &self,
        update: ProviderCredentialUpdate,
    ) -> StoreResult<Revision>;
    async fn update_provider_account_observation(
        &self,
        observation: ProviderAccountObservation,
    ) -> StoreResult<bool>;
    async fn apply_provider_account_state(
        &self,
        update: ProviderAccountStateUpdate,
    ) -> StoreResult<bool>;
    async fn set_provider_account_enabled(&self, id: &str, enabled: bool) -> StoreResult<bool>;
    async fn compare_and_swap_provider_quota(
        &self,
        account_id: &str,
        expected_revision: Revision,
        quota: Option<JsonObject>,
        observed_at: Option<DateTime<Utc>>,
    ) -> StoreResult<bool>;
    async fn delete_provider_account(&self, id: &str) -> StoreResult<bool>;
}

#[async_trait]
pub trait ProviderAccountAdminRepository: Send + Sync {
    async fn export_provider_accounts(
        &self,
        scope: ProviderAccountAdminScope,
        account_ids: Vec<String>,
    ) -> StoreResult<Vec<ProviderAccountRecord>>;

    async fn import_provider_accounts(
        &self,
        expected_config_revision: Revision,
        command: ImportProviderAccounts,
    ) -> StoreResult<ProviderAccountAdminImport>;

    async fn rotate_provider_account(
        &self,
        expected_config_revision: Revision,
        command: RotateProviderAccount,
    ) -> StoreResult<ProviderAccountAdminRotation>;

    async fn set_provider_account_enabled_admin(
        &self,
        expected_config_revision: Revision,
        command: SetProviderAccountEnabled,
    ) -> StoreResult<Revision>;

    async fn delete_provider_accounts_admin(
        &self,
        expected_config_revision: Revision,
        command: DeleteProviderAccounts,
    ) -> StoreResult<Revision>;
}

#[derive(Clone)]
pub struct PgProviderAccountRepository {
    pool: PgPool,
}

impl PgProviderAccountRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ProviderAccountRepository for PgProviderAccountRepository {
    async fn load_provider_account(&self, id: &str) -> StoreResult<Option<ProviderAccountRecord>> {
        require_nonempty(ENTITY, "id", id)?;
        let row = sqlx::query(ACCOUNT_SELECT)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("load provider account"))?;
        row.map(account_record_from_row).transpose()
    }

    async fn list_provider_accounts(
        &self,
        provider_kind: Option<&str>,
        include_disabled: bool,
    ) -> StoreResult<Vec<ProviderAccountSummary>> {
        let rows = sqlx::query(
            "select id, provider_kind, name, email, upstream_user_id,
                    upstream_account_id, plan_type, credential_revision, has_refresh_token,
                    access_token_expires_at, next_refresh_at, enabled, availability,
                    availability_reason, cooldown_until, availability_observed_at,
                    quota_observed_at, created_at, updated_at
             from provider_accounts
             where ($1::text is null or provider_kind = $1) and ($2 or enabled)
             order by provider_kind, name, id",
        )
        .bind(provider_kind)
        .bind(include_disabled)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("list provider accounts"))?;
        rows.into_iter().map(account_summary_from_row).collect()
    }

    async fn insert_provider_account(&self, account: NewProviderAccount) -> StoreResult<()> {
        account.validate()?;
        sqlx::query(
            "insert into provider_accounts (
               id, provider_kind, name, email, upstream_user_id,
               upstream_account_id, plan_type, provider_credentials_json, credential_revision,
               has_refresh_token, access_token_expires_at, next_refresh_at, enabled,
               availability, availability_reason, cooldown_until, provider_quota_json,
               availability_observed_at, quota_observed_at, created_at, updated_at
             ) values (
               $1, $2, $3, $4, $5, $6, $7, $8, 1, $9, $10, $11, $12,
               $13, null, $14, null, $15, null, now(), greatest(now(), $15)
             )",
        )
        .bind(account.id)
        .bind(account.provider_kind)
        .bind(account.name)
        .bind(account.email)
        .bind(account.upstream_user_id)
        .bind(account.upstream_account_id)
        .bind(account.plan_type)
        .bind(account.provider_credentials_json.as_value())
        .bind(account.has_refresh_token)
        .bind(account.access_token_expires_at)
        .bind(account.next_refresh_at)
        .bind(account.enabled)
        .bind(account.availability.as_str())
        .bind(account.cooldown_until)
        .bind(account.availability_observed_at)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("insert provider account"))?;
        Ok(())
    }

    async fn update_provider_account(&self, account: UpdateProviderAccount) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", &account.id)?;
        require_nonempty(ENTITY, "name", &account.name)?;
        let result = sqlx::query(
            "update provider_accounts
             set name = $2, email = $3, plan_type = $4, updated_at = now()
             where id = $1",
        )
        .bind(account.id)
        .bind(account.name)
        .bind(account.email)
        .bind(account.plan_type)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("update provider account"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn compare_and_swap_credentials(
        &self,
        update: ProviderCredentialUpdate,
    ) -> StoreResult<Revision> {
        require_nonempty(ENTITY, "account_id", &update.account_id)?;
        validate_object_size(
            "provider_credentials_json",
            &update.provider_credentials_json,
            CREDENTIALS_MAX_BYTES,
        )?;
        if !update.has_refresh_token && update.next_refresh_at.is_some() {
            return Err(invalid("next_refresh_at requires a refresh token"));
        }
        let next = sqlx::query_scalar::<_, i64>(
            "update provider_accounts
             set provider_credentials_json = $3,
                 credential_revision = credential_revision + 1,
                 has_refresh_token = $4,
                 access_token_expires_at = $5,
                 next_refresh_at = $6,
                 updated_at = now()
             where id = $1 and credential_revision = $2
             returning credential_revision",
        )
        .bind(&update.account_id)
        .bind(to_i64(update.expected_revision.get())?)
        .bind(update.provider_credentials_json.as_value())
        .bind(update.has_refresh_token)
        .bind(update.access_token_expires_at)
        .bind(update.next_refresh_at)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("compare and swap provider credentials"))?
        .ok_or(StoreError::Conflict {
            entity: ENTITY,
            id: update.account_id,
            kind: ConflictKind::StaleRevision,
        })?;
        Revision::new(to_u64(next)?)
    }

    async fn update_provider_account_observation(
        &self,
        observation: ProviderAccountObservation,
    ) -> StoreResult<bool> {
        observation.validate()?;
        let result = sqlx::query(
            "update provider_accounts
             set availability = $2, availability_reason = $3, cooldown_until = $4,
                 provider_quota_json = $5, availability_observed_at = $6,
                 quota_observed_at = $7,
                 updated_at = greatest(now(), $6, coalesce($7, $6))
             where id = $1",
        )
        .bind(observation.account_id)
        .bind(observation.availability.as_str())
        .bind(observation.availability_reason)
        .bind(observation.cooldown_until)
        .bind(
            observation
                .provider_quota_json
                .map(|quota| quota.as_value()),
        )
        .bind(observation.availability_observed_at)
        .bind(observation.quota_observed_at)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("update provider account observation"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn apply_provider_account_state(
        &self,
        update: ProviderAccountStateUpdate,
    ) -> StoreResult<bool> {
        update.validate()?;
        let result = sqlx::query(
            "update provider_accounts
             set availability = $3, availability_reason = $4, cooldown_until = $5,
                 availability_observed_at = $6, updated_at = greatest(now(), $6)
             where id = $1 and credential_revision = $2",
        )
        .bind(update.account_id)
        .bind(to_i64(update.expected_revision.get())?)
        .bind(update.availability.as_str())
        .bind(update.availability_reason)
        .bind(update.cooldown_until)
        .bind(update.availability_observed_at)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("apply provider account state"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn set_provider_account_enabled(&self, id: &str, enabled: bool) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", id)?;
        let result = sqlx::query(
            "update provider_accounts set enabled = $2, updated_at = now() where id = $1",
        )
        .bind(id)
        .bind(enabled)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("set provider account enabled state"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn compare_and_swap_provider_quota(
        &self,
        account_id: &str,
        expected_revision: Revision,
        quota: Option<JsonObject>,
        observed_at: Option<DateTime<Utc>>,
    ) -> StoreResult<bool> {
        require_nonempty(ENTITY, "account_id", account_id)?;
        if quota.is_some() != observed_at.is_some() {
            return Err(invalid("quota JSON and observed_at must agree"));
        }
        if let Some(quota) = quota.as_ref() {
            validate_object_size("provider_quota_json", quota, QUOTA_MAX_BYTES)?;
        }
        let result = sqlx::query(
            "update provider_accounts
             set provider_quota_json = $3, quota_observed_at = $4,
                 updated_at = greatest(now(), coalesce($4, now()))
             where id = $1 and credential_revision = $2",
        )
        .bind(account_id)
        .bind(to_i64(expected_revision.get())?)
        .bind(quota.map(|value| value.as_value()))
        .bind(observed_at)
        .execute(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("compare and swap provider quota"))?;
        Ok(result.rows_affected() == 1)
    }

    async fn delete_provider_account(&self, id: &str) -> StoreResult<bool> {
        require_nonempty(ENTITY, "id", id)?;
        let result = sqlx::query("delete from provider_accounts where id = $1 and not enabled")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("delete disabled provider account"))?;
        Ok(result.rows_affected() == 1)
    }
}

#[async_trait]
impl ProviderAccountAdminRepository for PgProviderAccountRepository {
    async fn export_provider_accounts(
        &self,
        scope: ProviderAccountAdminScope,
        account_ids: Vec<String>,
    ) -> StoreResult<Vec<ProviderAccountRecord>> {
        scope.validate()?;
        validate_admin_account_ids(&account_ids)?;
        let rows = sqlx::query(ACCOUNT_SELECT_BY_IDS)
            .bind(&account_ids)
            .bind(&scope.provider_kind)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| postgres_unavailable("export provider accounts"))?;
        let records = rows
            .into_iter()
            .map(account_record_from_row)
            .collect::<StoreResult<Vec<_>>>()?;
        if records.len() != account_ids.len() {
            return Err(invalid(
                "one or more exported accounts are missing or outside the Provider scope",
            ));
        }
        let by_id = records
            .into_iter()
            .map(|record| (record.summary.id.clone(), record))
            .collect::<std::collections::HashMap<_, _>>();
        account_ids
            .into_iter()
            .map(|id| {
                by_id.get(&id).cloned().ok_or_else(|| {
                    invalid("one or more exported accounts are missing after loading")
                })
            })
            .collect()
    }

    async fn import_provider_accounts(
        &self,
        expected_config_revision: Revision,
        command: ImportProviderAccounts,
    ) -> StoreResult<ProviderAccountAdminImport> {
        command.validate()?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin import"))?;
        let result = async {
            let revision =
                bump_config_revision_in_transaction(&mut transaction, expected_config_revision)
                    .await?;
            let mut account_ids = Vec::with_capacity(command.accounts.len());
            for account in &command.accounts {
                account_ids
                    .push(upsert_provider_account_in_transaction(&mut transaction, account).await?);
            }
            append_admin_audit_event_in_transaction(&mut transaction, command.audit, revision)
                .await?;
            Ok(ProviderAccountAdminImport {
                config_revision: revision,
                account_ids,
            })
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin import").await
    }

    async fn rotate_provider_account(
        &self,
        expected_config_revision: Revision,
        command: RotateProviderAccount,
    ) -> StoreResult<ProviderAccountAdminRotation> {
        command.scope.validate()?;
        require_nonempty(ENTITY, "account_id", &command.profile.id)?;
        require_nonempty(ENTITY, "name", &command.profile.name)?;
        if command.profile.id != command.credential.account_id {
            return Err(invalid("rotated profile and credential account IDs differ"));
        }
        validate_credential_update(&command.credential)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin rotation"))?;
        let result = async {
            let config_revision =
                bump_config_revision_in_transaction(&mut transaction, expected_config_revision)
                    .await?;
            let credential_revision = rotate_provider_account_in_transaction(
                &mut transaction,
                &command.scope,
                &command.profile,
                &command.credential,
            )
            .await?;
            append_admin_audit_event_in_transaction(
                &mut transaction,
                command.audit,
                config_revision,
            )
            .await?;
            Ok(ProviderAccountAdminRotation {
                config_revision,
                credential_revision,
            })
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin rotation").await
    }

    async fn set_provider_account_enabled_admin(
        &self,
        expected_config_revision: Revision,
        command: SetProviderAccountEnabled,
    ) -> StoreResult<Revision> {
        command.scope.validate()?;
        require_nonempty(ENTITY, "account_id", &command.account_id)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin state change"))?;
        let result = async {
            let revision =
                bump_config_revision_in_transaction(&mut transaction, expected_config_revision)
                    .await?;
            set_provider_account_enabled_in_transaction(
                &mut transaction,
                &command.scope,
                &command.account_id,
                command.enabled,
            )
            .await?;
            append_admin_audit_event_in_transaction(&mut transaction, command.audit, revision)
                .await?;
            Ok(revision)
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin state change").await
    }

    async fn delete_provider_accounts_admin(
        &self,
        expected_config_revision: Revision,
        command: DeleteProviderAccounts,
    ) -> StoreResult<Revision> {
        command.scope.validate()?;
        validate_admin_account_ids(&command.account_ids)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| postgres_unavailable("begin provider account admin deletion"))?;
        let result = async {
            let revision =
                bump_config_revision_in_transaction(&mut transaction, expected_config_revision)
                    .await?;
            delete_provider_accounts_in_transaction(
                &mut transaction,
                &command.scope,
                &command.account_ids,
            )
            .await?;
            append_admin_audit_event_in_transaction(&mut transaction, command.audit, revision)
                .await?;
            Ok(revision)
        }
        .await;
        finish_admin_transaction(transaction, result, "provider account admin deletion").await
    }
}

/// Admin 账号用例所需的公共账号、留存观测与 revision 事务能力。
///
/// 三个 PostgreSQL adapter 都保持私有，调用方只能取得 [`AccountStore`] 暴露的领域能力。
#[derive(Clone)]
pub struct PgAdminAccountStore {
    accounts: PgProviderAccountRepository,
    observability: PgObservabilityRepository,
    control_plane: PgControlPlaneRepository,
}

impl PgAdminAccountStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            accounts: PgProviderAccountRepository::new(pool.clone()),
            observability: PgObservabilityRepository::new(pool.clone()),
            control_plane: PgControlPlaneRepository::new(pool),
        }
    }

    async fn usage_observations(
        &self,
        range: ObservabilityRange,
        account_ids: &[String],
    ) -> AdminStoreResult<Vec<ProviderAccountUsageObservation>> {
        if account_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut observations = Vec::with_capacity(account_ids.len());
        for account_ids in account_ids.chunks(ADMIN_USAGE_CHUNK_SIZE) {
            let query = ProviderAccountUsageQuery::for_accounts(range, account_ids.to_vec())
                .map_err(|error| admin_store_error(ENTITY, error))?;
            observations.extend(
                self.observability
                    .provider_account_usage(query)
                    .await
                    .map_err(|error| admin_store_error(ENTITY, error))?,
            );
        }
        Ok(observations)
    }

    async fn required_scope(
        &self,
        account_id: &str,
    ) -> AdminStoreResult<ProviderAccountAdminScope> {
        let record = self
            .accounts
            .load_provider_account(account_id)
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?
            .ok_or_else(|| {
                admin_store_error(
                    ENTITY,
                    StoreError::NotFound {
                        entity: ENTITY,
                        id: account_id.to_owned(),
                    },
                )
            })?;
        Ok(ProviderAccountAdminScope {
            provider_kind: record.summary.provider_kind,
        })
    }

    async fn commit_prepared_import(
        &self,
        expected_config_revision: AdminRevision,
        prepared: PreparedCredentialImport,
        context: &MutationContext,
        action: &str,
    ) -> AdminStoreResult<CredentialImportResult> {
        let provider_kind = prepared.provider_kind.as_str().to_owned();
        let accounts = prepared
            .credentials
            .into_iter()
            .map(prepared_account)
            .collect::<StoreResult<Vec<_>>>()
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let imported = self
            .accounts
            .import_provider_accounts(
                store_revision(expected_config_revision)?,
                ImportProviderAccounts {
                    scope: ProviderAccountAdminScope {
                        provider_kind: provider_kind.clone(),
                    },
                    accounts,
                    audit: mutation_audit(
                        context,
                        action,
                        "provider_account",
                        &provider_kind,
                        vec!["credentials".to_owned()],
                    ),
                },
            )
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(CredentialImportResult {
            config_revision: admin_revision(imported.config_revision)?,
            credential_ids: imported
                .account_ids
                .into_iter()
                .map(CoreProviderAccountId::new)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| {
                    AdminStoreError::new(
                        AdminStoreErrorKind::Unavailable,
                        ENTITY,
                        "provider account import returned an invalid account ID",
                    )
                })?,
        })
    }

    async fn commit_prepared_rotation(
        &self,
        expected_config_revision: AdminRevision,
        prepared: PreparedCredentialRotationFacts,
        context: &MutationContext,
        action: &str,
    ) -> AdminStoreResult<CredentialMutationResult> {
        let account_id = prepared.account_id.clone();
        let scope = ProviderAccountAdminScope {
            provider_kind: prepared.provider_kind.as_str().to_owned(),
        };
        let rotation = self
            .accounts
            .rotate_provider_account(
                store_revision(expected_config_revision)?,
                RotateProviderAccount {
                    scope,
                    profile: UpdateProviderAccount {
                        id: account_id.as_str().to_owned(),
                        name: prepared.name,
                        email: prepared.email,
                        plan_type: prepared.plan_type,
                    },
                    credential: ProviderCredentialUpdate {
                        account_id: account_id.as_str().to_owned(),
                        expected_revision: store_revision(prepared.expected_credential_revision)?,
                        provider_credentials_json: provider_document_json(
                            prepared.provider_material,
                        )
                        .map_err(|error| admin_store_error(ENTITY, error))?,
                        has_refresh_token: prepared.has_refresh_token,
                        access_token_expires_at: prepared.access_token_expires_at,
                        next_refresh_at: prepared.next_refresh_at,
                    },
                    audit: mutation_audit(
                        context,
                        action,
                        "provider_account",
                        account_id.as_str(),
                        vec!["credentials".to_owned()],
                    ),
                },
            )
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        Ok(CredentialMutationResult {
            config_revision: admin_revision(rotation.config_revision)?,
            account_id,
            credential_revision: Some(admin_revision(rotation.credential_revision)?),
        })
    }
}

#[async_trait]
impl AccountStore for PgAdminAccountStore {
    async fn list_accounts(&self, query: AdminAccountListQuery) -> AdminStoreResult<AccountPage> {
        if query.page == 0 {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "page number must be positive",
            ));
        }
        let (control_plane, accounts) = futures::try_join!(
            self.control_plane.load_control_plane(),
            self.accounts.list_provider_accounts(None, true),
        )
        .map_err(|error| admin_store_error(ENTITY, error))?;
        let now = Utc::now();
        let summary = admin_account_summary(&accounts, now);
        let mut items = accounts
            .into_iter()
            .filter_map(|account| {
                let status = admin_account_status(&account, now);
                account_matches_admin_query(&account, status, &query).then_some(
                    AdminAccountListItem {
                        account,
                        status,
                        usage: None,
                    },
                )
            })
            .collect::<Vec<_>>();

        if query.sort.is_some_and(|sort| {
            matches!(
                sort.field,
                AdminAccountSortField::Usage | AdminAccountSortField::LastUsedAt
            )
        }) {
            let range = retained_usage_range(control_plane.settings.usage_retention_days, now)?;
            let account_ids = items
                .iter()
                .map(|item| item.account.id.clone())
                .collect::<Vec<_>>();
            let mut usage_by_account = self
                .usage_observations(range, &account_ids)
                .await?
                .into_iter()
                .map(|usage| (usage.account_id.clone(), usage))
                .collect::<BTreeMap<_, _>>();
            for item in &mut items {
                item.usage = usage_by_account.remove(&item.account.id);
            }
        }
        sort_admin_account_items(&mut items, query.sort);

        let total = u64::try_from(items.len()).unwrap_or(u64::MAX);
        let page_size = usize::from(query.page_size.get());
        let offset = u64::from(query.page - 1).saturating_mul(u64::from(query.page_size.get()));
        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        let items = items
            .into_iter()
            .skip(offset)
            .take(page_size)
            .map(|item| admin_account_record(item.account))
            .collect::<AdminStoreResult<Vec<_>>>()?;
        Ok(AccountPage {
            config_revision: admin_revision(control_plane.settings.config_revision)?,
            items,
            total,
            summary,
        })
    }

    async fn load_account(&self, account_id: &str) -> AdminStoreResult<Option<AccountRecord>> {
        self.accounts
            .load_provider_account(account_id)
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?
            .map(|record| admin_account_record(record.summary))
            .transpose()
    }

    async fn load_account_usage(
        &self,
        range: TimeRange,
        account_ids: &[String],
    ) -> AdminStoreResult<Vec<AccountUsage>> {
        let range = ObservabilityRange::new(range.start, range.end)
            .map_err(|error| admin_store_error(ENTITY, error))?;
        self.usage_observations(range, account_ids)
            .await?
            .into_iter()
            .map(admin_account_usage)
            .collect()
    }

    async fn list_credentials(
        &self,
        provider_kind: &ProviderKind,
        query: CredentialListQuery,
    ) -> AdminStoreResult<CredentialPage> {
        let (control_plane, accounts) = futures::try_join!(
            self.control_plane.load_control_plane(),
            self.accounts
                .list_provider_accounts(Some(provider_kind.as_str()), true),
        )
        .map_err(|error| admin_store_error(ENTITY, error))?;
        let mut accounts = accounts
            .into_iter()
            .filter(|account| account.provider_kind == provider_kind.as_str())
            .filter(|account| {
                query.availability.as_ref().is_none_or(|expected| {
                    expected.matches(admin_account_availability(account.availability))
                })
            })
            .filter(|account| {
                query
                    .enabled
                    .is_none_or(|enabled| account.enabled == enabled)
            })
            .filter(|account| {
                let CredentialListWindow::Page {
                    cursor: Some(cursor),
                    ..
                } = &query.window
                else {
                    return true;
                };
                account.created_at > cursor.created_at
                    || (account.created_at == cursor.created_at
                        && account.id.as_str() > cursor.account_id.as_str())
            })
            .collect::<Vec<_>>();
        if matches!(&query.window, CredentialListWindow::Page { .. }) {
            accounts.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.id.cmp(&right.id))
            });
        }
        let next_cursor = match query.window {
            CredentialListWindow::All => None,
            CredentialListWindow::Page { page_size, .. } => {
                let page_size = usize::from(page_size.get());
                let has_more = accounts.len() > page_size;
                accounts.truncate(page_size);
                has_more
                    .then(|| accounts.last())
                    .flatten()
                    .map(|account| {
                        Ok(CredentialCursor {
                            created_at: account.created_at,
                            account_id: CoreProviderAccountId::new(account.id.clone()).map_err(
                                |_| {
                                    AdminStoreError::new(
                                        AdminStoreErrorKind::Invalid,
                                        ENTITY,
                                        "persisted Provider account ID is invalid",
                                    )
                                },
                            )?,
                        })
                    })
                    .transpose()?
            }
        };
        Ok(CredentialPage {
            config_revision: admin_revision(control_plane.settings.config_revision)?,
            items: accounts
                .into_iter()
                .map(admin_account_record)
                .collect::<AdminStoreResult<Vec<_>>>()?,
            next_cursor,
        })
    }

    async fn credential_details(
        &self,
        provider_kind: &ProviderKind,
        account_id: &CoreProviderAccountId,
    ) -> AdminStoreResult<Option<CredentialDetails>> {
        let (control_plane, account) = futures::try_join!(
            self.control_plane.load_control_plane(),
            self.accounts.load_provider_account(account_id.as_str()),
        )
        .map_err(|error| admin_store_error(ENTITY, error))?;
        account
            .filter(|record| record.summary.provider_kind == provider_kind.as_str())
            .map(|record| {
                Ok(CredentialDetails {
                    config_revision: admin_revision(control_plane.settings.config_revision)?,
                    credential: admin_account_record(record.summary)?,
                })
            })
            .transpose()
    }

    async fn load_credentials_for_export(
        &self,
        provider_kind: &ProviderKind,
        account_ids: &[CoreProviderAccountId],
    ) -> AdminStoreResult<Vec<ProviderExportCredentialInput>> {
        let ids = account_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect::<Vec<_>>();
        validate_admin_account_ids(&ids).map_err(|error| admin_store_error(ENTITY, error))?;
        let mut credentials = Vec::with_capacity(account_ids.len());
        for account_id in account_ids {
            let record = self
                .accounts
                .load_provider_account(account_id.as_str())
                .await
                .map_err(|error| admin_store_error(ENTITY, error))?
                .ok_or_else(|| {
                    AdminStoreError::new(
                        AdminStoreErrorKind::NotFound,
                        ENTITY,
                        "one or more exported credentials do not exist",
                    )
                })?;
            if record.summary.provider_kind != provider_kind.as_str() {
                return Err(AdminStoreError::new(
                    AdminStoreErrorKind::NotFound,
                    ENTITY,
                    "one or more exported credentials belong to another Provider",
                ));
            }
            credentials.push(ProviderExportCredentialInput {
                account: admin_account_record(record.summary)?,
                provider_material: ProviderDocument::new(OpaqueProviderData::new(
                    record.provider_credentials_json.fields().clone(),
                )),
            });
        }
        Ok(credentials)
    }

    async fn commit_credential_import(
        &self,
        command: CredentialImportCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialImportResult> {
        self.commit_prepared_import(
            command.expected_config_revision,
            command.prepared,
            context,
            "import_document",
        )
        .await
    }

    async fn commit_authorization(
        &self,
        command: AuthorizationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        let expected_config_revision = command.pending.expected_config_revision();
        match command.credential {
            AuthorizationCredentialCommit::Create(credential) => {
                let account_id = credential.account_id.clone();
                let result = self
                    .commit_prepared_import(
                        expected_config_revision,
                        PreparedCredentialImport {
                            provider_kind: credential.provider_kind.clone(),
                            credentials: vec![credential],
                        },
                        context,
                        "authorize",
                    )
                    .await?;
                let details = self
                    .accounts
                    .load_provider_account(account_id.as_str())
                    .await
                    .map_err(|error| admin_store_error(ENTITY, error))?
                    .ok_or_else(|| {
                        AdminStoreError::new(
                            AdminStoreErrorKind::Unavailable,
                            ENTITY,
                            "authorized credential was not visible after commit",
                        )
                    })?;
                Ok(CredentialMutationResult {
                    config_revision: result.config_revision,
                    account_id,
                    credential_revision: Some(admin_revision(details.summary.credential_revision)?),
                })
            }
            AuthorizationCredentialCommit::Reauthorize(prepared) => {
                self.commit_prepared_rotation(
                    expected_config_revision,
                    prepared,
                    context,
                    "reauthorize",
                )
                .await
            }
        }
    }

    async fn commit_credential_rotation(
        &self,
        command: CredentialRotationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        self.commit_prepared_rotation(
            command.expected_config_revision,
            command.prepared,
            context,
            "rotate_credential",
        )
        .await
    }

    async fn commit_credential_refresh(
        &self,
        command: CredentialRotationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult> {
        self.commit_prepared_rotation(
            command.expected_config_revision,
            command.prepared,
            context,
            "refresh_credential",
        )
        .await
    }

    async fn set_account_enabled(
        &self,
        command: SetAccountEnabled,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminRevision> {
        let scope = self.required_scope(&command.account_id).await?;
        let action = if command.enabled { "enable" } else { "disable" };
        self.accounts
            .set_provider_account_enabled_admin(
                store_revision(command.expected_config_revision)?,
                SetProviderAccountEnabled {
                    scope,
                    account_id: command.account_id.clone(),
                    enabled: command.enabled,
                    audit: mutation_audit(
                        context,
                        action,
                        "provider_account",
                        &command.account_id,
                        vec!["enabled".to_owned()],
                    ),
                },
            )
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
            .and_then(admin_revision)
    }

    async fn delete_accounts(
        &self,
        command: DeleteAccounts,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminRevision> {
        let first_account_id = command.account_ids.first().ok_or_else(|| {
            AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "account deletion requires at least one account ID",
            )
        })?;
        let scope = self.required_scope(first_account_id).await?;
        let audit_target = if command.account_ids.len() == 1 {
            first_account_id.clone()
        } else {
            "provider_accounts".to_owned()
        };
        self.accounts
            .delete_provider_accounts_admin(
                store_revision(command.expected_config_revision)?,
                DeleteProviderAccounts {
                    scope,
                    account_ids: command.account_ids,
                    audit: mutation_audit(
                        context,
                        "delete",
                        "provider_account",
                        &audit_target,
                        Vec::new(),
                    ),
                },
            )
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
            .and_then(admin_revision)
    }

    async fn record_credential_export(
        &self,
        account_ids: &[CoreProviderAccountId],
        context: &MutationContext,
    ) -> AdminStoreResult<()> {
        let ids = account_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect::<Vec<_>>();
        validate_admin_account_ids(&ids).map_err(|error| admin_store_error(ENTITY, error))?;
        for account_id in &ids {
            if self
                .accounts
                .load_provider_account(account_id)
                .await
                .map_err(|error| admin_store_error(ENTITY, error))?
                .is_none()
            {
                return Err(AdminStoreError::new(
                    AdminStoreErrorKind::NotFound,
                    ENTITY,
                    "one or more exported credentials do not exist",
                ));
            }
        }
        let control_plane = self
            .control_plane
            .load_control_plane()
            .await
            .map_err(|error| admin_store_error(ENTITY, error))?;
        let revision = control_plane.settings.config_revision;
        let mut transaction = self.accounts.pool.begin().await.map_err(|_| {
            admin_store_error(
                ENTITY,
                postgres_unavailable("begin credential export audit"),
            )
        })?;
        let result = async {
            for account_id in &ids {
                append_admin_audit_event_in_transaction(
                    &mut transaction,
                    mutation_audit(
                        context,
                        "export_credentials",
                        "provider_account",
                        account_id,
                        Vec::new(),
                    ),
                    revision,
                )
                .await?;
            }
            Ok(())
        }
        .await;
        finish_admin_transaction(transaction, result, "credential export audit")
            .await
            .map_err(|error| admin_store_error(ENTITY, error))
    }
}

struct AdminAccountListItem {
    account: ProviderAccountSummary,
    status: AdminAccountStatus,
    usage: Option<ProviderAccountUsageObservation>,
}

fn retained_usage_range(
    retention_days: u32,
    now: DateTime<Utc>,
) -> AdminStoreResult<ObservabilityRange> {
    let duration = TimeDelta::try_days(i64::from(retention_days)).ok_or_else(|| {
        AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            ENTITY,
            "usage retention is outside the supported range",
        )
    })?;
    let start = now.checked_sub_signed(duration).ok_or_else(|| {
        AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            ENTITY,
            "usage retention start is outside the supported range",
        )
    })?;
    ObservabilityRange::new(start, now).map_err(|error| admin_store_error(ENTITY, error))
}

fn account_matches_admin_query(
    account: &ProviderAccountSummary,
    status: AdminAccountStatus,
    query: &AdminAccountListQuery,
) -> bool {
    let provider_matches = query
        .provider_kind
        .as_ref()
        .is_none_or(|provider| account.provider_kind == provider.as_str());
    let search_matches = query.search.as_ref().is_none_or(|search| {
        let search = search.to_lowercase();
        [
            Some(account.id.as_str()),
            Some(account.name.as_str()),
            account.email.as_deref(),
            account.upstream_account_id.as_deref(),
            Some(account.upstream_user_id.as_str()),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.to_lowercase().contains(&search))
    });
    let status_matches = query.status.is_none_or(|expected| expected == status);
    provider_matches && search_matches && status_matches
}

fn admin_account_status(
    account: &ProviderAccountSummary,
    now: DateTime<Utc>,
) -> AdminAccountStatus {
    if !account.enabled {
        AdminAccountStatus::Disabled
    } else if account.availability == ProviderAccountAvailability::Banned {
        AdminAccountStatus::Banned
    } else if account.availability == ProviderAccountAvailability::QuotaExhausted {
        AdminAccountStatus::QuotaExhausted
    } else if account.availability == ProviderAccountAvailability::Expired
        || account.access_token_expires_at <= now
    {
        AdminAccountStatus::Expired
    } else if account.availability == ProviderAccountAvailability::Ready {
        AdminAccountStatus::Active
    } else {
        AdminAccountStatus::Attention
    }
}

fn admin_account_summary(
    accounts: &[ProviderAccountSummary],
    now: DateTime<Utc>,
) -> AccountSummary {
    let total = u64::try_from(accounts.len()).unwrap_or(u64::MAX);
    let active = u64::try_from(
        accounts
            .iter()
            .filter(|account| admin_account_status(account, now) == AdminAccountStatus::Active)
            .count(),
    )
    .unwrap_or(u64::MAX);
    let quota_exhausted = u64::try_from(
        accounts
            .iter()
            .filter(|account| {
                admin_account_status(account, now) == AdminAccountStatus::QuotaExhausted
            })
            .count(),
    )
    .unwrap_or(u64::MAX);
    AccountSummary {
        total,
        active,
        quota_exhausted,
        attention: total.saturating_sub(active),
    }
}

fn sort_admin_account_items(items: &mut [AdminAccountListItem], sort: Option<AdminAccountSort>) {
    let Some(sort) = sort else {
        items.sort_by(|left, right| left.account.id.cmp(&right.account.id));
        return;
    };
    items.sort_by(|left, right| {
        let ordering = match sort.field {
            AdminAccountSortField::Email => left.account.email.cmp(&right.account.email),
            AdminAccountSortField::Status => {
                admin_account_status_name(left.status).cmp(admin_account_status_name(right.status))
            }
            AdminAccountSortField::PlanType => left.account.plan_type.cmp(&right.account.plan_type),
            AdminAccountSortField::Usage => left
                .usage
                .as_ref()
                .and_then(|usage| usage.total_tokens)
                .cmp(&right.usage.as_ref().and_then(|usage| usage.total_tokens)),
            AdminAccountSortField::LastUsedAt => left
                .usage
                .as_ref()
                .and_then(|usage| usage.last_used_at.as_ref())
                .cmp(
                    &right
                        .usage
                        .as_ref()
                        .and_then(|usage| usage.last_used_at.as_ref()),
                ),
            AdminAccountSortField::ExpiresAt => left
                .account
                .access_token_expires_at
                .cmp(&right.account.access_token_expires_at),
        }
        .then_with(|| left.account.id.cmp(&right.account.id));
        match sort.direction {
            AdminSortDirection::Asc => ordering,
            AdminSortDirection::Desc => ordering.reverse(),
        }
    });
}

const fn admin_account_status_name(status: AdminAccountStatus) -> &'static str {
    match status {
        AdminAccountStatus::Active => "active",
        AdminAccountStatus::Expired => "expired",
        AdminAccountStatus::QuotaExhausted => "quota_exhausted",
        AdminAccountStatus::Disabled => "disabled",
        AdminAccountStatus::Banned => "banned",
        AdminAccountStatus::Attention => "attention",
    }
}

fn admin_account_record(summary: ProviderAccountSummary) -> AdminStoreResult<AccountRecord> {
    Ok(AccountRecord {
        id: summary.id,
        provider_kind: ProviderKind::new(summary.provider_kind).map_err(|_| {
            AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                ENTITY,
                "persisted Provider kind is invalid",
            )
        })?,
        name: summary.name,
        email: summary.email,
        upstream_user_id: summary.upstream_user_id,
        upstream_account_id: summary.upstream_account_id,
        plan_type: summary.plan_type,
        credential_revision: admin_revision(summary.credential_revision)?,
        has_refresh_token: summary.has_refresh_token,
        access_token_expires_at: summary.access_token_expires_at,
        next_refresh_at: summary.next_refresh_at,
        enabled: summary.enabled,
        availability: admin_account_availability(summary.availability),
        availability_reason: summary.availability_reason,
        cooldown_until: summary.cooldown_until,
        availability_observed_at: summary.availability_observed_at,
        quota_observed_at: summary.quota_observed_at,
        created_at: summary.created_at,
        updated_at: summary.updated_at,
    })
}

fn prepared_account(credential: PreparedCredentialCreate) -> StoreResult<NewProviderAccount> {
    Ok(NewProviderAccount {
        id: credential.account_id.as_str().to_owned(),
        provider_kind: credential.provider_kind.as_str().to_owned(),
        name: credential.name,
        email: credential.email,
        upstream_user_id: credential.upstream_user_id,
        upstream_account_id: credential.upstream_account_id,
        plan_type: credential.plan_type,
        provider_credentials_json: provider_document_json(credential.provider_material)?,
        has_refresh_token: credential.has_refresh_token,
        access_token_expires_at: credential.access_token_expires_at,
        next_refresh_at: credential.next_refresh_at,
        enabled: credential.enabled,
        availability: provider_account_availability(credential.availability),
        availability_reason: credential.availability_reason,
        cooldown_until: credential.cooldown_until,
        availability_observed_at: credential.availability_observed_at,
    })
}

fn provider_document_json(document: ProviderDocument) -> StoreResult<JsonObject> {
    JsonObject::try_from_value(
        ENTITY,
        serde_json::Value::Object(document.into_provider_data().into_inner()),
        CREDENTIALS_MAX_BYTES,
    )
}

const fn provider_account_availability(
    availability: AdminAccountAvailability,
) -> ProviderAccountAvailability {
    match availability {
        AdminAccountAvailability::Unknown => ProviderAccountAvailability::Unknown,
        AdminAccountAvailability::Ready => ProviderAccountAvailability::Ready,
        AdminAccountAvailability::Cooldown => ProviderAccountAvailability::Cooldown,
        AdminAccountAvailability::QuotaExhausted => ProviderAccountAvailability::QuotaExhausted,
        AdminAccountAvailability::Expired => ProviderAccountAvailability::Expired,
        AdminAccountAvailability::Banned => ProviderAccountAvailability::Banned,
        AdminAccountAvailability::Invalid => ProviderAccountAvailability::Invalid,
    }
}

const fn admin_account_availability(
    availability: ProviderAccountAvailability,
) -> AdminAccountAvailability {
    match availability {
        ProviderAccountAvailability::Unknown => AdminAccountAvailability::Unknown,
        ProviderAccountAvailability::Ready => AdminAccountAvailability::Ready,
        ProviderAccountAvailability::Cooldown => AdminAccountAvailability::Cooldown,
        ProviderAccountAvailability::QuotaExhausted => AdminAccountAvailability::QuotaExhausted,
        ProviderAccountAvailability::Expired => AdminAccountAvailability::Expired,
        ProviderAccountAvailability::Banned => AdminAccountAvailability::Banned,
        ProviderAccountAvailability::Invalid => AdminAccountAvailability::Invalid,
    }
}

fn admin_account_usage(usage: ProviderAccountUsageObservation) -> AdminStoreResult<AccountUsage> {
    Ok(AccountUsage {
        account_id: usage.account_id,
        request_count: usage.request_count,
        success_count: usage.success_count,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_tokens: usage.cached_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        image_input_tokens: usage.image_input_tokens,
        image_output_tokens: usage.image_output_tokens,
        image_request_count: usage.image_request_count,
        image_request_failed_count: usage.image_request_failed_count,
        total_tokens: usage.total_tokens,
        cost_coverage: admin_account_cost_coverage(usage.cost_coverage),
        costs: admin_account_costs(usage.costs)?,
        last_used_at: usage.last_used_at,
        models: usage
            .models
            .into_iter()
            .map(admin_account_model_usage)
            .collect::<AdminStoreResult<Vec<_>>>()?,
    })
}

fn admin_account_model_usage(
    usage: ProviderAccountModelUsageObservation,
) -> AdminStoreResult<AccountModelUsage> {
    Ok(AccountModelUsage {
        model: usage.model,
        request_count: usage.request_count,
        success_count: usage.success_count,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_tokens: usage.cached_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        image_input_tokens: usage.image_input_tokens,
        image_output_tokens: usage.image_output_tokens,
        image_request_count: usage.image_request_count,
        image_request_failed_count: usage.image_request_failed_count,
        total_tokens: usage.total_tokens,
        cost_coverage: admin_account_cost_coverage(usage.cost_coverage),
        costs: admin_account_costs(usage.costs)?,
        last_used_at: usage.last_used_at,
    })
}

const fn admin_account_cost_coverage(coverage: super::CostCoverage) -> AdminCostCoverage {
    AdminCostCoverage {
        provider_reported_count: coverage.provider_reported_count,
        calculated_count: coverage.calculated_count,
        partial_count: 0,
        unavailable_count: coverage.unavailable_count,
        not_billable_count: 0,
    }
}

fn admin_account_costs(costs: Vec<CurrencyCostTotal>) -> AdminStoreResult<Vec<AccountCost>> {
    costs
        .into_iter()
        .map(|cost| {
            let amount = AdminDecimalAmount::from_str(cost.amount.as_str()).map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Invalid,
                    ENTITY,
                    "persisted account cost is invalid",
                )
            })?;
            Ok(AccountCost {
                currency: cost.currency,
                amount,
            })
        })
        .collect()
}

async fn upsert_provider_account_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    account: &NewProviderAccount,
) -> StoreResult<String> {
    account.validate()?;
    let imported_id = sqlx::query_scalar::<_, String>(
        "insert into provider_accounts (
           id, provider_kind, name, email, upstream_user_id,
           upstream_account_id, plan_type, provider_credentials_json, credential_revision,
           has_refresh_token, access_token_expires_at, next_refresh_at, enabled,
           availability, availability_reason, cooldown_until, provider_quota_json,
           availability_observed_at, quota_observed_at, created_at, updated_at
         ) values (
           $1, $2, $3, $4, $5, $6, $7, $8, 1, $9, $10, $11, $12,
           $13, $14, $15, null, $16, null, now(), greatest(now(), $16)
         )
         on conflict (
           provider_kind,
           upstream_user_id,
           (coalesce(upstream_account_id, ''))
         ) do update set
           name = excluded.name,
           email = excluded.email,
           plan_type = excluded.plan_type,
           provider_credentials_json = excluded.provider_credentials_json,
           credential_revision = provider_accounts.credential_revision + 1,
           has_refresh_token = excluded.has_refresh_token,
           access_token_expires_at = excluded.access_token_expires_at,
           next_refresh_at = excluded.next_refresh_at,
           enabled = excluded.enabled,
           availability = excluded.availability,
           availability_reason = excluded.availability_reason,
           cooldown_until = excluded.cooldown_until,
           provider_quota_json = null,
           availability_observed_at = excluded.availability_observed_at,
           quota_observed_at = null,
           updated_at = greatest(now(), excluded.availability_observed_at)
         returning id",
    )
    .bind(&account.id)
    .bind(&account.provider_kind)
    .bind(&account.name)
    .bind(&account.email)
    .bind(&account.upstream_user_id)
    .bind(&account.upstream_account_id)
    .bind(&account.plan_type)
    .bind(account.provider_credentials_json.as_value())
    .bind(account.has_refresh_token)
    .bind(account.access_token_expires_at)
    .bind(account.next_refresh_at)
    .bind(account.enabled)
    .bind(account.availability.as_str())
    .bind(&account.availability_reason)
    .bind(account.cooldown_until)
    .bind(account.availability_observed_at)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
        {
            StoreError::Conflict {
                entity: ENTITY,
                id: account.id.clone(),
                kind: ConflictKind::InvalidTransition,
            }
        } else {
            postgres_unavailable("upsert provider account in admin transaction")
        }
    })?
    .ok_or_else(|| StoreError::Conflict {
        entity: ENTITY,
        id: account.id.clone(),
        kind: ConflictKind::InvalidTransition,
    })?;
    Ok(imported_id)
}

async fn rotate_provider_account_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &ProviderAccountAdminScope,
    profile: &UpdateProviderAccount,
    update: &ProviderCredentialUpdate,
) -> StoreResult<Revision> {
    let next = sqlx::query_scalar::<_, i64>(
        "update provider_accounts
         set name = $4,
             email = $5,
             plan_type = $6,
             provider_credentials_json = $7,
             credential_revision = credential_revision + 1,
             has_refresh_token = $8,
             access_token_expires_at = $9,
             next_refresh_at = $10,
             updated_at = now()
         where id = $1 and provider_kind = $2
           and credential_revision = $3
         returning credential_revision",
    )
    .bind(&update.account_id)
    .bind(&scope.provider_kind)
    .bind(to_i64(update.expected_revision.get())?)
    .bind(&profile.name)
    .bind(&profile.email)
    .bind(&profile.plan_type)
    .bind(update.provider_credentials_json.as_value())
    .bind(update.has_refresh_token)
    .bind(update.access_token_expires_at)
    .bind(update.next_refresh_at)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("rotate provider account in admin transaction"))?
    .ok_or_else(|| StoreError::Conflict {
        entity: ENTITY,
        id: update.account_id.clone(),
        kind: ConflictKind::StaleRevision,
    })?;
    Revision::new(to_u64(next)?)
}

async fn set_provider_account_enabled_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &ProviderAccountAdminScope,
    account_id: &str,
    enabled: bool,
) -> StoreResult<()> {
    let result = sqlx::query(
        "update provider_accounts set enabled = $3, updated_at = now()
         where id = $1 and provider_kind = $2",
    )
    .bind(account_id)
    .bind(&scope.provider_kind)
    .bind(enabled)
    .execute(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("set provider account state in admin transaction"))?;
    require_admin_account_changed(result.rows_affected(), account_id)
}

async fn delete_provider_accounts_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &ProviderAccountAdminScope,
    account_ids: &[String],
) -> StoreResult<()> {
    let deleted = sqlx::query_scalar::<_, String>(
        "delete from provider_accounts
         where id = any($1::text[]) and provider_kind = $2
         returning id",
    )
    .bind(account_ids)
    .bind(&scope.provider_kind)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| postgres_unavailable("delete provider account in admin transaction"))?;
    let deleted = deleted.into_iter().collect::<BTreeSet<_>>();
    let expected = account_ids.iter().cloned().collect::<BTreeSet<_>>();
    if deleted == expected {
        Ok(())
    } else {
        Err(invalid(
            "all deleted accounts must exist and match Provider scope",
        ))
    }
}

fn validate_credential_update(update: &ProviderCredentialUpdate) -> StoreResult<()> {
    require_nonempty(ENTITY, "account_id", &update.account_id)?;
    validate_object_size(
        "provider_credentials_json",
        &update.provider_credentials_json,
        CREDENTIALS_MAX_BYTES,
    )?;
    if !update.has_refresh_token && update.next_refresh_at.is_some() {
        return Err(invalid("next_refresh_at requires a refresh token"));
    }
    Ok(())
}

fn require_admin_account_changed(rows_affected: u64, account_id: &str) -> StoreResult<()> {
    if rows_affected == 1 {
        Ok(())
    } else {
        Err(StoreError::NotFound {
            entity: ENTITY,
            id: account_id.to_owned(),
        })
    }
}

fn validate_admin_account_ids(account_ids: &[String]) -> StoreResult<()> {
    if account_ids.is_empty() || account_ids.len() > MAX_ADMIN_IMPORT_BATCH {
        return Err(invalid(
            "admin account selection must contain between 1 and 200 IDs",
        ));
    }
    let mut unique = BTreeSet::new();
    for account_id in account_ids {
        require_nonempty(ENTITY, "account_id", account_id)?;
        if !unique.insert(account_id.as_str()) {
            return Err(invalid("admin account selection contains duplicate IDs"));
        }
    }
    Ok(())
}

async fn finish_admin_transaction<T>(
    transaction: Transaction<'_, Postgres>,
    result: StoreResult<T>,
    operation: &'static str,
) -> StoreResult<T> {
    match result {
        Ok(value) => {
            transaction
                .commit()
                .await
                .map_err(|_| postgres_unavailable(operation))?;
            Ok(value)
        }
        Err(error) => {
            transaction
                .rollback()
                .await
                .map_err(|_| postgres_unavailable(operation))?;
            Err(error)
        }
    }
}

#[async_trait]
impl ProviderAccountStore for PgProviderAccountRepository {
    async fn create_account(&self, account: CoreNewProviderAccount) -> Result<(), CoreStoreError> {
        if account.account.revision().get() != 1 {
            return Err(CoreStoreError::new(CoreStoreErrorKind::InvalidData));
        }
        let credential = JsonObject::try_from_value(
            "provider_credentials_json",
            serde_json::Value::Object(account.credential.into_inner()),
            CREDENTIALS_MAX_BYTES,
        )
        .map_err(core_store_error)?;
        self.insert_provider_account(NewProviderAccount {
            id: account.account.id().as_str().to_owned(),
            provider_kind: account.account.provider().as_str().to_owned(),
            name: account.account.name().to_owned(),
            email: account.account.email().map(str::to_owned),
            upstream_user_id: account.account.upstream_user_id().to_owned(),
            upstream_account_id: account.account.upstream_account_id().map(str::to_owned),
            plan_type: account.account.plan_type().map(str::to_owned),
            provider_credentials_json: credential,
            has_refresh_token: account.account.has_refresh_token(),
            access_token_expires_at: DateTime::<Utc>::from(
                account.account.access_token_expires_at(),
            ),
            next_refresh_at: account.account.next_refresh_at().map(DateTime::<Utc>::from),
            enabled: account.account.enabled(),
            availability: availability_from_core(account.account.availability()),
            availability_reason: None,
            cooldown_until: account.account.cooldown_until().map(DateTime::<Utc>::from),
            availability_observed_at: Utc::now(),
        })
        .await
        .map_err(core_store_error)
    }

    async fn get_account(
        &self,
        account: &CoreProviderAccountId,
    ) -> Result<Option<CoreProviderAccount>, CoreStoreError> {
        self.load_provider_account(account.as_str())
            .await
            .map_err(core_store_error)?
            .map(|record| core_account_from_summary(record.summary))
            .transpose()
    }

    async fn list_accounts(&self) -> Result<Vec<CoreProviderAccount>, CoreStoreError> {
        self.list_provider_accounts(None, true)
            .await
            .map_err(core_store_error)?
            .into_iter()
            .map(core_account_from_summary)
            .collect()
    }

    async fn list_for_provider(
        &self,
        provider: &ProviderKind,
    ) -> Result<Vec<CoreProviderAccount>, CoreStoreError> {
        self.list_provider_accounts(Some(provider.as_str()), false)
            .await
            .map_err(core_store_error)?
            .into_iter()
            .map(core_account_from_summary)
            .collect()
    }

    async fn load_credential(
        &self,
        account: &CoreProviderAccountId,
        expected_revision: CoreCredentialRevision,
    ) -> Result<LoadedCredential, CoreStoreError> {
        let record = self
            .load_provider_account(account.as_str())
            .await
            .map_err(core_store_error)?
            .ok_or_else(|| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
        if record.summary.credential_revision.get() != expected_revision.get() {
            return Err(CoreStoreError::new(CoreStoreErrorKind::Conflict));
        }
        Ok(LoadedCredential {
            account: core_account_from_summary(record.summary)?,
            credential: PlaintextCredential::new(record.provider_credentials_json.fields().clone()),
        })
    }

    async fn compare_and_swap_credential(
        &self,
        update: CredentialCasUpdate,
    ) -> Result<CredentialCasOutcome, CoreStoreError> {
        let (
            account_id,
            expected_revision,
            profile,
            credential,
            has_refresh_token,
            access_token_expires_at,
            next_refresh_at,
        ) = update.into_parts();
        if profile.account_id != account_id {
            return Err(CoreStoreError::new(CoreStoreErrorKind::InvalidData));
        }
        let account_id = account_id.as_str().to_owned();
        let credentials = JsonObject::try_from_value(
            "provider_credentials_json",
            serde_json::Value::Object(credential.into_inner()),
            CREDENTIALS_MAX_BYTES,
        )
        .map_err(core_store_error)?;
        let next = sqlx::query_scalar::<_, i64>(
            "update provider_accounts
             set name = $3, email = $4, plan_type = $5,
                 provider_credentials_json = $6,
                 credential_revision = credential_revision + 1,
                 has_refresh_token = $7, access_token_expires_at = $8,
                 next_refresh_at = $9, updated_at = now()
             where id = $1 and credential_revision = $2
             returning credential_revision",
        )
        .bind(&account_id)
        .bind(to_i64(expected_revision.get()).map_err(core_store_error)?)
        .bind(profile.name)
        .bind(profile.email)
        .bind(profile.plan_type)
        .bind(credentials.as_value())
        .bind(has_refresh_token)
        .bind(DateTime::<Utc>::from(access_token_expires_at))
        .bind(next_refresh_at.map(DateTime::<Utc>::from))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::Unavailable))?;
        match next {
            Some(next) => Ok(CredentialCasOutcome::Updated(
                CoreCredentialRevision::new(to_u64(next).map_err(core_store_error)?)
                    .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?,
            )),
            None => Ok(CredentialCasOutcome::Conflict),
        }
    }

    async fn get_quotas(
        &self,
        accounts: &[CoreProviderAccountId],
    ) -> Result<Vec<QuotaObservation>, CoreStoreError> {
        if accounts.is_empty() {
            return Ok(Vec::new());
        }
        let account_ids = accounts
            .iter()
            .map(|account| account.as_str().to_owned())
            .collect::<Vec<_>>();
        let rows = sqlx::query(
            "select id, credential_revision, provider_quota_json, quota_observed_at \
             from provider_accounts \
             where id = any($1) and provider_quota_json is not null",
        )
        .bind(account_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::Unavailable))?;
        rows.into_iter()
            .map(|row| {
                let account_id = row
                    .try_get::<String, _>("id")
                    .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
                let revision = row
                    .try_get::<i64, _>("credential_revision")
                    .ok()
                    .and_then(|value| u64::try_from(value).ok())
                    .and_then(|value| CoreCredentialRevision::new(value).ok())
                    .ok_or_else(|| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
                let quota = row
                    .try_get::<serde_json::Value, _>("provider_quota_json")
                    .ok()
                    .and_then(|value| value.as_object().cloned())
                    .map(OpaqueProviderData::new)
                    .ok_or_else(|| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
                let observed_at = row
                    .try_get::<DateTime<Utc>, _>("quota_observed_at")
                    .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
                Ok(QuotaObservation {
                    account_id: CoreProviderAccountId::new(account_id)
                        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?,
                    expected_revision: revision,
                    quota: Some(quota),
                    observed_at: Some(observed_at.into()),
                })
            })
            .collect()
    }

    async fn compare_and_swap_quota(
        &self,
        observation: QuotaObservation,
    ) -> Result<QuotaWriteOutcome, CoreStoreError> {
        let quota = observation
            .quota
            .map(|value| {
                JsonObject::try_from_value(
                    "provider_quota_json",
                    serde_json::Value::Object(value.into_inner()),
                    QUOTA_MAX_BYTES,
                )
            })
            .transpose()
            .map_err(core_store_error)?;
        let updated = self
            .compare_and_swap_provider_quota(
                observation.account_id.as_str(),
                Revision::new(observation.expected_revision.get()).map_err(core_store_error)?,
                quota,
                observation.observed_at.map(DateTime::<Utc>::from),
            )
            .await
            .map_err(core_store_error)?;
        Ok(if updated {
            QuotaWriteOutcome::Updated
        } else {
            QuotaWriteOutcome::Conflict
        })
    }

    async fn apply_state_change(&self, change: AccountStateChange) -> Result<(), CoreStoreError> {
        let updated = self
            .apply_provider_account_state(ProviderAccountStateUpdate {
                account_id: change.account_id.as_str().to_owned(),
                expected_revision: Revision::new(change.expected_revision.get())
                    .map_err(core_store_error)?,
                availability: availability_from_core(change.availability),
                availability_reason: change.reason,
                cooldown_until: change.cooldown_until.map(DateTime::<Utc>::from),
                availability_observed_at: DateTime::<Utc>::from(change.observed_at),
            })
            .await
            .map_err(core_store_error)?;
        if updated {
            Ok(())
        } else {
            Err(CoreStoreError::new(CoreStoreErrorKind::Conflict))
        }
    }

    async fn update_account(
        &self,
        update: CoreProviderAccountUpdate,
    ) -> Result<(), CoreStoreError> {
        let updated = self
            .update_provider_account(UpdateProviderAccount {
                id: update.account_id.as_str().to_owned(),
                name: update.name,
                email: update.email,
                plan_type: update.plan_type,
            })
            .await
            .map_err(core_store_error)?;
        require_core_update(updated)
    }

    async fn set_enabled(
        &self,
        account: &CoreProviderAccountId,
        enabled: bool,
    ) -> Result<(), CoreStoreError> {
        let updated = self
            .set_provider_account_enabled(account.as_str(), enabled)
            .await
            .map_err(core_store_error)?;
        require_core_update(updated)
    }

    async fn delete_account(&self, account: &CoreProviderAccountId) -> Result<(), CoreStoreError> {
        let deleted = self
            .delete_provider_account(account.as_str())
            .await
            .map_err(core_store_error)?;
        require_core_update(deleted)
    }
}

const ACCOUNT_SELECT: &str = "select id, provider_kind, name, email, upstream_user_id,
            upstream_account_id, plan_type, provider_credentials_json, credential_revision,
            has_refresh_token, access_token_expires_at, next_refresh_at, enabled, availability,
            availability_reason, cooldown_until, provider_quota_json,
            availability_observed_at, quota_observed_at, created_at, updated_at
     from provider_accounts where id = $1";

const ACCOUNT_SELECT_BY_IDS: &str = "select id, provider_kind, name, email, upstream_user_id,
            upstream_account_id, plan_type, provider_credentials_json, credential_revision,
            has_refresh_token, access_token_expires_at, next_refresh_at, enabled, availability,
            availability_reason, cooldown_until, provider_quota_json,
            availability_observed_at, quota_observed_at, created_at, updated_at
     from provider_accounts
     where id = any($1::text[]) and provider_kind = $2
     order by id";

fn account_record_from_row(row: sqlx::postgres::PgRow) -> StoreResult<ProviderAccountRecord> {
    let credentials = JsonObject::try_from_value(
        "provider_credentials_json",
        row.try_get("provider_credentials_json")
            .map_err(|_| invalid("invalid credentials JSON"))?,
        CREDENTIALS_MAX_BYTES,
    )?;
    let quota = row
        .try_get::<Option<serde_json::Value>, _>("provider_quota_json")
        .map_err(|_| invalid("invalid quota JSON"))?
        .map(|value| JsonObject::try_from_value("provider_quota_json", value, QUOTA_MAX_BYTES))
        .transpose()?;
    Ok(ProviderAccountRecord {
        summary: account_summary_from_row(row)?,
        provider_credentials_json: credentials,
        provider_quota_json: quota,
    })
}

fn core_account_from_summary(
    summary: ProviderAccountSummary,
) -> Result<CoreProviderAccount, CoreStoreError> {
    let id = CoreProviderAccountId::new(summary.id)
        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
    let provider = ProviderKind::new(summary.provider_kind)
        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
    let revision = CoreCredentialRevision::new(summary.credential_revision.get())
        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
    Ok(CoreProviderAccount::new(
        id,
        provider,
        summary.name,
        summary.upstream_user_id,
        revision,
        summary.access_token_expires_at.into(),
    )
    .with_profile(
        summary.email,
        summary.upstream_account_id,
        summary.plan_type,
    )
    .with_runtime_state(
        summary.enabled,
        availability_to_core(summary.availability),
        summary.cooldown_until.map(Into::into),
    )
    .with_refresh_schedule(
        summary.has_refresh_token,
        summary.next_refresh_at.map(Into::into),
    ))
}

const fn availability_to_core(value: ProviderAccountAvailability) -> CoreAccountAvailability {
    match value {
        ProviderAccountAvailability::Unknown => CoreAccountAvailability::Unknown,
        ProviderAccountAvailability::Ready => CoreAccountAvailability::Ready,
        ProviderAccountAvailability::Cooldown => CoreAccountAvailability::Cooldown,
        ProviderAccountAvailability::QuotaExhausted => CoreAccountAvailability::QuotaExhausted,
        ProviderAccountAvailability::Expired => CoreAccountAvailability::Expired,
        ProviderAccountAvailability::Banned => CoreAccountAvailability::Banned,
        ProviderAccountAvailability::Invalid => CoreAccountAvailability::Invalid,
    }
}

const fn availability_from_core(value: CoreAccountAvailability) -> ProviderAccountAvailability {
    match value {
        CoreAccountAvailability::Unknown => ProviderAccountAvailability::Unknown,
        CoreAccountAvailability::Ready => ProviderAccountAvailability::Ready,
        CoreAccountAvailability::Cooldown => ProviderAccountAvailability::Cooldown,
        CoreAccountAvailability::QuotaExhausted => ProviderAccountAvailability::QuotaExhausted,
        CoreAccountAvailability::Expired => ProviderAccountAvailability::Expired,
        CoreAccountAvailability::Banned => ProviderAccountAvailability::Banned,
        CoreAccountAvailability::Invalid => ProviderAccountAvailability::Invalid,
    }
}

fn core_store_error(error: StoreError) -> CoreStoreError {
    let kind = match error {
        StoreError::Unavailable { .. } => CoreStoreErrorKind::Unavailable,
        StoreError::Conflict { .. } => CoreStoreErrorKind::Conflict,
        StoreError::NotFound { .. } | StoreError::InvalidData { .. } => {
            CoreStoreErrorKind::InvalidData
        }
    };
    CoreStoreError::new(kind)
}

fn require_core_update(updated: bool) -> Result<(), CoreStoreError> {
    if updated {
        Ok(())
    } else {
        Err(CoreStoreError::new(CoreStoreErrorKind::InvalidState))
    }
}

fn account_summary_from_row(row: sqlx::postgres::PgRow) -> StoreResult<ProviderAccountSummary> {
    let revision = row
        .try_get::<i64, _>("credential_revision")
        .map_err(|_| invalid("invalid credential revision"))?;
    let availability = row
        .try_get::<String, _>("availability")
        .map_err(|_| invalid("invalid availability"))?;
    Ok(ProviderAccountSummary {
        id: get(&row, "id")?,
        provider_kind: get(&row, "provider_kind")?,
        name: get(&row, "name")?,
        email: get(&row, "email")?,
        upstream_user_id: get(&row, "upstream_user_id")?,
        upstream_account_id: get(&row, "upstream_account_id")?,
        plan_type: get(&row, "plan_type")?,
        credential_revision: Revision::new(to_u64(revision)?)?,
        has_refresh_token: get(&row, "has_refresh_token")?,
        access_token_expires_at: get(&row, "access_token_expires_at")?,
        next_refresh_at: get(&row, "next_refresh_at")?,
        enabled: get(&row, "enabled")?,
        availability: ProviderAccountAvailability::parse(&availability)?,
        availability_reason: get(&row, "availability_reason")?,
        cooldown_until: get(&row, "cooldown_until")?,
        availability_observed_at: get(&row, "availability_observed_at")?,
        quota_observed_at: get(&row, "quota_observed_at")?,
        created_at: get(&row, "created_at")?,
        updated_at: get(&row, "updated_at")?,
    })
}

fn get<'r, T>(row: &'r sqlx::postgres::PgRow, column: &'static str) -> StoreResult<T>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get(column).map_err(|_| invalid(column))
}

fn validate_object_size(field: &'static str, object: &JsonObject, max: usize) -> StoreResult<()> {
    let size = serde_json::to_vec(&object.as_value())
        .map_err(|error| StoreError::InvalidData {
            entity: ENTITY,
            message: error.to_string(),
        })?
        .len();
    if size > max {
        Err(invalid(field))
    } else {
        Ok(())
    }
}

fn to_i64(value: u64) -> StoreResult<i64> {
    i64::try_from(value).map_err(|_| invalid("revision is too large"))
}

fn to_u64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| invalid("revision must be positive"))
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: ENTITY,
        message: message.to_owned(),
    }
}
