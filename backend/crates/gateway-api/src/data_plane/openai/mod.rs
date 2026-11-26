//! 绑定 OpenAI Provider 的 Images、独立搜索与 Live 客户端入口

mod endpoints;
mod images;
mod live;
mod search;

use axum::{Router, routing::post};

pub(crate) const IMAGE_GENERATIONS_PATH: &str = "/v1/images/generations";
pub(crate) const IMAGE_EDITS_PATH: &str = "/v1/images/edits";
pub(crate) const SEARCH_PATH: &str = "/v1/alpha/search";

pub(crate) fn router() -> Router<crate::ApiState> {
    Router::new()
        .route(IMAGE_GENERATIONS_PATH, post(images::image_generations))
        .route(IMAGE_EDITS_PATH, post(images::image_edits))
        .route(SEARCH_PATH, post(search::standalone_search))
        .merge(live::router())
}
