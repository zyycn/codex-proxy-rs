//! 顶层 HTTP 路由 —— 组合 OpenAI API、管理端 API 和静态资源服务。

use std::path::Path;

use axum::{extract::State, http::StatusCode, middleware, routing::get, Router};

use crate::admin;
use crate::http::middleware::{request_id::attach_request_id, trace::http_trace_layer};
use crate::proxy;
use crate::runtime::state::AppState;
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
        .route("/healthz", get(healthz))
        .merge(proxy::router::router())
        .merge(admin::router::router())
        .fallback_service(web::assets::spa_router(dist_dir))
        .layer(http_trace_layer())
        .layer(middleware::from_fn(attach_request_id))
}

async fn healthz(State(state): State<AppState>) -> StatusCode {
    match sqlx::query("select 1")
        .execute(state.services.background_tasks.client_keys.pool())
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(error) => {
            tracing::warn!(error = %error, "health check failed");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}
