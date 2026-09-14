//! 当前 Client 会话 Key 的概览与使用统计接口。

use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use gateway_admin::model::{
    PageSize,
    observability::{
        DiagnosticDimension, OpsErrorFilter, OpsErrorQuery, TimeRange, UsageFilter, UsageQuery,
    },
};
use serde::Deserialize;

use crate::{
    admin::{AdminEnvelope, AdminError, AdminQuery, AdminResponse, wire::map_admin_service_error},
    auth::SessionState,
};

use super::presenter::{
    ClientDiagnosticsData, ClientOpsErrorsData, ClientOverviewData, ClientUsageInsightsData,
    ClientUsageRecordsData, ClientUsageSummaryData,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientRangeQuery {
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientRecordsQuery {
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    current_page: Option<u32>,
    page_size: Option<u16>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientDiagnosticsQuery {
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    dimension: String,
}

pub(super) fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/client/overview", get(overview::<S>))
        .route("/api/client/usage/records", get(records::<S>))
        .route("/api/client/usage/records/summary", get(summary::<S>))
        .route("/api/client/usage/insights/overview", get(insights::<S>))
        .route(
            "/api/client/usage/insights/diagnostics",
            get(diagnostics::<S>),
        )
        .route("/api/client/operations/errors", get(errors::<S>))
}

async fn overview<S>(
    State(state): State<S>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let snapshot = state
        .admin_services()
        .client_usage()
        .overview(session_id(&headers).as_deref(), china_today(Utc::now())?)
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(AdminError::session_required)?;
    ok(ClientOverviewData::from(&snapshot))
}

async fn records<S>(
    State(state): State<S>,
    headers: axum::http::HeaderMap,
    AdminQuery(query): AdminQuery<ClientRecordsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let page = state
        .admin_services()
        .client_usage()
        .records(
            session_id(&headers).as_deref(),
            UsageQuery {
                range: range(query.start_time, query.end_time)?,
                filter: UsageFilter {
                    model: normalize_model(query.model)?,
                    ..UsageFilter::default()
                },
                current_page: current_page(query.current_page)?,
                page_size: page_size(query.page_size)?,
            },
        )
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(AdminError::session_required)?;
    ok(ClientUsageRecordsData::from(page))
}

async fn summary<S>(
    State(state): State<S>,
    headers: axum::http::HeaderMap,
    AdminQuery(query): AdminQuery<ClientRangeQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .client_usage()
        .summary(
            session_id(&headers).as_deref(),
            range(query.start_time, query.end_time)?,
        )
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(AdminError::session_required)?;
    ok(ClientUsageSummaryData::from(result))
}

async fn insights<S>(
    State(state): State<S>,
    headers: axum::http::HeaderMap,
    AdminQuery(query): AdminQuery<ClientRangeQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let result = state
        .admin_services()
        .client_usage()
        .insights(
            session_id(&headers).as_deref(),
            range(query.start_time, query.end_time)?,
        )
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(AdminError::session_required)?;
    ok(ClientUsageInsightsData::from(result))
}

async fn diagnostics<S>(
    State(state): State<S>,
    headers: axum::http::HeaderMap,
    AdminQuery(query): AdminQuery<ClientDiagnosticsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let dimension = match query.dimension.as_str() {
        "model" => DiagnosticDimension::Model,
        "transport" => DiagnosticDimension::Transport,
        "failureClass" => DiagnosticDimension::Failure,
        _ => return Err(AdminError::bad_request("诊断维度无效")),
    };
    let result = state
        .admin_services()
        .client_usage()
        .diagnostics(
            session_id(&headers).as_deref(),
            range(query.start_time, query.end_time)?,
            dimension,
        )
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(AdminError::session_required)?;
    ok(ClientDiagnosticsData::from(result))
}

async fn errors<S>(
    State(state): State<S>,
    headers: axum::http::HeaderMap,
    AdminQuery(query): AdminQuery<ClientRecordsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let page = state
        .admin_services()
        .client_usage()
        .errors(
            session_id(&headers).as_deref(),
            OpsErrorQuery {
                range: range(query.start_time, query.end_time)?,
                filter: OpsErrorFilter {
                    model: normalize_model(query.model)?,
                    ..OpsErrorFilter::default()
                },
                current_page: current_page(query.current_page)?,
                page_size: page_size(query.page_size)?,
            },
        )
        .await
        .map_err(map_admin_service_error)?
        .ok_or_else(AdminError::session_required)?;
    ok(ClientOpsErrorsData::from(page))
}

fn ok<T: serde::Serialize>(data: T) -> Result<impl IntoResponse, AdminError> {
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

fn session_id(headers: &axum::http::HeaderMap) -> Option<String> {
    crate::session_cookie::value(headers)
}

fn range(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<TimeRange, AdminError> {
    TimeRange::new(start, end).map_err(|_| AdminError::invalid_time_range())
}

fn current_page(value: Option<u32>) -> Result<u32, AdminError> {
    match value.unwrap_or(1) {
        0 => Err(AdminError::bad_request("页码无效")),
        value => Ok(value),
    }
}

fn page_size(value: Option<u16>) -> Result<PageSize, AdminError> {
    PageSize::new(value.unwrap_or(10)).map_err(|_| AdminError::bad_request("每页数量无效"))
}

fn normalize_model(model: Option<String>) -> Result<Option<String>, AdminError> {
    let Some(model) = model else {
        return Ok(None);
    };
    let model = model.trim();
    if model.is_empty() {
        return Ok(None);
    }
    if model.len() > 256 || model.chars().any(char::is_control) {
        return Err(AdminError::bad_request("模型筛选条件无效"));
    }
    Ok(Some(model.to_owned()))
}

fn china_today(now: DateTime<Utc>) -> Result<TimeRange, AdminError> {
    let offset = FixedOffset::east_opt(8 * 60 * 60).ok_or_else(AdminError::internal)?;
    let local = now.with_timezone(&offset);
    let start = offset
        .from_local_datetime(
            &local
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .ok_or_else(AdminError::internal)?,
        )
        .single()
        .ok_or_else(AdminError::internal)?
        .with_timezone(&Utc);
    TimeRange::new(start, now).map_err(|_| AdminError::invalid_time_range())
}
