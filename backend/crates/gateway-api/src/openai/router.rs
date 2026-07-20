//! OpenAI 客户端协议路由。

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};

use super::{
    models::{model_detail, models},
    responses::{responses, responses_websocket, review_responses},
};

use crate::ApiState;

/// OpenAI 请求正文上限。
pub const MAX_CLIENT_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;

/// 构造 OpenAI 客户端协议路由。
pub(crate) fn router() -> Router<ApiState> {
    Router::new()
        .route("/v1/responses", get(responses_websocket).post(responses))
        .route("/v1/responses/review", post(review_responses))
        .route("/v1/models", get(models))
        // 官方 OpenAI 模型详情合同使用 path ID；它不属于 Admin API 约束。
        .route("/v1/models/{model_id}", get(model_detail))
        .layer(DefaultBodyLimit::max(MAX_CLIENT_REQUEST_BODY_BYTES))
}
