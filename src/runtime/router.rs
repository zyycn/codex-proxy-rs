use axum::{middleware::from_fn, routing::get, Router};
use tower_http::trace::TraceLayer;

use crate::{
    admin::http as admin_http,
    codex::serving::http::{
        diagnostics::{debug_fingerprint, debug_upstream, diagnostics},
        router as serving_http,
    },
    platform::http::{health::health, middleware::attach_request_id},
};

use super::state::AppState;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/debug/diagnostics", get(diagnostics))
        .route("/debug/fingerprint", get(debug_fingerprint))
        .route("/debug/upstream", get(debug_upstream))
        .merge(serving_http::router())
        .merge(admin_http::router())
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(from_fn(attach_request_id))
}
