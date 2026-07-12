//! OpenAI 兼容 API 路由。

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};

use crate::api::AppState;

use super::{
    models::{model_catalog, model_detail, model_info, models},
    responses::{responses, responses_websocket, review_responses},
};

pub const MAX_CLIENT_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;

/// 构造 OpenAI 兼容 API 路由。
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/responses", get(responses_websocket).post(responses))
        .route("/v1/responses/review", post(review_responses))
        .route("/v1/models", get(models))
        .route("/v1/models/catalog", get(model_catalog))
        .route("/v1/models/{model_id}", get(model_detail))
        .route("/v1/models/{model_id}/info", get(model_info))
        .layer(DefaultBodyLimit::max(MAX_CLIENT_REQUEST_BODY_BYTES))
}
