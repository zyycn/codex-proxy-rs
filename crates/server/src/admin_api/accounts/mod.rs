//! 管理端账号处理器。

use axum::http::StatusCode;
use codex_proxy_core::accounts::model::AccountStatus;
use codex_proxy_runtime::services::{AdminAccountError, AdminAccountMetadata};
use serde::Serialize;

use crate::admin_api::AdminError;

pub mod cookies;
pub mod create;
pub mod export;
pub mod health;
pub mod import;
pub mod import_cli;
pub mod lifecycle;
pub mod list;
pub mod oauth;
pub mod quota;

pub use cookies::{delete_account_cookies, get_account_cookies, set_account_cookies};
pub use create::create_account;
pub use export::{export_accounts, AccountExportQuery};
pub use health::health_check_accounts;
pub use import::import_accounts;
pub use import_cli::import_cli_account;
pub use lifecycle::{
    batch_delete_accounts, batch_update_account_status, delete_account, refresh_account,
    reset_account_usage, update_account_label, update_account_status,
};
pub use list::{accounts, AccountsQuery};
pub use oauth::{
    auth_callback, auth_code_relay, auth_device_login, auth_device_poll, auth_login_start,
    auth_logout, auth_status,
};
pub use quota::{account_quota, quota_warnings};

/// 管理端账号元数据响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAccountData {
    /// 账号 ID。
    pub id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// ChatGPT 账号 ID。
    pub account_id: Option<String>,
    /// ChatGPT 用户 ID。
    pub user_id: Option<String>,
    /// 管理标签。
    pub label: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// 账号状态。
    pub status: String,
    /// access token 过期时间。
    pub access_token_expires_at: Option<String>,
    /// 创建时间。
    pub added_at: String,
    /// 更新时间。
    pub updated_at: String,
}

impl From<AdminAccountMetadata> for AdminAccountData {
    fn from(account: AdminAccountMetadata) -> Self {
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

pub(super) fn account_error(error: AdminAccountError, request_id: String) -> AdminError {
    match error {
        AdminAccountError::InvalidStatus(_)
        | AdminAccountError::LabelTooLong
        | AdminAccountError::EmptyIds
        | AdminAccountError::NoImportableAccounts
        | AdminAccountError::InvalidAccessTokenExpiresAt
        | AdminAccountError::TokenRequired
        | AdminAccountError::InvalidToken(_)
        | AdminAccountError::RefreshTokenExchange(_)
        | AdminAccountError::NoValidCookies => AdminError::new(
            StatusCode::BAD_REQUEST,
            40001,
            error.to_string(),
            request_id,
        ),
        AdminAccountError::NotFound => account_not_found(request_id),
        AdminAccountError::List
        | AdminAccountError::Export
        | AdminAccountError::Import
        | AdminAccountError::Inspect
        | AdminAccountError::UpdateLabel
        | AdminAccountError::UpdateStatus
        | AdminAccountError::Delete
        | AdminAccountError::LoadCookies
        | AdminAccountError::StoreCookies
        | AdminAccountError::DeleteCookies
        | AdminAccountError::UpdateClaims
        | AdminAccountError::ResetUsage
        | AdminAccountError::SyncAccountPool
        | AdminAccountError::QuotaWarnings
        | AdminAccountError::StoreQuota
        | AdminAccountError::FetchQuota(_)
        | AdminAccountError::HealthCheck => AdminError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            50001,
            error.to_string(),
            request_id,
        ),
        AdminAccountError::Inactive(status) => AdminError::new(
            StatusCode::CONFLICT,
            40901,
            format!(
                "Account is {}, cannot query quota",
                account_status_value(status)
            ),
            request_id,
        ),
    }
}

pub(super) fn account_not_found(request_id: String) -> AdminError {
    AdminError::new(
        StatusCode::NOT_FOUND,
        40401,
        "Account not found",
        request_id,
    )
}
