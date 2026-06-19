//! 客户端 key 列表处理器。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use codex_proxy_platform::json::{clamp_limit, Page};
use codex_proxy_runtime::state::AppState;

use crate::{
    admin_api::{
        client_keys::{client_key_error, ApiKeysQuery, ClientApiKeyData},
        require_admin_session, AdminError, AdminPageEnvelope, AdminResponse,
    },
    middleware::request_id::RequestId,
};

/// 分页列出客户端 API Key。
pub async fn api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ApiKeysQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let limit = clamp_limit(query.limit.unwrap_or(50));
    match state
        .services
        .admin_client_keys
        .list(query.cursor, limit)
        .await
    {
        Ok(page) => {
            let page = Page {
                items: page.items.into_iter().map(ClientApiKeyData::from).collect(),
                next_cursor: page.next_cursor,
            };
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminPageEnvelope::ok(page, limit, request_id),
            ))
        }
        Err(error) => Err(client_key_error(error, request_id)),
    }
}
