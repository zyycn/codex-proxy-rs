use axum::{
    routing::{delete, get, patch, post},
    Router,
};

use crate::app::state::AppState;

use super::{
    accounts::{
        account_quota, accounts, batch_delete_accounts, batch_update_account_status,
        create_account, delete_account, delete_account_cookies, export_accounts,
        get_account_cookies, health_check_accounts, import_accounts, import_cli_auth,
        quota_warnings, refresh_account, reset_account_usage, set_account_cookies,
        update_account_label, update_account_status,
    },
    api_keys::{
        api_keys, batch_delete_api_keys, create_api_key, delete_api_key, export_api_keys,
        import_api_keys, update_api_key_label, update_api_key_status,
    },
    auth::{
        auth_callback, auth_code_relay, auth_device_login, auth_device_poll, auth_login_start,
        auth_logout, auth_status, login,
    },
    logs::logs,
    models::refresh_models,
    settings::settings,
    usage::{usage_stats, usage_stats_summary},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/login", post(login))
        .route("/admin/auth/status", get(auth_status))
        .route("/admin/auth/logout", post(auth_logout))
        .route("/admin/auth/login-start", post(auth_login_start))
        .route("/admin/auth/code-relay", post(auth_code_relay))
        .route("/admin/auth/callback", get(auth_callback))
        // OpenAI OAuth 客户端注册的是 /auth/callback；handler 内仍要求管理员会话。
        .route("/auth/callback", get(auth_callback))
        .route("/admin/auth/device-login", post(auth_device_login))
        .route(
            "/admin/auth/device-poll/{device_code}",
            get(auth_device_poll),
        )
        .route("/admin/logs", get(logs))
        .route("/admin/settings", get(settings))
        .route("/admin/refresh-models", post(refresh_models))
        .route("/admin/usage-stats", get(usage_stats))
        .route("/admin/usage-stats/summary", get(usage_stats_summary))
        .route("/admin/accounts", get(accounts).post(create_account))
        .route("/admin/accounts/health-check", post(health_check_accounts))
        .route("/admin/accounts/quota-warnings", get(quota_warnings))
        .route("/admin/accounts/batch-delete", post(batch_delete_accounts))
        .route(
            "/admin/accounts/batch-status",
            post(batch_update_account_status),
        )
        .route("/admin/accounts/export", get(export_accounts))
        .route(
            "/admin/accounts/{account_id}/refresh",
            post(refresh_account),
        )
        .route(
            "/admin/accounts/{account_id}/reset-usage",
            post(reset_account_usage),
        )
        .route("/admin/accounts/{account_id}/quota", get(account_quota))
        .route(
            "/admin/accounts/{account_id}/cookies",
            get(get_account_cookies)
                .post(set_account_cookies)
                .delete(delete_account_cookies),
        )
        .route("/admin/accounts/{account_id}", delete(delete_account))
        .route(
            "/admin/accounts/{account_id}/label",
            patch(update_account_label),
        )
        .route(
            "/admin/accounts/{account_id}/status",
            patch(update_account_status),
        )
        .route("/admin/accounts/import", post(import_accounts))
        .route("/admin/accounts/import-cli", post(import_cli_auth))
        .route("/admin/api-keys", get(api_keys).post(create_api_key))
        .route("/admin/api-keys/export", get(export_api_keys))
        .route("/admin/api-keys/import", post(import_api_keys))
        .route("/admin/api-keys/batch-delete", post(batch_delete_api_keys))
        .route("/admin/api-keys/{key_id}", delete(delete_api_key))
        .route(
            "/admin/api-keys/{key_id}/label",
            patch(update_api_key_label),
        )
        .route(
            "/admin/api-keys/{key_id}/status",
            patch(update_api_key_status),
        )
}
