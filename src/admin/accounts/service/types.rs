//! 管理端账号 DTO、错误与内部转换辅助。

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::{
    upstream::accounts::{
        model::{AccountStatus, AccountStatus as AcctStatus},
        store::StoredAccountMetadata,
    },
    upstream::transport::CodexClientError,
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct AdminAccountMetadata {
    pub id: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub status: AcctStatus,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub added_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<StoredAccountMetadata> for AdminAccountMetadata {
    fn from(m: StoredAccountMetadata) -> Self {
        let added_at = m
            .added_at
            .parse::<DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now());
        let updated_at = m
            .updated_at
            .parse::<DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now());
        Self {
            id: m.id,
            email: m.email,
            account_id: m.account_id,
            user_id: m.user_id,
            label: m.label,
            plan_type: m.plan_type,
            status: m.status,
            access_token_expires_at: m.access_token_expires_at,
            added_at,
            updated_at,
        }
    }
}

#[derive(Debug, Error)]
pub enum AdminAccountError {
    #[error("failed to list accounts")]
    List,
    #[error("failed to export accounts")]
    Export,
    #[error("failed to import accounts")]
    Import,
    #[error("failed to inspect account")]
    Inspect,
    #[error("account not found")]
    NotFound,
    #[error("failed to update label")]
    UpdateLabel,
    #[error("failed to update account metadata")]
    UpdateMetadata,
    #[error("failed to update status")]
    UpdateStatus,
    #[error("failed to delete account")]
    Delete,
    #[error("failed to load cookies")]
    LoadCookies,
    #[error("failed to store cookies")]
    StoreCookies,
    #[error("failed to delete cookies")]
    DeleteCookies,
    #[error("failed to update claims")]
    UpdateClaims,
    #[error("failed to reset usage")]
    ResetUsage,
    #[error("failed to sync account pool")]
    SyncAccountPool,
    #[error("failed to get quota warnings")]
    QuotaWarnings,
    #[error("failed to store quota")]
    StoreQuota,
    #[error("failed to fetch quota: {0}")]
    FetchQuota(String),
    #[error("health check failed")]
    HealthCheck,
    #[error("invalid status: {0}")]
    InvalidStatus(String),
    #[error("label must be 64 characters or fewer")]
    LabelTooLong,
    #[error("account ids are required")]
    EmptyIds,
    #[error("no importable accounts found")]
    NoImportableAccounts,
    #[error("invalid access token expires at")]
    InvalidAccessTokenExpiresAt,
    #[error("token is required")]
    TokenRequired,
    #[error("invalid token: {0}")]
    InvalidToken(&'static str),
    #[error("token refresh exchange failed: {0}")]
    RefreshTokenExchange(crate::upstream::accounts::token_refresh::RefreshFailure),
    #[error("no valid cookies provided")]
    NoValidCookies,
    #[error("account is {0}")]
    Inactive(AcctStatus),
}

#[derive(Debug, Clone)]
pub struct UpdatedAccountStatus {
    pub id: String,
    pub status: AcctStatus,
}

#[derive(Debug, Clone, Default)]
pub struct AdminAccountMetadataUpdate {
    pub email: Option<Option<String>>,
    pub account_id: Option<Option<String>>,
    pub user_id: Option<Option<String>>,
    pub label: Option<Option<String>>,
    pub plan_type: Option<Option<String>>,
    pub status: Option<String>,
}

impl AdminAccountMetadataUpdate {
    pub fn any(&self) -> bool {
        self.email.is_some()
            || self.account_id.is_some()
            || self.user_id.is_some()
            || self.label.is_some()
            || self.plan_type.is_some()
            || self.status.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct BatchDeleteAccounts {
    pub deleted: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BatchUpdateAccountStatus {
    pub updated: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ImportedAccounts {
    pub imported: u32,
    pub skipped: u32,
    pub source_format: &'static str,
}

#[derive(Debug, Clone)]
pub(super) struct ManualCreateTokens {
    pub(super) access_token: String,
    pub(super) refresh_token_for_new: Option<String>,
    pub(super) refresh_token_for_existing: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ResolvedImportTokens {
    pub(super) access_token: String,
    pub(super) refresh_token: Option<String>,
    pub(super) claims: Option<crate::upstream::accounts::token_refresh::ManualAccountClaims>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ImportSupplementalAccountInfo {
    pub(super) account_id: Option<String>,
    pub(super) user_id: Option<String>,
    pub(super) email: Option<String>,
    pub(super) plan_type: Option<String>,
    pub(super) quota_json: Option<String>,
    pub(super) quota_fetched_at: Option<DateTime<Utc>>,
    pub(super) status: Option<AccountStatus>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ImportSupplementalNeeds {
    pub(super) account_id: bool,
    pub(super) user_id: bool,
    pub(super) email: bool,
    pub(super) plan_type: bool,
    pub(super) quota: bool,
}

impl ImportSupplementalNeeds {
    pub(super) fn any(self) -> bool {
        self.account_id || self.user_id || self.email || self.plan_type || self.quota
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ImportedAccountState {
    Imported(String),
    Skipped,
}

pub(super) fn stored_to_admin_metadata(
    s: crate::upstream::accounts::store::StoredAccount,
) -> AdminAccountMetadata {
    AdminAccountMetadata::from(crate::upstream::accounts::store::StoredAccountMetadata {
        id: s.id,
        email: s.email,
        account_id: s.account_id,
        user_id: s.user_id,
        label: s.label,
        plan_type: s.plan_type,
        access_token_expires_at: s.access_token_expires_at,
        status: s.status,
        added_at: s.added_at,
        updated_at: s.updated_at,
    })
}

pub(super) fn import_usage_plan_type(usage: &serde_json::Value) -> Option<String> {
    usage
        .get("plan_type")
        .and_then(serde_json::Value::as_str)
        .and_then(normalized_plan_type)
}

pub(super) fn import_usage_string(usage: &serde_json::Value, key: &str) -> Option<String> {
    usage
        .get(key)
        .and_then(serde_json::Value::as_str)
        .and_then(|value| {
            crate::upstream::accounts::import_export::normalize_nonempty_str(Some(value))
        })
        .map(ToString::to_string)
}

pub(super) fn import_quota_plan_type(quota: &serde_json::Value) -> Option<String> {
    quota
        .get("plan_type")
        .and_then(serde_json::Value::as_str)
        .and_then(normalized_plan_type)
}

fn normalized_plan_type(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    (!value.is_empty() && !matches!(value.as_str(), "unknown" | "null")).then_some(value)
}

pub(super) fn import_status_from_usage_error(error: &CodexClientError) -> Option<AccountStatus> {
    if crate::upstream::transport::is_banned_upstream_error(error) {
        Some(AccountStatus::Banned)
    } else {
        None
    }
}

pub(super) fn parse_account_status(status: &str) -> Result<AcctStatus, AdminAccountError> {
    crate::upstream::accounts::import_export::parse_account_status(status)
        .map_err(|_| AdminAccountError::InvalidStatus(status.trim().to_ascii_lowercase()))
}

pub(super) fn parse_batch_account_status(status: &str) -> Result<AcctStatus, AdminAccountError> {
    crate::upstream::accounts::import_export::parse_batch_account_status(status)
        .map_err(|_| AdminAccountError::InvalidStatus(status.trim().to_ascii_lowercase()))
}
