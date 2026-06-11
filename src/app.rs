use axum::{
    routing::{get, post},
    Router,
};

use crate::{
    http::{health::health, v1::responses},
    state::AppState,
};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/responses", post(responses))
        .route("/v1/chat/completions", post(responses))
        .with_state(state)
}
