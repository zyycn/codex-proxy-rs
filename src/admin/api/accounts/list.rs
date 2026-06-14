use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use serde::Deserialize;

use crate::{
    platform::http::request_id::RequestId,
    runtime::state::AppState,
    utils::pagination::{clamp_limit, Page},
};

use super::super::{require_admin_session, AdminError, AdminPageEnvelope, AdminResponse};
use super::{account_service_error, AdminAccountData};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

pub async fn accounts(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<AccountsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let limit = clamp_limit(query.limit.unwrap_or(50));
    let page = state
        .services
        .accounts
        .list(query.cursor, limit)
        .await
        .map_err(|error| account_service_error(error, &request_id))?;
    let Page { items, next_cursor } = page;
    let page = Page {
        items: items.into_iter().map(AdminAccountData::from).collect(),
        next_cursor,
    };

    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminPageEnvelope::ok(page, limit, request_id),
    ))
}
