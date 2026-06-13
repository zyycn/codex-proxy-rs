use axum::{middleware::from_fn, routing::get, Router};
use tower_http::trace::TraceLayer;

use crate::http::{
    admin,
    diagnostics::{debug_fingerprint, diagnostics},
    health::health,
    middleware::attach_request_id,
    v1,
};

use super::state::AppState;

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/debug/diagnostics", get(diagnostics))
        .route("/debug/fingerprint", get(debug_fingerprint))
        .merge(v1::router())
        .merge(admin::router())
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(from_fn(attach_request_id))
}
