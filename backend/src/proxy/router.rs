//! Proxy 路由聚合。

use axum::Router;

use crate::runtime::state::AppState;

use super::openai;

/// 构造 Proxy 路由。
pub fn router() -> Router<AppState> {
    openai::routes::router()
}
