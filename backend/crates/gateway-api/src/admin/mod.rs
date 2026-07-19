//! 管理端 HTTP adapter、wire contract 与固定路由。

use axum::{
    Router,
    http::{HeaderValue, header},
    middleware,
    response::Response,
};

pub mod accounts;
pub mod auth;
pub mod catalog;
pub mod client_keys;
pub mod observability;
pub mod openai;
pub mod settings;
pub mod system;
pub mod wire;
pub mod xai;

pub use auth::{
    AdminAuth, AdminPrincipal, AdminRequestContext, AdminServiceError, AdminServiceErrorKind,
    AdminSessionResolver, AdminSessionState,
};
pub use settings::{
    AdminApiKeyStatus, DeletedAdminApiKey, ProviderModelMappings, RegeneratedAdminApiKey,
    RuntimeSettingsView, UpdateRuntimeSettingsRequest,
};
pub use wire::{
    ADMIN_OK_CODE, ADMIN_OK_MESSAGE, AdminEnvelope, AdminError, AdminErrorBody, AdminErrorCode,
    AdminPageData, AdminResponse, PageMeta, WireValidationError,
};

use accounts::AccountAdminState;
use auth::AdminAuthState;
use catalog::CatalogAdminState;
use client_keys::ClientKeyAdminState;
use observability::ObservabilityAdminState;
use openai::CodexAdminState;
use settings::AdminSettingsState;
use system::SystemAdminState;
use xai::XaiAdminState;

/// 构造完整且固定的 `/api/admin` 路由。
pub fn router<S>() -> Router<S>
where
    S: AccountAdminState
        + AdminAuthState
        + CatalogAdminState
        + ClientKeyAdminState
        + CodexAdminState
        + ObservabilityAdminState
        + AdminSettingsState
        + SystemAdminState
        + XaiAdminState
        + Clone
        + Send
        + Sync
        + 'static,
{
    Router::new()
        .merge(accounts::router::<S>())
        .merge(auth::router::<S>())
        .merge(catalog::router::<S>())
        .merge(client_keys::router::<S>())
        .merge(openai::router::<S>())
        .merge(observability::router::<S>())
        .merge(settings::router::<S>())
        .merge(system::router::<S>())
        .merge(xai::router::<S>())
        .layer(middleware::map_response(no_store))
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
