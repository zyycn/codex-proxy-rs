use axum::{
    middleware::from_fn,
    routing::{get, post},
    Router,
};

use crate::{
    http::{health::health, middleware::attach_request_id, v1::responses},
    state::AppState,
};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/responses", post(responses))
        .route("/v1/chat/completions", post(responses))
        .with_state(state)
        .layer(from_fn(attach_request_id))
}
