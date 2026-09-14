//! Client API Key 自助用量 HTTP adapter 与固定路由。

use crate::auth::SessionState;
use axum::{
    Router,
    http::{HeaderValue, header},
    middleware,
    response::Response,
    routing::any,
};

mod presenter;
mod system;
mod usage;

pub(crate) fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .merge(system::router::<S>())
        .merge(usage::router::<S>())
        .method_not_allowed_fallback(method_not_allowed)
        .route("/api/client", any(client_not_found))
        .route("/api/client/{*path}", any(client_not_found))
        .layer(middleware::map_response(no_store))
}

async fn method_not_allowed() -> crate::admin::AdminError {
    crate::admin::AdminError::method_not_allowed()
}

async fn client_not_found() -> crate::admin::AdminError {
    crate::admin::AdminError::not_found("Client 接口不存在")
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    response
}
