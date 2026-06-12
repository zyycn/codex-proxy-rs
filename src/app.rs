use axum::{
    middleware::from_fn,
    routing::{delete, get, patch, post},
    Router,
};
use tower_http::trace::TraceLayer;

use crate::{
    http::{
        admin::{
            accounts, api_keys, batch_delete_accounts, batch_delete_api_keys,
            batch_update_account_status, create_api_key, delete_account, delete_account_cookies,
            delete_api_key, get_account_cookies, import_accounts, login, logs, refresh_models,
            set_account_cookies, settings, update_account_label, update_account_status,
            update_api_key_label, update_api_key_status, usage_stats, usage_stats_summary,
        },
        health::health,
        middleware::attach_request_id,
        v1::{
            chat_completions, debug_models, model_catalog, model_detail, model_info, models,
            responses,
        },
    },
    state::AppState,
};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/responses", post(responses))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(models))
        .route("/v1/models/catalog", get(model_catalog))
        .route("/v1/models/{model_id}", get(model_detail))
        .route("/v1/models/{model_id}/info", get(model_info))
        .route("/debug/models", get(debug_models))
        .route("/admin/login", post(login))
        .route("/admin/logs", get(logs))
        .route("/admin/settings", get(settings))
        .route("/admin/refresh-models", post(refresh_models))
        .route("/admin/usage-stats", get(usage_stats))
        .route("/admin/usage-stats/summary", get(usage_stats_summary))
        .route("/admin/accounts", get(accounts))
        .route("/admin/accounts/batch-delete", post(batch_delete_accounts))
        .route(
            "/admin/accounts/batch-status",
            post(batch_update_account_status),
        )
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
        .route("/admin/api-keys", get(api_keys).post(create_api_key))
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
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(from_fn(attach_request_id))
}
