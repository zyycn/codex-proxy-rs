use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};

use crate::{
    platform::http::request_id::RequestId,
    runtime::state::AppState,
    utils::pagination::{clamp_limit, Page},
};

use super::{api_key_service_error, ApiKeysQuery, ClientApiKeyData};
use crate::admin::api::{require_admin_session, AdminError, AdminPageEnvelope, AdminResponse};

pub async fn api_keys(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<ApiKeysQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;
    let limit = clamp_limit(query.limit.unwrap_or(50));
    match state.services.api_keys.list(query.cursor, limit).await {
        Ok(page) => {
            let Page { items, next_cursor } = page;
            let page = Page {
                items: items.into_iter().map(ClientApiKeyData::from).collect(),
                next_cursor,
            };
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminPageEnvelope::ok(page, limit, request_id),
            ))
        }
        Err(error) => Err(api_key_service_error(error, request_id)),
    }
}
