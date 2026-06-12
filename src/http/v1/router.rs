use axum::{
    routing::{get, post},
    Router,
};

use crate::app::state::AppState;

use super::{
    chat::chat_completions,
    models::{debug_models, model_catalog, model_detail, model_info, models},
    responses::responses,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/responses", post(responses))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(models))
        .route("/v1/models/catalog", get(model_catalog))
        .route("/v1/models/{model_id}", get(model_detail))
        .route("/v1/models/{model_id}/info", get(model_info))
        .route("/debug/models", get(debug_models))
}
