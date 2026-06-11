use axum::{
    middleware::from_fn,
    routing::{get, post},
    Router,
};
use tower_http::trace::TraceLayer;

use crate::{
    http::{
        admin::logs,
        health::health,
        middleware::attach_request_id,
        v1::{models, responses},
    },
    state::AppState,
};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/responses", post(responses))
        .route("/v1/chat/completions", post(responses))
        .route("/v1/models", get(models))
        .route("/admin/logs", get(logs))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(from_fn(attach_request_id))
}
