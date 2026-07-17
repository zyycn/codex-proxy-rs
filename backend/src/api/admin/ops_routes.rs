//! 运维错误明细 HTTP 处理器。

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::{
    api::AppState,
    api::admin::{
        response::{AdminError, AdminPageEnvelope, AdminResponse},
        session::AdminAuth,
    },
    infra::{
        json::{NumberedPage, clamp_limit, clamp_page},
        time::china_datetime,
    },
    telemetry::{
        ops::query::OpsQueryError,
        ops::types::{OpsErrorFilter, OpsErrorLog},
    },
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OpsErrorsQuery {
    page: Option<u32>,
    page_size: Option<u32>,
    kind: Option<String>,
    client_api_key_id: Option<String>,
    provider: Option<String>,
    request_id: Option<String>,
    account_id: Option<String>,
    route: Option<String>,
    model: Option<String>,
    status_code: Option<i64>,
    client_status_code: Option<i64>,
    upstream_status_code: Option<i64>,
    transport: Option<String>,
    attempt_index: Option<i64>,
    failure_class: Option<String>,
    response_id: Option<String>,
    upstream_request_id: Option<String>,
    search: Option<String>,
    start_time: Option<String>,
    end_time: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpsErrorData {
    #[serde(flatten)]
    error: OpsErrorLog,
    created_at_display: String,
}

/// `GET /api/admin/ops/errors`
pub(crate) async fn ops_errors(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<OpsErrorsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let page = clamp_page(query.page.unwrap_or(1));
    let page_size = clamp_limit(query.page_size.unwrap_or(50));
    let filter = filter_from_query(query)?;

    state
        .services
        .ops_errors
        .list_page(page, page_size, filter)
        .await
        .map(|page| {
            let page = NumberedPage {
                items: ops_error_items(page.items),
                total: page.total,
                page: page.page,
                page_size: page.page_size,
            };
            AdminResponse::new(StatusCode::OK, AdminPageEnvelope::ok(page))
        })
        .map_err(log_error)
}

fn filter_from_query(query: OpsErrorsQuery) -> Result<OpsErrorFilter, AdminError> {
    Ok(OpsErrorFilter {
        kind: non_empty(query.kind),
        client_api_key_id: non_empty(query.client_api_key_id),
        provider: non_empty(query.provider),
        request_id: non_empty(query.request_id),
        account_id: non_empty(query.account_id),
        route: non_empty(query.route),
        model: non_empty(query.model),
        status_code: query.status_code,
        client_status_code: query.client_status_code,
        upstream_status_code: query.upstream_status_code,
        transport: non_empty(query.transport),
        attempt_index: query.attempt_index,
        failure_class: non_empty(query.failure_class),
        response_id: non_empty(query.response_id),
        upstream_request_id: non_empty(query.upstream_request_id),
        search: non_empty(query.search),
        start_time: optional_datetime(query.start_time)?,
        end_time: optional_datetime(query.end_time)?,
    })
}

fn ops_error_items(items: Vec<OpsErrorLog>) -> Vec<OpsErrorData> {
    items
        .into_iter()
        .map(|error| OpsErrorData {
            created_at_display: china_datetime(&error.created_at),
            error,
        })
        .collect()
}

fn optional_datetime(
    value: Option<String>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, AdminError> {
    let Some(value) = non_empty(value) else {
        return Ok(None);
    };
    chrono::DateTime::parse_from_rfc3339(&value)
        .map(|value| Some(value.with_timezone(&chrono::Utc)))
        .map_err(|_| AdminError::invalid_time_range("Invalid time range"))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn log_error(error: OpsQueryError) -> AdminError {
    AdminError::internal(error.to_string())
}
