//! 顶层 HTTP 路由。

use std::path::Path;

use axum::{middleware, Router};

use codex_proxy_runtime::state::AppState;

use crate::{
    admin_api,
    middleware::{request_id::attach_request_id, trace::http_trace_layer},
    openai_api,
};

/// 默认前端构建产物目录。
pub const DEFAULT_ASSET_DIST_DIR: &str = "web/dist";

/// 构造整个 HTTP 服务路由。
pub fn router() -> Router<AppState> {
    router_with_assets(DEFAULT_ASSET_DIST_DIR)
}

/// 使用指定前端构建产物目录构造整个 HTTP 服务路由。
pub fn router_with_assets(dist_dir: impl AsRef<Path>) -> Router<AppState> {
    Router::new()
        .merge(openai_api::router::router())
        .merge(admin_api::router())
        .fallback_service(codex_proxy_assets::router::spa_router(dist_dir))
        .layer(http_trace_layer())
        .layer(middleware::from_fn(attach_request_id))
}
