//! 顶层 HTTP 路由 —— 组合 OpenAI API、管理端 API 和静态资源服务。

use std::path::Path;

use axum::{middleware, Router};

use crate::admin;
use crate::app::state::AppState;
use crate::gateway;
use crate::http::middleware::{request_id::attach_request_id, trace::http_trace_layer};
use crate::web;

/// 默认前端构建产物目录。
pub const DEFAULT_ASSET_DIST_DIR: &str = "web/dist";

/// 构造整个 HTTP 服务路由。
pub fn router() -> Router<AppState> {
    router_with_assets(DEFAULT_ASSET_DIST_DIR)
}

/// 使用指定前端构建产物目录构造整个 HTTP 服务路由。
pub fn router_with_assets(dist_dir: impl AsRef<Path>) -> Router<AppState> {
    Router::new()
        .merge(gateway::openai::router())
        .merge(admin::router())
        .fallback_service(web::assets::spa_router(dist_dir))
        .layer(http_trace_layer())
        .layer(middleware::from_fn(attach_request_id))
}
