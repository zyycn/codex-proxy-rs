//! Bearer Key 的 CPR 互联导出路由，只消费 Admin 的本地观测

mod wire;

use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use gateway_admin::PeerLinkError;
use gateway_admin::model::peer_link::PeerLinkLimits;
use gateway_core::engine::execution::AuthenticatedClient;
use gateway_protocol::peer::{MAX_ACCOUNTS, MAX_MODELS, MAX_QUOTA_WINDOWS, MAX_RESPONSE_BYTES};
use serde::Deserialize;

use super::{
    auth::{client_access_error_response, verify_client},
    error::{missing_client_api_key_response, openai_error_response},
};
use crate::{
    ApiState,
    auth::SessionState as _,
    openai::middleware::{self as plugin_middleware, RequestInput},
};

pub(super) fn router() -> Router<ApiState> {
    Router::new()
        .route(super::router::PEER_LINK_PATH, get(peer_link))
        .layer(middleware::map_response(no_store))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerQuery {}

async fn peer_link(State(state): State<ApiState>, headers: HeaderMap, uri: Uri) -> Response {
    let client = match verify_client(state.openai(), &headers).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error.into()),
    };
    plugin_middleware::query_response(
        state.openai().execution(),
        client,
        RequestInput::query(super::router::PEER_LINK_PATH.to_owned(), None, &headers),
        move |client| peer_response(state, client, uri),
    )
    .await
}

async fn peer_response(state: ApiState, client: AuthenticatedClient, uri: Uri) -> Response {
    if Query::<PeerQuery>::try_from_uri(&uri).is_err() {
        return openai_error_response(
            StatusCode::BAD_REQUEST,
            "Peer query does not accept query parameters.",
            "invalid_request_error",
            "invalid_peer_query",
        )
        .into_response();
    }
    match state
        .admin_services()
        .peer_link()
        .snapshot(
            &client,
            PeerLinkLimits {
                max_accounts: MAX_ACCOUNTS,
                max_quota_windows: MAX_QUOTA_WINDOWS,
                max_models: MAX_MODELS,
            },
        )
        .await
    {
        Ok(snapshot) => match serde_json::to_vec(&wire::response(snapshot)) {
            Ok(body) if body.len() <= MAX_RESPONSE_BYTES => {
                ([(header::CONTENT_TYPE, "application/json")], body).into_response()
            }
            _ => unavailable(),
        },
        Err(PeerLinkError::Unauthorized) => missing_client_api_key_response().into_response(),
        Err(PeerLinkError::Forbidden) => openai_error_response(
            StatusCode::FORBIDDEN,
            "CPR Link sharing is not enabled for this API key.",
            "permission_error",
            "peer_sharing_disabled",
        )
        .into_response(),
        Err(PeerLinkError::RateLimited { retry_after }) => {
            let mut response = openai_error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "Peer query rate limit reached.",
                "rate_limit_error",
                "peer_query_rate_limited",
            )
            .into_response();
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(retry_after));
            response
        }
        Err(PeerLinkError::Unavailable(_) | PeerLinkError::SnapshotTooLarge) => unavailable(),
    }
}

fn unavailable() -> Response {
    openai_error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "Peer snapshot is currently unavailable.",
        "server_error",
        "peer_snapshot_unavailable",
    )
    .into_response()
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
