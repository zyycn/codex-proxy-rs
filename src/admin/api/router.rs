use axum::{
    routing::{delete, get, patch, post},
    Router,
};

use crate::runtime::state::AppState;

use super::{
    accounts::{
        account_quota, accounts, auth_callback, auth_code_relay, auth_device_login,
        auth_device_poll, auth_login_start, batch_delete_accounts, batch_update_account_status,
        create_account, delete_account, delete_account_cookies, export_accounts,
        get_account_cookies, health_check_accounts, import_accounts, import_cli_auth,
        quota_warnings, refresh_account, reset_account_usage, set_account_cookies,
        update_account_label, update_account_status,
    },
    client_keys::{
        api_keys, batch_delete_api_keys, create_api_key, delete_api_key, export_api_keys,
        import_api_keys, update_api_key_label, update_api_key_status,
    },
    diagnostics::diagnostics,
    logs::{clear_logs, log_detail, logs, logs_state, update_logs_state},
    models::refresh_models,
    session::{auth_logout, auth_status, login},
    settings::{settings, update_settings},
    usage::{usage_stats, usage_stats_summary},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/login", post(login))
        .route("/api/admin/auth/status", get(auth_status))
        .route("/api/admin/auth/logout", post(auth_logout))
        .route("/api/admin/auth/login-start", post(auth_login_start))
        .route("/api/admin/auth/code-relay", post(auth_code_relay))
        .route("/auth/openai/callback", get(auth_callback))
        .route("/api/admin/auth/device-login", post(auth_device_login))
        .route(
            "/api/admin/auth/device-poll/{device_code}",
            get(auth_device_poll),
        )
        .route("/api/admin/diagnostics", get(diagnostics))
        .route("/api/admin/logs", get(logs).delete(clear_logs))
        .route(
            "/api/admin/logs/state",
            get(logs_state).patch(update_logs_state),
        )
        .route("/api/admin/logs/{log_id}", get(log_detail))
        .route("/api/admin/settings", get(settings).patch(update_settings))
        .route("/api/admin/refresh-models", post(refresh_models))
        .route("/api/admin/usage-stats", get(usage_stats))
        .route("/api/admin/usage-stats/summary", get(usage_stats_summary))
        .route("/api/admin/accounts", get(accounts).post(create_account))
        .route(
            "/api/admin/accounts/health-check",
            post(health_check_accounts),
        )
        .route("/api/admin/accounts/quota-warnings", get(quota_warnings))
        .route(
            "/api/admin/accounts/batch-delete",
            post(batch_delete_accounts),
        )
        .route(
            "/api/admin/accounts/batch-status",
            post(batch_update_account_status),
        )
        .route("/api/admin/accounts/export", get(export_accounts))
        .route(
            "/api/admin/accounts/{account_id}/refresh",
            post(refresh_account),
        )
        .route(
            "/api/admin/accounts/{account_id}/reset-usage",
            post(reset_account_usage),
        )
        .route("/api/admin/accounts/{account_id}/quota", get(account_quota))
        .route(
            "/api/admin/accounts/{account_id}/cookies",
            get(get_account_cookies)
                .post(set_account_cookies)
                .delete(delete_account_cookies),
        )
        .route("/api/admin/accounts/{account_id}", delete(delete_account))
        .route(
            "/api/admin/accounts/{account_id}/label",
            patch(update_account_label),
        )
        .route(
            "/api/admin/accounts/{account_id}/status",
            patch(update_account_status),
        )
        .route("/api/admin/accounts/import", post(import_accounts))
        .route("/api/admin/accounts/import-cli", post(import_cli_auth))
        .route("/api/admin/api-keys", get(api_keys).post(create_api_key))
        .route("/api/admin/api-keys/export", get(export_api_keys))
        .route("/api/admin/api-keys/import", post(import_api_keys))
        .route(
            "/api/admin/api-keys/batch-delete",
            post(batch_delete_api_keys),
        )
        .route("/api/admin/api-keys/{key_id}", delete(delete_api_key))
        .route(
            "/api/admin/api-keys/{key_id}/label",
            patch(update_api_key_label),
        )
        .route(
            "/api/admin/api-keys/{key_id}/status",
            patch(update_api_key_status),
        )
}
