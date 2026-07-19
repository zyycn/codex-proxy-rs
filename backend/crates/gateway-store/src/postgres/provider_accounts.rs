//! 明文 `provider_accounts` 与凭证 revision CAS 的唯一 PostgreSQL owner。

use std::{collections::BTreeSet, fmt};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
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
use gateway_core::routing::{ProviderInstanceId, ProviderKind};

use crate::{
    ConflictKind, JsonObject, Revision, StoreError, StoreResult, postgres_unavailable,
    require_nonempty,
};

use super::{
    AdminAuditEvent, append_admin_audit_event_in_transaction, bump_config_revision_in_transaction,
};

const ENTITY: &str = "provider account";
const CREDENTIALS_MAX_BYTES: usize = 256 * 1024;
const QUOTA_MAX_BYTES: usize = 128 * 1024;
const MAX_ADMIN_IMPORT_BATCH: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountAdminScope {
    pub provider_kind: String,
    pub provider_instance_id: String,
}

impl ProviderAccountAdminScope {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "provider_kind", &self.provider_kind)?;
        require_nonempty(ENTITY, "provider_instance_id", &self.provider_instance_id)
    }

    fn contains(&self, account: &NewProviderAccount) -> bool {
        account.provider_kind == self.provider_kind
            && account.provider_instance_id == self.provider_instance_id
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
    pub provider_instance_id: String,
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
    pub provider_instance_id: String,
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
            .field("provider_instance_id", &self.provider_instance_id)
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
        require_nonempty(ENTITY, "provider_instance_id", &self.provider_instance_id)?;
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
        provider_instance_id: Option<&str>,
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
    ) -> StoreResult<Revision>;

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
        provider_instance_id: Option<&str>,
        include_disabled: bool,
    ) -> StoreResult<Vec<ProviderAccountSummary>> {
        let rows = sqlx::query(
            "select id, provider_instance_id, provider_kind, name, email, upstream_user_id,
                    upstream_account_id, plan_type, credential_revision, has_refresh_token,
                    access_token_expires_at, next_refresh_at, enabled, availability,
                    availability_reason, cooldown_until, availability_observed_at,
                    quota_observed_at, created_at, updated_at
             from provider_accounts
             where ($1::text is null or provider_instance_id = $1) and ($2 or enabled)
             order by provider_kind, provider_instance_id, name, id",
        )
        .bind(provider_instance_id)
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
               id, provider_instance_id, provider_kind, name, email, upstream_user_id,
               upstream_account_id, plan_type, provider_credentials_json, credential_revision,
               has_refresh_token, access_token_expires_at, next_refresh_at, enabled,
               availability, availability_reason, cooldown_until, provider_quota_json,
               availability_observed_at, quota_observed_at, created_at, updated_at
             ) values (
               $1, $2, $3, $4, $5, $6, $7, $8, $9, 1, $10, $11, $12, $13,
               $14, null, $15, null, $16, null, now(), greatest(now(), $16)
             )",
        )
        .bind(account.id)
        .bind(account.provider_instance_id)
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
            .bind(&scope.provider_instance_id)
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
    ) -> StoreResult<Revision> {
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
            for account in &command.accounts {
                upsert_provider_account_in_transaction(&mut transaction, account).await?;
            }
            append_admin_audit_event_in_transaction(&mut transaction, command.audit, revision)
                .await?;
            Ok(revision)
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

async fn upsert_provider_account_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    account: &NewProviderAccount,
) -> StoreResult<()> {
    account.validate()?;
    let imported_id = sqlx::query_scalar::<_, String>(
        "insert into provider_accounts (
           id, provider_instance_id, provider_kind, name, email, upstream_user_id,
           upstream_account_id, plan_type, provider_credentials_json, credential_revision,
           has_refresh_token, access_token_expires_at, next_refresh_at, enabled,
           availability, availability_reason, cooldown_until, provider_quota_json,
           availability_observed_at, quota_observed_at, created_at, updated_at
         ) values (
           $1, $2, $3, $4, $5, $6, $7, $8, $9, 1, $10, $11, $12, $13,
           $14, null, $15, null, $16, null, now(), greatest(now(), $16)
         )
         on conflict (id) do update set
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
           availability_reason = null,
           cooldown_until = excluded.cooldown_until,
           provider_quota_json = null,
           availability_observed_at = excluded.availability_observed_at,
           quota_observed_at = null,
           updated_at = greatest(now(), excluded.availability_observed_at)
         where provider_accounts.provider_instance_id = excluded.provider_instance_id
           and provider_accounts.provider_kind = excluded.provider_kind
           and provider_accounts.upstream_user_id = excluded.upstream_user_id
           and provider_accounts.upstream_account_id is not distinct from excluded.upstream_account_id
         returning id",
    )
    .bind(&account.id)
    .bind(&account.provider_instance_id)
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
    debug_assert_eq!(imported_id, account.id);
    Ok(())
}

async fn rotate_provider_account_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    scope: &ProviderAccountAdminScope,
    profile: &UpdateProviderAccount,
    update: &ProviderCredentialUpdate,
) -> StoreResult<Revision> {
    let next = sqlx::query_scalar::<_, i64>(
        "update provider_accounts
         set name = $5,
             email = $6,
             plan_type = $7,
             provider_credentials_json = $8,
             credential_revision = credential_revision + 1,
             has_refresh_token = $9,
             access_token_expires_at = $10,
             next_refresh_at = $11,
             updated_at = now()
         where id = $1 and provider_instance_id = $2 and provider_kind = $3
           and credential_revision = $4
         returning credential_revision",
    )
    .bind(&update.account_id)
    .bind(&scope.provider_instance_id)
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
        "update provider_accounts set enabled = $4, updated_at = now()
         where id = $1 and provider_instance_id = $2 and provider_kind = $3",
    )
    .bind(account_id)
    .bind(&scope.provider_instance_id)
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
         where id = any($1::text[]) and provider_instance_id = $2
           and provider_kind = $3 and not enabled
         returning id",
    )
    .bind(account_ids)
    .bind(&scope.provider_instance_id)
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
            "all deleted accounts must exist, match Provider scope, and be disabled",
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
            provider_instance_id: account.account.instance().as_str().to_owned(),
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

    async fn list_for_instance(
        &self,
        instance: &ProviderInstanceId,
    ) -> Result<Vec<CoreProviderAccount>, CoreStoreError> {
        self.list_provider_accounts(Some(instance.as_str()), false)
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

    async fn get_quota(
        &self,
        account: &CoreProviderAccountId,
    ) -> Result<Option<QuotaObservation>, CoreStoreError> {
        let Some(record) = self
            .load_provider_account(account.as_str())
            .await
            .map_err(core_store_error)?
        else {
            return Ok(None);
        };
        if record.provider_quota_json.is_none() {
            return Ok(None);
        }
        Ok(Some(QuotaObservation {
            account_id: account.clone(),
            expected_revision: CoreCredentialRevision::new(
                record.summary.credential_revision.get(),
            )
            .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?,
            quota: record
                .provider_quota_json
                .map(|value| OpaqueProviderData::new(value.fields().clone())),
            observed_at: record.summary.quota_observed_at.map(Into::into),
        }))
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

const ACCOUNT_SELECT: &str =
    "select id, provider_instance_id, provider_kind, name, email, upstream_user_id,
            upstream_account_id, plan_type, provider_credentials_json, credential_revision,
            has_refresh_token, access_token_expires_at, next_refresh_at, enabled, availability,
            availability_reason, cooldown_until, provider_quota_json,
            availability_observed_at, quota_observed_at, created_at, updated_at
     from provider_accounts where id = $1";

const ACCOUNT_SELECT_BY_IDS: &str =
    "select id, provider_instance_id, provider_kind, name, email, upstream_user_id,
            upstream_account_id, plan_type, provider_credentials_json, credential_revision,
            has_refresh_token, access_token_expires_at, next_refresh_at, enabled, availability,
            availability_reason, cooldown_until, provider_quota_json,
            availability_observed_at, quota_observed_at, created_at, updated_at
     from provider_accounts
     where id = any($1::text[]) and provider_instance_id = $2 and provider_kind = $3
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
    let instance = ProviderInstanceId::new(summary.provider_instance_id)
        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
    let provider = ProviderKind::new(summary.provider_kind)
        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
    let revision = CoreCredentialRevision::new(summary.credential_revision.get())
        .map_err(|_| CoreStoreError::new(CoreStoreErrorKind::InvalidData))?;
    Ok(CoreProviderAccount::new(
        id,
        instance,
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
        provider_instance_id: get(&row, "provider_instance_id")?,
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
