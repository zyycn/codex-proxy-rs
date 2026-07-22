//! 管理端 HTTP adapter、wire contract 与固定路由。

use axum::{
    Router,
    http::{HeaderValue, header},
    middleware,
    response::Response,
};

pub mod accounts;
pub mod auth;
pub mod client_keys;
pub mod observability;
pub mod settings;
pub mod system;
pub mod wire;

pub use auth::{AdminAuth, AdminSessionState};
pub use wire::{
    ADMIN_OK_CODE, ADMIN_OK_MESSAGE, AdminEnvelope, AdminError, AdminErrorBody, AdminErrorCode,
    AdminPageData, AdminResponse, PageMeta, WireValidationError,
};

/// 构造完整且固定的 `/api/admin` 路由。
pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .merge(accounts::router::<S>())
        .merge(auth::router::<S>())
        .merge(client_keys::router::<S>())
        .merge(observability::router::<S>())
        .merge(settings::router::<S>())
        .merge(system::router::<S>())
        .layer(middleware::map_response(no_store))
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
