//! OpenAI 客户端协议路由

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};

use super::{
    images::{image_edits, image_generations},
    live,
    models::{model_detail, models},
    responses::{responses, responses_websocket},
    search::standalone_search,
    usage,
};

use crate::ApiState;

// 公开协议路径由 API 层统一定义；Core 使用操作类型，不根据这些路径分派业务
pub(super) const RESPONSES_PATH: &str = "/v1/responses";
pub(super) const IMAGE_GENERATIONS_PATH: &str = "/v1/images/generations";
pub(super) const IMAGE_EDITS_PATH: &str = "/v1/images/edits";
pub(super) const SEARCH_PATH: &str = "/v1/alpha/search";
pub(super) const MODELS_PATH: &str = "/v1/models";
pub(super) const MODEL_DETAIL_PATH: &str = "/v1/models/{model_id}";
pub(super) const USAGE_PATH: &str = "/v1/usage";

/// 构造 OpenAI 客户端协议路由
pub(crate) fn router() -> Router<ApiState> {
    Router::new()
        .route(IMAGE_GENERATIONS_PATH, post(image_generations))
        .route(IMAGE_EDITS_PATH, post(image_edits))
        .route(SEARCH_PATH, post(standalone_search))
        .route(RESPONSES_PATH, get(responses_websocket).post(responses))
        .route(MODELS_PATH, get(models))
        // 官方 OpenAI 模型详情合同使用 path ID；它不属于 Admin API 约束
        .route(MODEL_DETAIL_PATH, get(model_detail))
        .merge(usage::router())
        .merge(live::router())
        // OpenAI 数据面正文属于客户端/上游协议；代理不能用私有大小上限提前拒绝
        // 上游本可接受的未来 payload
        .layer(DefaultBodyLimit::disable())
}
