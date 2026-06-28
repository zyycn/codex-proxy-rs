//! 事件日志 HTTP 处理器。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::{
    admin::monitoring::{
        event_store::{AdminLogError, AdminLogFilter},
        events::{EventLevel, EventLog},
    },
    admin::{
        auth::session::require_admin_session,
        response::{AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse},
    },
    infra::{
        json::{clamp_limit, clamp_page, NumberedPage, Page},
        time::china_datetime,
    },
    runtime::state::AppState,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogsQuery {
    cursor: Option<String>,
    limit: Option<u32>,
    page: Option<u32>,
    page_size: Option<u32>,
    kind: Option<String>,
    level: Option<String>,
    request_id: Option<String>,
    account_id: Option<String>,
    route: Option<String>,
    model: Option<String>,
    status_code: Option<i64>,
    transport: Option<String>,
    attempt_index: Option<i64>,
    upstream_status_code: Option<i64>,
    failure_class: Option<String>,
    response_id: Option<String>,
    upstream_request_id: Option<String>,
    search: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogDetailQuery {
    id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClearLogsData {
    cleared: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LogData {
    #[serde(flatten)]
    log: EventLog,
    created_at_display: String,
}

/// `GET /api/admin/logs`
pub(crate) async fn logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LogsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_session(&state, &headers).await?;
    let limit = clamp_limit(query.page_size.or(query.limit).unwrap_or(50));
    let page = query.page;
    let use_numbered_page = page.is_some() || query.page_size.is_some();
    let cursor = query.cursor.clone();
    let filter = filter_from_query(query)?;

    if use_numbered_page {
        return match state
            .services
            .logs
            .list_page(clamp_page(page.unwrap_or(1)), limit, filter)
            .await
        {
            Ok(page) => {
                let page = NumberedPage {
                    items: page.items.into_iter().map(LogData::from).collect(),
                    total: page.total,
                    page: page.page,
                    page_size: page.page_size,
                };
                Ok(AdminResponse::new(
                    StatusCode::OK,
                    AdminPageEnvelope::numbered(page),
                ))
            }
            Err(error) => Err(log_error(&error)),
        };
    }

    match state.services.logs.list(cursor, limit, filter).await {
        Ok(page) => {
            let page = Page {
                items: page.items.into_iter().map(LogData::from).collect(),
                next_cursor: page.next_cursor,
            };
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminPageEnvelope::ok(page, limit),
            ))
        }
        Err(error) => Err(log_error(&error)),
    }
}

/// `GET /api/admin/logs/detail`
pub(crate) async fn log_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LogDetailQuery>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_session(&state, &headers).await?;
    match state.services.logs.get(&query.id).await {
        Ok(Some(log)) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(LogData::from(log)),
        )),
        Ok(None) => Err(AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Log event not found",
        )),
        Err(error) => Err(log_error(&error)),
    }
}

/// `POST /api/admin/logs/delete`
pub(crate) async fn clear_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_session(&state, &headers).await?;
    match state.services.logs.clear().await {
        Ok(cleared) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(ClearLogsData {
                cleared: cleared.cleared,
            }),
        )),
        Err(error) => Err(log_error(&error)),
    }
}

fn filter_from_query(query: LogsQuery) -> Result<AdminLogFilter, AdminError> {
    Ok(AdminLogFilter {
        kind: non_empty(query.kind),
        level: level_from_query(query.level)
            .map_err(|message| AdminError::new(StatusCode::BAD_REQUEST, 40001, message))?,
        request_id: non_empty(query.request_id),
        account_id: non_empty(query.account_id),
        route: non_empty(query.route),
        model: non_empty(query.model),
        status_code: query.status_code,
        transport: non_empty(query.transport),
        attempt_index: query.attempt_index,
        upstream_status_code: query.upstream_status_code,
        failure_class: non_empty(query.failure_class),
        response_id: non_empty(query.response_id),
        upstream_request_id: non_empty(query.upstream_request_id),
        search: non_empty(query.search),
    })
}

fn log_error(error: &AdminLogError) -> AdminError {
    match error {
        AdminLogError::List
        | AdminLogError::Get
        | AdminLogError::Clear
        | AdminLogError::Append
        | AdminLogError::Trim => {
            AdminError::new(StatusCode::INTERNAL_SERVER_ERROR, 50001, error.to_string())
        }
    }
}

fn level_from_query(value: Option<String>) -> Result<Option<EventLevel>, String> {
    let Some(value) = non_empty(value) else {
        return Ok(None);
    };
    match value.as_str() {
        "debug" => Ok(Some(EventLevel::Debug)),
        "info" => Ok(Some(EventLevel::Info)),
        "warn" => Ok(Some(EventLevel::Warn)),
        "error" => Ok(Some(EventLevel::Error)),
        other => Err(format!("Unsupported log level: {other}")),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

impl From<EventLog> for LogData {
    fn from(log: EventLog) -> Self {
        Self {
            created_at_display: china_datetime(&log.created_at),
            log,
        }
    }
}
