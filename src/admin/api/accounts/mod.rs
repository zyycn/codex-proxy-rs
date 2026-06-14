use axum::http::StatusCode;
use serde::Serialize;

use crate::{
    codex::accounts::service::{AccountServiceError, ValidatedAccountImportError},
    codex::accounts::{
        model::AccountStatus,
        repository::{StoredAccount, StoredAccountMetadata},
    },
};

use super::AdminError;

pub mod cookies;
pub mod create;
pub mod delete;
pub mod export;
pub mod health;
pub mod import;
pub mod lifecycle;
pub mod list;
pub mod oauth;
pub mod quota;

pub use cookies::{
    delete_account_cookies, get_account_cookies, set_account_cookies, AccountCookiesData,
    DeleteAccountCookiesData, SetAccountCookiesRequest,
};
pub use create::{create_account, CreateAccountRequest};
pub use delete::{
    batch_delete_accounts, delete_account, BatchDeleteAccountsData, BatchDeleteAccountsRequest,
    DeleteAccountData,
};
pub use export::{export_accounts, AccountExportQuery};
pub use health::{health_check_accounts, AccountProbeData, HealthCheckData, HealthCheckRequest};
pub use import::{import_accounts, import_cli_auth, AccountImportData, ImportCliAuthRequest};
pub use lifecycle::{
    batch_update_account_status, refresh_account, reset_account_usage, update_account_label,
    update_account_status, BatchUpdateAccountStatusData, BatchUpdateAccountStatusRequest,
    ResetAccountUsageData, UpdateAccountLabelData, UpdateAccountLabelRequest,
    UpdateAccountStatusData, UpdateAccountStatusRequest,
};
pub use list::{accounts, AccountsQuery};
pub use oauth::{
    auth_callback, auth_code_relay, auth_device_login, auth_device_poll, auth_login_start,
    AdminAuthCallbackQuery, AdminAuthCodeRelayData, AdminAuthCodeRelayRequest,
    AdminAuthDeviceLoginData, AdminAuthDevicePollData, AdminAuthLoginStartData,
};
pub use quota::{AccountQuotaData, AccountQuotaWarningsData};

pub(super) use health::account_probe_data_from_service;
pub(super) use quota::{account_quota, quota_warnings};

// 管理端账号接口按能力拆分；这里仅保留共享转换和对路由层稳定的 re-export。
fn account_service_error(error: AccountServiceError, request_id: &str) -> AdminError {
    match error {
        AccountServiceError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        ),
        AccountServiceError::UsageRepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account usage repository is not initialized",
            request_id,
        ),
        AccountServiceError::CookieRepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Cookie repository is not initialized",
            request_id,
        ),
        AccountServiceError::AccountNotFound => AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ),
        AccountServiceError::List => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to list accounts",
            request_id,
        ),
        AccountServiceError::Export => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to export accounts",
            request_id,
        ),
        AccountServiceError::Inspect => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to inspect account",
            request_id,
        ),
        AccountServiceError::ResetUsage => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to reset account usage",
            request_id,
        ),
        AccountServiceError::UpdateLabel => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to update account label",
            request_id,
        ),
        AccountServiceError::UpdateStatus => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to update account status",
            request_id,
        ),
        AccountServiceError::SyncStatus => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to sync account status",
            request_id,
        ),
        AccountServiceError::Delete => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to delete account",
            request_id,
        ),
        AccountServiceError::LoadCookies => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load account cookies",
            request_id,
        ),
        AccountServiceError::StoreCookies => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to store account cookies",
            request_id,
        ),
        AccountServiceError::DeleteCookies => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to delete account cookies",
            request_id,
        ),
        AccountServiceError::NoValidCookies => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "No valid cookies found",
            request_id,
        ),
        AccountServiceError::EmptyIds => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Account ids are required",
            request_id,
        ),
        AccountServiceError::InvalidStatus(message) => {
            AdminError::new(StatusCode::BAD_REQUEST, 40001, message, request_id)
        }
        AccountServiceError::LabelTooLong => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Account label must be 64 characters or fewer",
            request_id,
        ),
        AccountServiceError::QuotaWarnings => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load account quota warnings",
            request_id,
        ),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAccountData {
    pub id: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub status: String,
    pub access_token_expires_at: Option<String>,
    pub added_at: String,
    pub updated_at: String,
}

impl From<StoredAccountMetadata> for AdminAccountData {
    fn from(account: StoredAccountMetadata) -> Self {
        Self {
            id: account.id,
            email: account.email,
            account_id: account.account_id,
            user_id: account.user_id,
            label: account.label,
            plan_type: account.plan_type,
            status: account_status_value(account.status).to_string(),
            access_token_expires_at: account
                .access_token_expires_at
                .map(|value| value.to_rfc3339()),
            added_at: account.added_at.to_rfc3339(),
            updated_at: account.updated_at.to_rfc3339(),
        }
    }
}

fn admin_account_data_from_stored(account: StoredAccount) -> AdminAccountData {
    AdminAccountData {
        id: account.id,
        email: account.email,
        account_id: account.account_id,
        user_id: account.user_id,
        label: account.label,
        plan_type: account.plan_type,
        status: account_status_value(account.status).to_string(),
        access_token_expires_at: account
            .access_token_expires_at
            .map(|value| value.to_rfc3339()),
        added_at: account.added_at.to_rfc3339(),
        updated_at: account.updated_at.to_rfc3339(),
    }
}

pub(super) fn validated_account_import_error(
    error: ValidatedAccountImportError,
    request_id: &str,
) -> AdminError {
    match error {
        ValidatedAccountImportError::RepositoryUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Account repository is not initialized",
            request_id,
        ),
        ValidatedAccountImportError::TokenRequired => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Either token or refreshToken is required",
            request_id,
        ),
        ValidatedAccountImportError::TokenRefresherUnavailable => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Token refresher is not initialized",
            request_id,
        ),
        ValidatedAccountImportError::RefreshTransport => AdminError::new(
            StatusCode::BAD_GATEWAY,
            50201,
            "Refresh token exchange failed",
            request_id,
        ),
        ValidatedAccountImportError::RefreshRejected => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            "Refresh token exchange failed",
            request_id,
        ),
        ValidatedAccountImportError::InvalidToken(message) => {
            AdminError::new(StatusCode::BAD_REQUEST, 40001, message, request_id)
        }
        ValidatedAccountImportError::Inspect => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to inspect account",
            request_id,
        ),
        ValidatedAccountImportError::NotFound => AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Account not found",
            request_id,
        ),
        ValidatedAccountImportError::Update => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to update account",
            request_id,
        ),
        ValidatedAccountImportError::Insert => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to create account",
            request_id,
        ),
        ValidatedAccountImportError::Load => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            "Failed to load account",
            request_id,
        ),
    }
}

pub(super) fn account_export_ids(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|ids| ids.split(','))
        .filter_map(|id| {
            let id = id.trim();
            (!id.is_empty()).then(|| id.to_string())
        })
        .collect()
}

pub(super) fn account_status_value(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::Active => "active",
        AccountStatus::Expired => "expired",
        AccountStatus::QuotaExhausted => "quota_exhausted",
        AccountStatus::Refreshing => "refreshing",
        AccountStatus::Disabled => "disabled",
        AccountStatus::Banned => "banned",
    }
}
