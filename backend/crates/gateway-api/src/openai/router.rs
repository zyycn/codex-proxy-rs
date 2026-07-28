//! OpenAI 客户端协议路由。

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};

use super::{
    models::{model_catalog, model_detail, model_info, models},
    responses::{responses, responses_websocket, review_responses},
};

use crate::ApiState;

/// 构造 OpenAI 客户端协议路由。
pub(crate) fn router() -> Router<ApiState> {
    Router::new()
        .route("/v1/responses", get(responses_websocket).post(responses))
        .route("/v1/responses/review", post(review_responses))
        .route("/v1/models", get(models))
        .route("/v1/models/catalog", get(model_catalog))
        .route("/v1/models/{model_id}/info", get(model_info))
        // 官方 OpenAI 模型详情合同使用 path ID；它不属于 Admin API 约束。
        .route("/v1/models/{model_id}", get(model_detail))
        // Responses 正文属于 OpenAI/Codex 协议；代理不能用私有大小上限提前拒绝
        // 上游本可接受的未来 payload。
        .layer(DefaultBodyLimit::disable())
}
