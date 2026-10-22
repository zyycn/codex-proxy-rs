//! 普通 Client Key 的匿名账号额度查询，不执行推理准入或主动上游刷新

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use gateway_admin::model::upstream_quota::QuotaExportError;
use gateway_core::engine::execution::AuthenticatedClient;

use super::{
    auth::{client_access_error_response, verify_client},
    error::openai_error_response,
    middleware::{self as plugin_middleware, RequestInput},
};
use crate::{ApiState, auth::SessionState as _};

pub(super) fn router() -> Router<ApiState> {
    Router::new()
        .route(super::router::QUOTA_PATH, get(quota))
        .layer(middleware::map_response(no_store))
}

async fn quota(
    State(state): State<ApiState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let client = match verify_client(state.openai(), &headers).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error.into()),
    };
    let invalid_body = headers.contains_key(header::TRANSFER_ENCODING)
        || headers
            .get(header::CONTENT_LENGTH)
            .is_some_and(|value| value.as_bytes() != b"0");
    plugin_middleware::query_response(
        state.openai().execution(),
        client,
        RequestInput::query(super::router::QUOTA_PATH.to_owned(), None, &headers),
        move |client| quota_response(state, client, uri, invalid_body),
    )
    .await
}

async fn quota_response(
    state: ApiState,
    client: AuthenticatedClient,
    uri: axum::http::Uri,
    invalid_body: bool,
) -> Response {
    if uri.query().is_some() || invalid_body {
        return openai_error_response(
            StatusCode::BAD_REQUEST,
            "Quota query does not accept query parameters or a body",
            "invalid_request_error",
            "invalid_quota_query",
        )
        .into_response();
    }
    match state
        .admin_services()
        .upstream_quota()
        .export(client.policy())
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(error) => {
            let (status, kind, code) = match error {
                QuotaExportError::Unauthorized => (
                    StatusCode::UNAUTHORIZED,
                    "authentication_error",
                    "invalid_api_key",
                ),
                QuotaExportError::SharingDisabled => (
                    StatusCode::FORBIDDEN,
                    "permission_error",
                    "quota_sharing_disabled",
                ),
                QuotaExportError::Unavailable => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "server_error",
                    "quota_unavailable",
                ),
                QuotaExportError::ResponseTooLarge => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "server_error",
                    "quota_response_too_large",
                ),
            };
            openai_error_response(status, &error.to_string(), kind, code).into_response()
        }
    }
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
