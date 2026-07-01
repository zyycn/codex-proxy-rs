//! 管理端路由。

use axum::{
    routing::{get, post},
    Router,
};

use crate::runtime::state::AppState;

use super::{
    accounts::routes::{
        account_models, account_quota, accounts, batch_delete_accounts, create_account,
        export_accounts, health_check_accounts, import_accounts, oauth_authorize_account,
        oauth_exchange_account, refresh_account, test_account_connection, update_account,
    },
    auth::session::{login, logout, session_status},
    keys::routes::{api_keys, batch_delete_api_keys, create_api_key, update_api_key},
    monitoring::{
        dashboard::{dashboard_summary, dashboard_trend},
        usage_records::{
            clear_usage_records, usage_record_detail, usage_records,
            usage_records_endpoint_distribution, usage_records_latency_trend,
            usage_records_model_distribution, usage_records_summary, usage_records_token_trend,
        },
    },
    settings::routes::{
        admin_api_key_status, delete_admin_api_key, regenerate_admin_api_key, settings,
        update_settings,
    },
    system::routes::{
        check_updates, perform_update, restart, rollback, update_event_stream, update_status,
        version,
    },
};

/// 构造管理端路由。
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/login", post(login))
        .route("/api/admin/auth/status", get(session_status))
        .route("/api/admin/logout", post(logout))
        .route("/api/admin/settings", get(settings).post(update_settings))
        .route(
            "/api/admin/settings/admin-api-key",
            get(admin_api_key_status).delete(delete_admin_api_key),
        )
        .route(
            "/api/admin/settings/admin-api-key/regenerate",
            post(regenerate_admin_api_key),
        )
        .route("/api/admin/system/version", get(version))
        .route("/api/admin/system/check-updates", get(check_updates))
        .route("/api/admin/system/update-events", get(update_event_stream))
        .route("/api/admin/system/update", post(perform_update))
        .route("/api/admin/system/update-status", get(update_status))
        .route("/api/admin/system/rollback", post(rollback))
        .route("/api/admin/system/restart", post(restart))
        .route("/api/admin/dashboard/summary", get(dashboard_summary))
        .route("/api/admin/dashboard/trend", get(dashboard_trend))
        .route("/api/admin/accounts", get(accounts).post(create_account))
        .route("/api/admin/accounts/export", get(export_accounts))
        .route("/api/admin/accounts/import", post(import_accounts))
        .route(
            "/api/admin/accounts/oauth/authorize",
            post(oauth_authorize_account),
        )
        .route(
            "/api/admin/accounts/oauth/exchange",
            post(oauth_exchange_account),
        )
        .route("/api/admin/accounts/test", get(test_account_connection))
        .route("/api/admin/accounts/models", get(account_models))
        .route("/api/admin/accounts/delete", post(batch_delete_accounts))
        .route("/api/admin/accounts/update", post(update_account))
        .route("/api/admin/accounts/refresh", post(refresh_account))
        .route(
            "/api/admin/accounts/health-check",
            post(health_check_accounts),
        )
        .route("/api/admin/accounts/quota", get(account_quota))
        .route("/api/admin/usage/records", get(usage_records))
        .route(
            "/api/admin/usage/records/summary",
            get(usage_records_summary),
        )
        .route(
            "/api/admin/usage/records/insights/models",
            get(usage_records_model_distribution),
        )
        .route(
            "/api/admin/usage/records/insights/endpoints",
            get(usage_records_endpoint_distribution),
        )
        .route(
            "/api/admin/usage/records/insights/token-trend",
            get(usage_records_token_trend),
        )
        .route(
            "/api/admin/usage/records/insights/latency-trend",
            get(usage_records_latency_trend),
        )
        .route("/api/admin/usage/records/delete", post(clear_usage_records))
        .route("/api/admin/usage/records/detail", get(usage_record_detail))
        .route("/api/admin/keys", get(api_keys).post(create_api_key))
        .route("/api/admin/keys/delete", post(batch_delete_api_keys))
        .route("/api/admin/keys/update", post(update_api_key))
}
