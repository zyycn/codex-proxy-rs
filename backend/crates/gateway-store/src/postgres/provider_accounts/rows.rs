//! `provider_accounts` row 模型、解码与校验。

use super::*;
use gateway_core::routing::AccountGroupId;

pub(crate) const ENTITY: &str = "provider account";
pub(crate) const CREDENTIALS_MAX_BYTES: usize = 256 * 1024;
pub(crate) const QUOTA_MAX_BYTES: usize = 128 * 1024;
pub(crate) const MAX_ADMIN_IMPORT_BATCH: usize = 200;
pub(crate) const ADMIN_USAGE_CHUNK_SIZE: usize = 200;

pub(crate) const ACCOUNT_USAGE_BY_WINDOWS_SQL: &str = "with requested_windows as (
         select *
         from unnest($1::text[], $2::text[], $3::timestamptz[], $4::timestamptz[])
           as requested(account_id, window_key, window_start, window_end)
     )
     select requested.account_id,
            requested.window_key,
            count(mr.id)::bigint as request_count,
            count(mr.id) filter (where mr.outcome = 'succeeded')::bigint as success_count,
            sum(mr.input_tokens)::bigint as input_tokens,
            sum(mr.output_tokens)::bigint as output_tokens,
            sum(mr.cached_tokens)::bigint as cached_tokens,
            sum(mr.cache_write_tokens)::bigint as cache_write_tokens,
            sum(mr.reasoning_tokens)::bigint as reasoning_tokens,
            sum(mr.image_input_tokens)::bigint as image_input_tokens,
            sum(mr.image_output_tokens)::bigint as image_output_tokens,
            count(mr.id) filter (where mr.image_generation_succeeded is true)::bigint
              as image_request_count,
            count(mr.id) filter (where mr.image_generation_succeeded is false)::bigint
              as image_request_failed_count,
            coalesce(sum(coalesce(
              mr.total_tokens,
              coalesce(mr.input_tokens, 0) + coalesce(mr.output_tokens, 0)
            )), 0)::bigint as total_tokens,
            count(mr.id) filter (where mr.cost_source = 'provider_reported')::bigint
              as provider_reported_count,
            count(mr.id) filter (where mr.cost_source = 'calculated')::bigint
              as calculated_count,
            count(mr.id) filter (where mr.cost_source = 'unavailable')::bigint
              as unavailable_count,
            max(mr.started_at) as last_used_at
       from requested_windows requested
       left join model_requests mr
         on mr.provider_account_ref = requested.account_id
        and mr.started_at >= requested.window_start
        and mr.started_at < requested.window_end
        and mr.outcome = 'succeeded'
        and mr.downstream_committed_at is not null
        and mr.client_status_code between 200 and 399
      group by requested.account_id, requested.window_key
      order by requested.account_id, requested.window_key";

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

pub(crate) fn parse_credential_state(value: &str) -> StoreResult<CredentialState> {
    CredentialState::parse(value).ok_or_else(|| invalid("unknown credential_state value"))
}

pub(crate) fn parse_quota_access_state(value: &str) -> StoreResult<QuotaAccessState> {
    QuotaAccessState::parse(value).ok_or_else(|| invalid("unknown quota_access_state value"))
}

pub(crate) fn parse_quota_evidence(value: Option<String>) -> StoreResult<Option<QuotaEvidence>> {
    value
        .map(|value| {
            QuotaEvidence::parse(&value).ok_or_else(|| invalid("unknown quota_evidence value"))
        })
        .transpose()
}

pub(crate) fn parse_error_reason(value: Option<String>) -> StoreResult<Option<AccountErrorReason>> {
    value
        .map(|value| {
            AccountErrorReason::parse(&value)
                .ok_or_else(|| invalid("unknown last_error_reason value"))
        })
        .transpose()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountSummary {
    pub id: String,
    pub provider_kind: String,
    pub name: String,
    pub email: Option<String>,
    pub upstream_user_id: Option<String>,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    pub authentication_kind: String,
    pub credential_revision: Revision,
    pub has_refresh_token: bool,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub next_refresh_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub concurrency_limit: Option<AccountConcurrencyLimit>,
    pub weight: AccountWeight,
    pub credential_state: CredentialState,
    pub credential_observed_at: DateTime<Utc>,
    pub quota: QuotaState,
    pub last_error_reason: Option<AccountErrorReason>,
    pub last_error_message: Option<String>,
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
    pub upstream_user_id: Option<String>,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    pub authentication_kind: String,
    pub provider_credentials_json: JsonObject,
    pub has_refresh_token: bool,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub next_refresh_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub concurrency_limit: Option<AccountConcurrencyLimit>,
    pub weight: AccountWeight,
    pub credential_state: CredentialState,
    pub credential_observed_at: DateTime<Utc>,
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
            .field("credential_state", &self.credential_state)
            .field("provider_credentials_json", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl NewProviderAccount {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "id", &self.id)?;
        require_nonempty(ENTITY, "provider_kind", &self.provider_kind)?;
        require_nonempty(ENTITY, "name", &self.name)?;
        if let Some(upstream_user_id) = &self.upstream_user_id {
            require_nonempty(ENTITY, "upstream_user_id", upstream_user_id)?;
        }
        require_nonempty(ENTITY, "authentication_kind", &self.authentication_kind)?;
        if !self.has_refresh_token && self.next_refresh_at.is_some() {
            return Err(invalid("next_refresh_at requires a refresh token"));
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
    pub replacement_identity: Option<ProviderAccountIdentity>,
    pub credential: ProviderCredentialUpdate,
    pub audit: AdminAuditEvent,
}

impl fmt::Debug for RotateProviderAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RotateProviderAccount")
            .field("scope", &self.scope)
            .field("profile", &self.profile)
            .field("replacement_identity", &self.replacement_identity)
            .field("credential", &self.credential)
            .field("audit", &self.audit)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct BatchUpdateProviderAccountsAdmin {
    pub account_ids: Vec<String>,
    pub enabled: bool,
    pub concurrency_limit: Option<AccountConcurrencyLimit>,
    pub weight: AccountWeight,
    pub group_ids: Vec<AccountGroupId>,
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
    pub access_token_expires_at: Option<DateTime<Utc>>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountStateUpdate {
    pub account_id: String,
    pub expected_revision: Revision,
    pub credential_state: CredentialState,
    pub credential_observed_at: DateTime<Utc>,
    pub error_reason: Option<AccountErrorReason>,
    pub message: Option<String>,
}

impl ProviderAccountStateUpdate {
    pub fn validate(&self) -> StoreResult<()> {
        require_nonempty(ENTITY, "account_id", &self.account_id)?;
        Ok(())
    }
}

pub(crate) const ACCOUNT_SELECT: &str = "select id, provider_kind, name, email, upstream_user_id,
            upstream_account_id, plan_type, authentication_kind, provider_credentials_json, credential_revision,
            has_refresh_token, access_token_expires_at, next_refresh_at, enabled, concurrency_limit, weight, credential_state,
            provider_quota_json, quota_access_state, quota_evidence, quota_access_observed_at, quota_reset_at,
            last_error_reason, last_error_message,
            credential_observed_at, quota_observed_at, created_at, updated_at
     from provider_accounts where id = $1";

pub(crate) const ACCOUNT_SELECT_BY_IDS: &str = "select id, provider_kind, name, email, upstream_user_id,
            upstream_account_id, plan_type, authentication_kind, provider_credentials_json, credential_revision,
            has_refresh_token, access_token_expires_at, next_refresh_at, enabled, concurrency_limit, weight, credential_state,
            provider_quota_json, quota_access_state, quota_evidence, quota_access_observed_at, quota_reset_at,
            last_error_reason, last_error_message,
            credential_observed_at, quota_observed_at, created_at, updated_at
     from provider_accounts
     where id = any($1::text[]) and provider_kind = $2
     order by id";

pub(crate) fn account_record_from_row(
    row: sqlx::postgres::PgRow,
) -> StoreResult<ProviderAccountRecord> {
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

pub(crate) fn core_account_from_summary(
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
        summary.authentication_kind,
        revision,
        summary.access_token_expires_at.map(Into::into),
    )
    .with_profile(
        summary.email,
        summary.upstream_account_id,
        summary.plan_type,
    )
    .with_account_facts(
        summary.enabled,
        summary.credential_state,
        summary.quota,
        summary.last_error_reason,
        summary.last_error_message,
    )
    .with_scheduling(summary.concurrency_limit, summary.weight)
    .with_refresh_schedule(
        summary.has_refresh_token,
        summary.next_refresh_at.map(Into::into),
    ))
}

pub(crate) fn core_store_error(error: StoreError) -> CoreStoreError {
    let kind = match error {
        StoreError::Unavailable { .. } => CoreStoreErrorKind::Unavailable,
        StoreError::Conflict { .. } => CoreStoreErrorKind::Conflict,
        StoreError::NotFound { .. } | StoreError::InvalidData { .. } => {
            CoreStoreErrorKind::InvalidData
        }
    };
    CoreStoreError::new(kind)
}

pub(crate) fn require_core_update(updated: bool) -> Result<(), CoreStoreError> {
    if updated {
        Ok(())
    } else {
        Err(CoreStoreError::new(CoreStoreErrorKind::InvalidState))
    }
}

pub(crate) fn account_summary_from_row(
    row: sqlx::postgres::PgRow,
) -> StoreResult<ProviderAccountSummary> {
    let revision = row
        .try_get::<i64, _>("credential_revision")
        .map_err(|_| invalid("invalid credential revision"))?;
    let credential_state = row
        .try_get::<String, _>("credential_state")
        .map_err(|_| invalid("invalid credential_state"))?;
    let quota_access_state = parse_quota_access_state(&get::<String>(&row, "quota_access_state")?)?;
    let quota_evidence = parse_quota_evidence(get(&row, "quota_evidence")?)?;
    let quota_access_observed_at = get::<Option<DateTime<Utc>>>(&row, "quota_access_observed_at")?;
    let quota_reset_at = get::<Option<DateTime<Utc>>>(&row, "quota_reset_at")?;
    let quota = QuotaState::from_persisted(
        quota_access_state,
        quota_evidence,
        quota_access_observed_at.map(Into::into),
        quota_reset_at.map(Into::into),
    )
    .ok_or_else(|| invalid("invalid persisted quota fact"))?;
    let concurrency_limit = get::<Option<i64>>(&row, "concurrency_limit")?
        .map(|value| {
            u32::try_from(value)
                .ok()
                .and_then(AccountConcurrencyLimit::new)
                .ok_or_else(|| invalid("invalid concurrency_limit"))
        })
        .transpose()?;
    let weight = u16::try_from(get::<i16>(&row, "weight")?)
        .ok()
        .and_then(AccountWeight::new)
        .ok_or_else(|| invalid("invalid weight"))?;
    Ok(ProviderAccountSummary {
        id: get(&row, "id")?,
        provider_kind: get(&row, "provider_kind")?,
        name: get(&row, "name")?,
        email: get(&row, "email")?,
        upstream_user_id: get(&row, "upstream_user_id")?,
        upstream_account_id: get(&row, "upstream_account_id")?,
        plan_type: get(&row, "plan_type")?,
        authentication_kind: get(&row, "authentication_kind")?,
        credential_revision: Revision::new(to_u64(revision)?)?,
        has_refresh_token: get(&row, "has_refresh_token")?,
        access_token_expires_at: get(&row, "access_token_expires_at")?,
        next_refresh_at: get(&row, "next_refresh_at")?,
        enabled: get(&row, "enabled")?,
        concurrency_limit,
        weight,
        credential_state: parse_credential_state(&credential_state)?,
        credential_observed_at: get(&row, "credential_observed_at")?,
        quota,
        last_error_reason: parse_error_reason(get(&row, "last_error_reason")?)?,
        last_error_message: get(&row, "last_error_message")?,
        created_at: get(&row, "created_at")?,
        updated_at: get(&row, "updated_at")?,
    })
}

pub(crate) fn get<'r, T>(row: &'r sqlx::postgres::PgRow, column: &'static str) -> StoreResult<T>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get(column).map_err(|_| invalid(column))
}

pub(crate) fn validate_object_size(
    field: &'static str,
    object: &JsonObject,
    max: usize,
) -> StoreResult<()> {
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

pub(crate) fn to_i64(value: u64) -> StoreResult<i64> {
    i64::try_from(value).map_err(|_| invalid("revision is too large"))
}

pub(crate) fn to_u64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| invalid("revision must be positive"))
}

pub(crate) fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: ENTITY,
        message: message.to_owned(),
    }
}
