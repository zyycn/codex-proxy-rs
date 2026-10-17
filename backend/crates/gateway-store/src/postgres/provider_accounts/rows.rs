//! `provider_accounts` row 模型、解码与校验

use super::*;
use gateway_core::routing::AccountGroupId;

pub(crate) const ENTITY: &str = "provider account";
pub(crate) const CREDENTIALS_MAX_BYTES: usize = 256 * 1024;
pub(crate) const QUOTA_MAX_BYTES: usize = 128 * 1024;
pub(crate) const MAX_ADMIN_IMPORT_BATCH: usize = 200;
pub(crate) const ADMIN_USAGE_CHUNK_SIZE: usize = 200;

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
    pub outbound_proxy_enabled: bool,
    pub request_location: Option<gateway_core::account::RequestLocation>,
    pub outbound_proxy: Option<gateway_core::account::OutboundProxy>,
    pub id: String,
    pub provider_kind: String,
    pub name: String,
    pub notes: Option<String>,
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
    pub model_access: gateway_core::account::AccountModelAccess,
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
    pub outbound_proxy: Option<gateway_core::account::OutboundProxy>,
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
    pub model_access: Option<gateway_core::account::AccountModelAccess>,
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
    pub settings: Option<AccountImportSettings>,
    pub outbound_proxy: Option<gateway_admin::model::proxies::ImportProxyBinding>,
    pub scope: ProviderAccountAdminScope,
    pub accounts: Vec<NewProviderAccount>,
    pub audit: AdminAuditEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountAdminImport {
    pub config_revision: Revision,
    pub account_ids: Vec<String>,
    /// 与导入同一事务返回的最终凭据版本，避免提交后再查到另一轮写入
    pub credential_revisions: std::collections::BTreeMap<String, Revision>,
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
        if let Some(settings) = &self.settings {
            repository::validate_batch_update_group_ids(&settings.group_ids)?;
        }
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
    pub settings: Option<gateway_admin::model::accounts::UpdateAccount>,
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
            .field("settings", &self.settings)
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
    pub outbound_proxy: Option<gateway_admin::model::proxies::AccountProxySelection>,
    pub account_ids: Vec<String>,
    pub notes: Option<String>,
    pub enabled: Option<bool>,
    pub concurrency_limit: Option<Option<AccountConcurrencyLimit>>,
    pub weight: Option<AccountWeight>,
    pub model_access: Option<gateway_core::account::AccountModelAccess>,
    pub group_ids: Option<Vec<AccountGroupId>>,
    pub audit: AdminAuditEvent,
}

#[derive(Debug, Clone)]
pub struct RecoverProviderAccount {
    pub account_id: String,
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
    pub preserve_profile: bool,
    pub preserve_credential_state: bool,
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

pub(crate) const ACCOUNT_SELECT: &str = "select coalesce(proxy_enabled, true) as outbound_proxy_enabled, coalesce(max_websocket_connections, 0) as outbound_proxy_max_websocket_connections, auto_location, detected_location_json, location_country, location_region, location_city, location_timezone, outbound_proxy_url, id, provider_kind, name, notes, email, upstream_user_id,
            upstream_account_id, plan_type, authentication_kind, provider_credentials_json, credential_revision,
            has_refresh_token, access_token_expires_at, next_refresh_at, enabled, concurrency_limit, weight, model_access_json, credential_state,
            provider_quota_json, quota_access_state, quota_evidence, quota_access_observed_at, quota_reset_at,
            last_error_reason, last_error_message,
            credential_observed_at, quota_observed_at, created_at, updated_at
     from provider_accounts
     left join (select id as location_proxy_id, enabled as proxy_enabled, max_websocket_connections, auto_location, detected_location_json, location_country, location_region, location_city, location_timezone from outbound_proxies) proxy_location
       on outbound_proxy_id = location_proxy_id
     where id = $1";

pub(crate) const ACCOUNT_SELECT_BY_IDS: &str = "select coalesce(proxy_enabled, true) as outbound_proxy_enabled, coalesce(max_websocket_connections, 0) as outbound_proxy_max_websocket_connections, auto_location, detected_location_json, location_country, location_region, location_city, location_timezone, outbound_proxy_url, id, provider_kind, name, notes, email, upstream_user_id,
            upstream_account_id, plan_type, authentication_kind, provider_credentials_json, credential_revision,
            has_refresh_token, access_token_expires_at, next_refresh_at, enabled, concurrency_limit, weight, model_access_json, credential_state,
            provider_quota_json, quota_access_state, quota_evidence, quota_access_observed_at, quota_reset_at,
            last_error_reason, last_error_message,
            credential_observed_at, quota_observed_at, created_at, updated_at
     from provider_accounts
     left join (select id as location_proxy_id, enabled as proxy_enabled, max_websocket_connections, auto_location, detected_location_json, location_country, location_region, location_city, location_timezone from outbound_proxies) proxy_location
       on outbound_proxy_id = location_proxy_id
     where id = any($1::text[]) and provider_kind = $2
     order by id";

pub(crate) const REFRESH_CANDIDATES_SELECT: &str = "select coalesce(proxy_enabled, true) as outbound_proxy_enabled, coalesce(max_websocket_connections, 0) as outbound_proxy_max_websocket_connections, auto_location, detected_location_json, location_country, location_region, location_city, location_timezone, outbound_proxy_url, id, provider_kind, name, notes, email, upstream_user_id,
            upstream_account_id, plan_type, authentication_kind, provider_credentials_json, credential_revision,
            has_refresh_token, access_token_expires_at, next_refresh_at, enabled, concurrency_limit, weight, model_access_json, credential_state,
            provider_quota_json, quota_access_state, quota_evidence, quota_access_observed_at, quota_reset_at,
            last_error_reason, last_error_message,
            credential_observed_at, quota_observed_at, created_at, updated_at
     from provider_accounts
     left join (select id as location_proxy_id, enabled as proxy_enabled, max_websocket_connections, auto_location, detected_location_json, location_country, location_region, location_city, location_timezone from outbound_proxies) proxy_location
       on outbound_proxy_id = location_proxy_id
     where provider_kind = $1
       and has_refresh_token
       and credential_state in ('unknown', 'ready')
       and access_token_expires_at is not null
       and (
         access_token_expires_at <= $3
         or (
           access_token_expires_at <= $2
           and (next_refresh_at is null or next_refresh_at <= $4)
         )
       )
       and not (id = any($5::text[]))
     order by access_token_expires_at, id
     limit $6";

pub(crate) fn account_record_from_row(
    row: sqlx::postgres::PgRow,
) -> StoreResult<ProviderAccountRecord> {
    let credentials = JsonObject::try_from_value(
        "provider_credentials_json",
        row.try_get("provider_credentials_json")
            .map_err(|source| invalid("invalid credentials JSON").with_source(source))?,
        CREDENTIALS_MAX_BYTES,
    )?;
    let quota = row
        .try_get::<Option<serde_json::Value>, _>("provider_quota_json")
        .map_err(|source| invalid("invalid quota JSON").with_source(source))?
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
        .map_err(|source| CoreStoreError::caused_by(CoreStoreErrorKind::InvalidData, source))?;
    let provider = ProviderKind::new(summary.provider_kind)
        .map_err(|source| CoreStoreError::caused_by(CoreStoreErrorKind::InvalidData, source))?;
    let revision = CoreCredentialRevision::new(summary.credential_revision.get())
        .map_err(|source| CoreStoreError::caused_by(CoreStoreErrorKind::InvalidData, source))?;
    Ok(CoreProviderAccount::new(
        id,
        provider,
        summary.name,
        summary.upstream_user_id,
        summary.authentication_kind,
        revision,
        summary.access_token_expires_at.map(Into::into),
    )
    .with_notes(summary.notes)
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
    .with_model_access(summary.model_access)
    .with_outbound_proxy(
        summary
            .outbound_proxy
            .filter(|_| summary.outbound_proxy_enabled),
    )
    .with_request_location(summary.request_location)
    .with_refresh_schedule(
        summary.has_refresh_token,
        summary.next_refresh_at.map(Into::into),
    ))
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
        .map_err(|source| invalid("invalid credential revision").with_source(source))?;
    let status = account_status_facts_from_row(&row)?;
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
        outbound_proxy_enabled: get(&row, "outbound_proxy_enabled")?,
        request_location: super::super::proxies::effective_location_from_row(&row)?,
        outbound_proxy: get::<Option<String>>(&row, "outbound_proxy_url")?
            .map(|url| {
                let limit = u32::try_from(get::<i64>(
                    &row,
                    "outbound_proxy_max_websocket_connections",
                )?)
                .map_err(|source| invalid("invalid proxy websocket limit").with_source(source))?;
                gateway_core::account::OutboundProxy::parse(&url)
                    .map(|proxy| proxy.with_max_websocket_connections(limit))
                    .map_err(|source| invalid("invalid outbound proxy").with_source(source))
            })
            .transpose()?,
        id: get(&row, "id")?,
        provider_kind: get(&row, "provider_kind")?,
        name: get(&row, "name")?,
        notes: get(&row, "notes")?,
        email: get(&row, "email")?,
        upstream_user_id: get(&row, "upstream_user_id")?,
        upstream_account_id: get(&row, "upstream_account_id")?,
        plan_type: get(&row, "plan_type")?,
        authentication_kind: get(&row, "authentication_kind")?,
        credential_revision: Revision::new(to_u64(revision)?)?,
        has_refresh_token: get(&row, "has_refresh_token")?,
        access_token_expires_at: status.access_token_expires_at.map(Into::into),
        next_refresh_at: get(&row, "next_refresh_at")?,
        enabled: status.enabled,
        concurrency_limit,
        weight,
        model_access: get::<sqlx::types::Json<gateway_core::account::AccountModelAccess>>(
            &row,
            "model_access_json",
        )?
        .0,
        credential_state: status.credential_state,
        credential_observed_at: get(&row, "credential_observed_at")?,
        quota: status.quota,
        last_error_reason: status.last_error_reason,
        last_error_message: status.last_error_message,
        created_at: get(&row, "created_at")?,
        updated_at: get(&row, "updated_at")?,
    })
}

pub(crate) fn get<'r, T>(row: &'r sqlx::postgres::PgRow, column: &'static str) -> StoreResult<T>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get(column)
        .map_err(|source| invalid(column).with_source(source))
}

pub(crate) fn validate_object_size(
    field: &'static str,
    object: &JsonObject,
    max: usize,
) -> StoreResult<()> {
    let size = serde_json::to_vec(&object.as_value())
        .map_err(|error| StoreError::InvalidData {
            source: Some(error.into()),
            entity: ENTITY,
            message: "JSON encoding failed".to_owned(),
        })?
        .len();
    if size > max {
        Err(invalid(field))
    } else {
        Ok(())
    }
}

pub(crate) fn to_i64(value: u64) -> StoreResult<i64> {
    i64::try_from(value).map_err(|source| invalid("revision is too large").with_source(source))
}

pub(crate) fn to_u64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|source| invalid("revision must be positive").with_source(source))
}

pub(crate) fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        source: None,
        entity: ENTITY,
        message: message.to_owned(),
    }
}

pub(crate) fn account_status_facts_from_row(
    row: &sqlx::postgres::PgRow,
) -> StoreResult<gateway_core::account::AccountStatusFacts> {
    let credential_state = row
        .try_get::<String, _>("credential_state")
        .map_err(|source| invalid("invalid credential_state").with_source(source))?;
    let quota_access_state = parse_quota_access_state(&get::<String>(row, "quota_access_state")?)?;
    let quota_evidence = parse_quota_evidence(get(row, "quota_evidence")?)?;
    let quota_access_observed_at = get::<Option<DateTime<Utc>>>(row, "quota_access_observed_at")?;
    let quota_reset_at = get::<Option<DateTime<Utc>>>(row, "quota_reset_at")?;
    let quota = QuotaState::from_persisted(
        quota_access_state,
        quota_evidence,
        quota_access_observed_at.map(Into::into),
        quota_reset_at.map(Into::into),
    )
    .ok_or_else(|| invalid("invalid persisted quota fact"))?;
    Ok(gateway_core::account::AccountStatusFacts {
        enabled: get(row, "enabled")?,
        credential_state: parse_credential_state(&credential_state)?,
        access_token_expires_at: get::<Option<DateTime<Utc>>>(row, "access_token_expires_at")?
            .map(Into::into),
        quota,
        cooldown: None,
        last_error_reason: parse_error_reason(get(row, "last_error_reason")?)?,
        last_error_message: get(row, "last_error_message")?,
    })
}
