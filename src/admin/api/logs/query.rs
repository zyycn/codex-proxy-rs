use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use serde::Deserialize;

use crate::{
    codex::events::service::LogListFilter, platform::http::request_id::RequestId,
    runtime::state::AppState, utils::pagination::clamp_limit,
};

use super::log_service_error;
use crate::admin::api::{require_admin_session, AdminError, AdminPageEnvelope, AdminResponse};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
    pub kind: Option<String>,
    pub level: Option<String>,
    pub request_id: Option<String>,
    pub account_id: Option<String>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub status_code: Option<i64>,
    pub search: Option<String>,
}

pub async fn logs(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Query(query): Query<LogsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let request_id = request_id.as_str().to_string();
    require_admin_session(&state, &headers, &request_id).await?;

    let limit = clamp_limit(query.limit.unwrap_or(50));
    let cursor = query.cursor.clone();
    match state.services.logs.list(cursor, limit, query.into()).await {
        Ok(page) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminPageEnvelope::ok(page, limit, request_id),
        )),
        Err(error) => Err(log_service_error(error, request_id)),
    }
}

impl From<LogsQuery> for LogListFilter {
    fn from(query: LogsQuery) -> Self {
        Self {
            kind: non_empty(query.kind),
            level: non_empty(query.level),
            request_id: non_empty(query.request_id),
            account_id: non_empty(query.account_id),
            route: non_empty(query.route),
            model: non_empty(query.model),
            status_code: query.status_code,
            search: non_empty(query.search),
        }
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
