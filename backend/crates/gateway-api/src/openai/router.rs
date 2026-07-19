//! OpenAI 客户端协议路由。

use axum::{Router, extract::DefaultBodyLimit, routing::get};

use super::{
    models::{model_detail, models},
    responses::{responses, responses_websocket},
    service::OpenAiApiState,
};

/// OpenAI 请求正文上限。
pub const MAX_CLIENT_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;

/// 构造 OpenAI 客户端协议路由。
pub fn router<S>() -> Router<S>
where
    S: OpenAiApiState,
{
    Router::new()
        .route(
            "/v1/responses",
            get(responses_websocket::<S>).post(responses::<S>),
        )
        .route("/v1/models", get(models::<S>))
        // 官方 OpenAI 模型详情合同使用 path ID；它不属于 Admin API 约束。
        .route("/v1/models/{model_id}", get(model_detail::<S>))
        .layer(DefaultBodyLimit::max(MAX_CLIENT_REQUEST_BODY_BYTES))
}
