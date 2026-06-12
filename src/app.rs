use axum::{
    middleware::from_fn,
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;

use crate::{
    http::{
        admin::{
            accounts, api_keys, create_api_key, import_accounts, login, logs, refresh_models,
            settings, usage_stats, usage_stats_summary,
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
        .route("/admin/accounts/import", post(import_accounts))
        .route("/admin/api-keys", get(api_keys).post(create_api_key))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(from_fn(attach_request_id))
}
