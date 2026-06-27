//! 管理端路由。

use axum::{
    routing::{get, post},
    Router,
};

use crate::runtime::state::AppState;

use super::{
    accounts::routes::{
        account_quota, account_test_models, accounts, batch_delete_accounts, create_account,
        export_accounts, get_account_cookies, health_check_accounts, import_accounts,
        quota_warnings, refresh_account, reset_account_usage, set_account_cookies,
        test_account_connection, update_account,
    },
    auth::session::{login, logout, session_status},
    keys::routes::{
        api_keys, batch_delete_api_keys, create_api_key, export_api_keys, update_api_key,
    },
    models::routes::refresh_models,
    monitoring::{
        dashboard::{dashboard_summary, dashboard_trend},
        logs::{clear_logs, log_detail, logs, logs_state, update_logs_state},
        usage::{usage_stats, usage_stats_summary},
    },
    settings::routes::{settings, update_settings},
};

/// 构造管理端路由。
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/login", post(login))
        .route("/api/admin/auth/status", get(session_status))
        .route("/api/admin/logout", post(logout))
        .route("/api/admin/settings", get(settings).post(update_settings))
        .route("/api/admin/dashboard/summary", get(dashboard_summary))
        .route("/api/admin/dashboard/trend", get(dashboard_trend))
        .route("/api/admin/models/refresh", post(refresh_models))
        .route("/api/admin/usage", get(usage_stats))
        .route("/api/admin/usage/summary", get(usage_stats_summary))
        .route("/api/admin/accounts", get(accounts).post(create_account))
        .route("/api/admin/accounts/export", get(export_accounts))
        .route("/api/admin/accounts/import", post(import_accounts))
        .route("/api/admin/accounts/quota-warnings", get(quota_warnings))
        .route(
            "/api/admin/accounts/health-check",
            post(health_check_accounts),
        )
        .route("/api/admin/accounts/test", post(test_account_connection))
        .route("/api/admin/accounts/models", get(account_test_models))
        .route("/api/admin/accounts/delete", post(batch_delete_accounts))
        .route("/api/admin/accounts/update", post(update_account))
        .route("/api/admin/accounts/refresh", post(refresh_account))
        .route("/api/admin/accounts/reset-usage", post(reset_account_usage))
        .route(
            "/api/admin/accounts/cookies",
            get(get_account_cookies).post(set_account_cookies),
        )
        .route("/api/admin/accounts/quota", get(account_quota))
        .route("/api/admin/logs", get(logs))
        .route("/api/admin/logs/delete", post(clear_logs))
        .route(
            "/api/admin/logs/state",
            get(logs_state).post(update_logs_state),
        )
        .route("/api/admin/logs/detail", get(log_detail))
        .route("/api/admin/keys", get(api_keys).post(create_api_key))
        .route("/api/admin/keys/export", get(export_api_keys))
        .route("/api/admin/keys/delete", post(batch_delete_api_keys))
        .route("/api/admin/keys/update", post(update_api_key))
}
