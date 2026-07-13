//! 使用统计与请求记录 HTTP 处理器。

use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    api::AppState,
    api::admin::{
        response::{AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse},
        session::AdminAuth,
    },
    infra::{
        format::{
            format_billing_amount, format_compact_number, format_duration_ms,
            format_duration_ms_f64, format_multiplier, format_number, format_percent,
            format_token_price, format_tokens,
        },
        json::{NumberedPage, clamp_limit, clamp_page},
        time::china_datetime,
    },
    telemetry::{
        usage::insights::{UsageDiagnosticsDimension, default_time_range},
        usage::query::{
            UsageQueryError, UsageQueryFilter, UsageRecordBreakdown, UsageRecordModelSource,
            UsageRecordSummary, UsageRecordTrendPoint, usage_record_billing,
        },
        usage::types::{UsageRecord, metadata_string},
    },
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageRecordsQuery {
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
    transport: Option<String>,
    attempt_index: Option<i64>,
    response_id: Option<String>,
    upstream_request_id: Option<String>,
    search: Option<String>,
    start_time: Option<String>,
    end_time: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageRecordDistributionQuery {
    #[serde(flatten)]
    records: UsageRecordsQuery,
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageInsightsQuery {
    start_time: Option<String>,
    end_time: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageDiagnosticsQuery {
    #[serde(flatten)]
    range: UsageInsightsQuery,
    dimension: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageRecordDetailQuery {
    id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClearUsageRecordsData {
    cleared: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageRecordData {
    #[serde(flatten)]
    record: UsageRecord,
    created_at_display: String,
    account_email: Option<String>,
    requested_model: Option<String>,
    upstream_model: Option<String>,
    client_ip: Option<String>,
    user_agent: Option<String>,
    reasoning_effort: Option<String>,
    reasoning_preset: Option<String>,
    compact: bool,
    request_kind: Option<String>,
    subagent_kind: Option<String>,
    token_details: UsageRecordTokenDetailsData,
    billing: Option<UsageRecordBillingData>,
    first_token_latency_ms: Option<i64>,
    first_token_latency_ms_display: String,
    latency_ms_display: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageRecordTokenDetailsData {
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    input_tokens_display: String,
    output_tokens_display: String,
    cached_tokens_display: String,
    reasoning_tokens_display: String,
    total_tokens_display: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageRecordBillingData {
    input_amount: f64,
    output_amount: f64,
    cache_read_amount: f64,
    standard_amount: f64,
    total_amount: f64,
    input_price_per_mtoken: f64,
    output_price_per_mtoken: f64,
    cache_read_price_per_mtoken: f64,
    service_tier: Option<String>,
    service_tier_display: String,
    multiplier: f64,
    input_amount_display: String,
    output_amount_display: String,
    cache_read_amount_display: String,
    standard_amount_display: String,
    total_amount_display: String,
    input_price_display: String,
    output_price_display: String,
    cache_read_price_display: String,
    multiplier_display: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageRecordSummaryData {
    total_requests: String,
    input_tokens: String,
    output_tokens: String,
    cached_tokens: String,
    total_tokens: String,
    average_latency_ms: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageRecordBreakdownData {
    name: String,
    request_count: String,
    request_count_value: u64,
    input_tokens: String,
    output_tokens: String,
    cached_tokens: String,
    total_tokens: String,
    total_tokens_value: u64,
    total_tokens_total: String,
    total_tokens_total_value: u64,
    standard_billing_amount: String,
    actual_billing_amount: String,
    account_billing_amount: String,
    average_latency_ms: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageRecordTrendPointData {
    date: String,
    input_tokens: String,
    input_tokens_value: u64,
    output_tokens: String,
    output_tokens_value: u64,
    cached_tokens: String,
    cached_tokens_value: u64,
    total_tokens: String,
    total_tokens_value: u64,
    cache_hit_rate: String,
    cache_hit_rate_value: f64,
    standard_billing_amount: String,
    actual_billing_amount: String,
    average_latency_ms: String,
    average_latency_ms_value: Option<f64>,
}

/// `GET /api/admin/usage/records`
pub(crate) async fn usage_records(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<UsageRecordsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let page = clamp_page(query.page.unwrap_or(1));
    let page_size = clamp_limit(query.page_size.unwrap_or(50));
    let filter = filter_from_query(query)?;

    match state
        .services
        .usage_records
        .list_page(page, page_size, filter)
        .await
    {
        Ok(page) => {
            let account_emails = state
                .services
                .usage_records
                .account_email_map(&page.items)
                .await
                .map_err(|error| log_error(&error))?;
            let page = NumberedPage {
                items: usage_record_items(page.items, &account_emails),
                total: page.total,
                page: page.page,
                page_size: page.page_size,
            };
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminPageEnvelope::ok(page),
            ))
        }
        Err(error) => Err(log_error(&error)),
    }
}

/// `GET /api/admin/usage/records/summary`
pub(crate) async fn usage_records_summary(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<UsageRecordsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let filter = filter_from_query(query)?;
    match state.services.usage_records.summary(filter).await {
        Ok(summary) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(UsageRecordSummaryData::from(summary)),
        )),
        Err(error) => Err(log_error(&error)),
    }
}

/// `GET /api/admin/usage/records/insights/models`
pub(crate) async fn usage_records_model_distribution(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<UsageRecordDistributionQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let source = model_source_from_query(query.source)?;
    let filter = filter_from_query(query.records)?;
    match state
        .services
        .usage_records
        .model_distribution(filter, source)
        .await
    {
        Ok(items) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(usage_record_breakdown_items(items)),
        )),
        Err(error) => Err(log_error(&error)),
    }
}

/// `GET /api/admin/usage/records/insights/endpoints`
pub(crate) async fn usage_records_endpoint_distribution(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<UsageRecordsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let filter = filter_from_query(query)?;
    match state
        .services
        .usage_records
        .endpoint_distribution(filter)
        .await
    {
        Ok(items) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(usage_record_breakdown_items(items)),
        )),
        Err(error) => Err(log_error(&error)),
    }
}

/// `GET /api/admin/usage/records/insights/token-trend`
pub(crate) async fn usage_records_token_trend(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<UsageRecordsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    usage_records_trend_response(state, query).await
}

/// `GET /api/admin/usage/records/insights/latency-trend`
pub(crate) async fn usage_records_latency_trend(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<UsageRecordsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    usage_records_trend_response(state, query).await
}

/// `GET /api/admin/usage/records/insights/overview`
pub(crate) async fn usage_records_insights_overview(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<UsageInsightsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let (start, end) = insights_time_range(query)?;
    state
        .services
        .usage_records
        .insights_overview(start, end)
        .await
        .map(|insights| AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(insights)))
        .map_err(|error| log_error(&error))
}

/// `GET /api/admin/usage/records/insights/diagnostics`
pub(crate) async fn usage_records_insights_diagnostics(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<UsageDiagnosticsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    let dimension = diagnostics_dimension(query.dimension)?;
    let (start, end) = insights_time_range(query.range)?;
    state
        .services
        .usage_records
        .insights_diagnostics(start, end, dimension)
        .await
        .map(|insights| AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(insights)))
        .map_err(|error| log_error(&error))
}

async fn usage_records_trend_response(
    state: AppState,
    query: UsageRecordsQuery,
) -> Result<AdminResponse<AdminEnvelope<Vec<UsageRecordTrendPointData>>>, AdminError> {
    let filter = filter_from_query(query)?;
    match state.services.usage_records.trend(filter).await {
        Ok(points) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(
                points
                    .into_iter()
                    .map(UsageRecordTrendPointData::from)
                    .collect::<Vec<_>>(),
            ),
        )),
        Err(error) => Err(log_error(&error)),
    }
}

/// `GET /api/admin/usage/records/detail`
pub(crate) async fn usage_record_detail(
    State(state): State<AppState>,
    _auth: AdminAuth,
    Query(query): Query<UsageRecordDetailQuery>,
) -> Result<impl IntoResponse, AdminError> {
    match state.services.usage_records.get(&query.id).await {
        Ok(Some(log)) => {
            let account_emails = state
                .services
                .usage_records
                .account_email_map(std::slice::from_ref(&log))
                .await
                .map_err(|error| log_error(&error))?;
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(usage_record_data(log, &account_emails)),
            ))
        }
        Ok(None) => Err(AdminError::not_found("Usage record not found")),
        Err(error) => Err(log_error(&error)),
    }
}

/// `POST /api/admin/usage/records/delete`
pub(crate) async fn clear_usage_records(
    State(state): State<AppState>,
    _auth: AdminAuth,
) -> Result<impl IntoResponse, AdminError> {
    match state.services.usage_records.clear().await {
        Ok(cleared) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(ClearUsageRecordsData {
                cleared: cleared.cleared,
            }),
        )),
        Err(error) => Err(log_error(&error)),
    }
}

fn filter_from_query(query: UsageRecordsQuery) -> Result<UsageQueryFilter, AdminError> {
    Ok(UsageQueryFilter {
        kind: non_empty(query.kind),
        client_api_key_id: non_empty(query.client_api_key_id),
        provider: non_empty(query.provider),
        request_id: non_empty(query.request_id),
        account_id: non_empty(query.account_id),
        route: non_empty(query.route),
        model: non_empty(query.model),
        status_code: query.status_code,
        transport: non_empty(query.transport),
        attempt_index: query.attempt_index,
        response_id: non_empty(query.response_id),
        upstream_request_id: non_empty(query.upstream_request_id),
        search: non_empty(query.search),
        start_time: optional_datetime(query.start_time)?,
        end_time: optional_datetime(query.end_time)?,
    })
}

fn model_source_from_query(value: Option<String>) -> Result<UsageRecordModelSource, AdminError> {
    match non_empty(value).as_deref().unwrap_or("requested") {
        "requested" => Ok(UsageRecordModelSource::Requested),
        "upstream" => Ok(UsageRecordModelSource::Upstream),
        "mapping" => Ok(UsageRecordModelSource::Mapping),
        _ => Err(AdminError::invalid_model_source(
            "Invalid model distribution source",
        )),
    }
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

fn insights_time_range(
    query: UsageInsightsQuery,
) -> Result<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>), AdminError> {
    let end = optional_datetime(query.end_time)?.unwrap_or_else(chrono::Utc::now);
    let start = optional_datetime(query.start_time)?.unwrap_or_else(|| default_time_range(end).0);
    if start >= end {
        return Err(AdminError::invalid_time_range(
            "Start time must be earlier than end time",
        ));
    }
    Ok((start, end))
}

fn diagnostics_dimension(value: Option<String>) -> Result<UsageDiagnosticsDimension, AdminError> {
    let value = non_empty(value).unwrap_or_else(|| "model".to_string());
    UsageDiagnosticsDimension::parse(&value)
        .ok_or_else(|| AdminError::bad_request("Invalid diagnostics dimension"))
}

fn log_error(error: &UsageQueryError) -> AdminError {
    match error {
        UsageQueryError::List | UsageQueryError::Get | UsageQueryError::Clear => {
            AdminError::internal(error.to_string())
        }
        UsageQueryError::Accounts => AdminError::usage_record_accounts_failed(error.to_string()),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn usage_record_items(
    items: Vec<UsageRecord>,
    account_emails: &HashMap<String, String>,
) -> Vec<UsageRecordData> {
    items
        .into_iter()
        .map(|record| usage_record_data(record, account_emails))
        .collect()
}

fn usage_record_data(
    record: UsageRecord,
    account_emails: &HashMap<String, String>,
) -> UsageRecordData {
    let account_email = account_emails
        .get(&record.account_id)
        .cloned()
        .or_else(|| metadata_string(&record.metadata, &["accountEmail", "account_email"]));
    let (requested_model, upstream_model) = usage_record_models(&record);
    let client_ip = metadata_string(&record.metadata, &["clientIp", "ipAddress", "ip_address"]);
    let user_agent = metadata_string(&record.metadata, &["userAgent", "user_agent"]);
    let reasoning_effort =
        metadata_string(&record.metadata, &["reasoningEffort", "reasoning_effort"]);
    let reasoning_preset = metadata_string(&record.metadata, &["reasoningPreset"]);
    let compact = record
        .metadata
        .get("compact")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let request_kind = metadata_string(&record.metadata, &["requestKind", "request_kind"]);
    let subagent_kind = metadata_string(&record.metadata, &["subagentKind", "subagent_kind"]);
    let token_details = usage_token_details(&record);
    let billing = usage_billing(&record, upstream_model.as_deref(), &token_details);
    let first_token_latency_ms = record.first_token_ms;
    let first_token_latency_ms_display = format_duration_ms(first_token_latency_ms);
    let latency_ms_display = format_duration_ms(record.latency_ms);

    UsageRecordData {
        created_at_display: china_datetime(&record.created_at),
        account_email,
        requested_model,
        upstream_model,
        client_ip,
        user_agent,
        reasoning_effort,
        reasoning_preset,
        compact,
        request_kind,
        subagent_kind,
        token_details,
        billing,
        first_token_latency_ms,
        first_token_latency_ms_display,
        latency_ms_display,
        record,
    }
}

pub(crate) fn usage_record_models(record: &UsageRecord) -> (Option<String>, Option<String>) {
    let stored_model = record.model.trim();
    let requested_model = record
        .requested_model
        .clone()
        .or_else(|| (!stored_model.is_empty()).then(|| stored_model.to_string()));
    let upstream_model = record
        .upstream_model
        .clone()
        .or_else(|| requested_model.clone())
        .or_else(|| (!stored_model.is_empty()).then(|| stored_model.to_string()));

    (requested_model, upstream_model)
}

pub(crate) fn usage_token_details(record: &UsageRecord) -> UsageRecordTokenDetailsData {
    let input_tokens: u64 = record
        .input_tokens
        .and_then(|value| value.try_into().ok())
        .unwrap_or(0);
    let output_tokens: u64 = record
        .output_tokens
        .and_then(|value| value.try_into().ok())
        .unwrap_or(0);
    let cached_tokens: u64 = record
        .cached_tokens
        .and_then(|value| value.try_into().ok())
        .unwrap_or(0);
    let reasoning_tokens: u64 = record
        .reasoning_tokens
        .and_then(|value| value.try_into().ok())
        .unwrap_or(0);
    let total_tokens = input_tokens.saturating_add(output_tokens);

    UsageRecordTokenDetailsData {
        input_tokens,
        output_tokens,
        cached_tokens,
        reasoning_tokens,
        total_tokens,
        input_tokens_display: format_number(input_tokens),
        output_tokens_display: format_number(output_tokens),
        cached_tokens_display: format_compact_number(cached_tokens),
        reasoning_tokens_display: format_number(reasoning_tokens),
        total_tokens_display: format_number(total_tokens),
    }
}

pub(crate) fn usage_billing(
    record: &UsageRecord,
    upstream_model: Option<&str>,
    tokens: &UsageRecordTokenDetailsData,
) -> Option<UsageRecordBillingData> {
    let billing = usage_record_billing(
        record,
        upstream_model,
        tokens.input_tokens,
        tokens.output_tokens,
        tokens.cached_tokens,
    )?;

    Some(UsageRecordBillingData {
        input_amount: billing.input_amount,
        output_amount: billing.output_amount,
        cache_read_amount: billing.cache_read_amount,
        standard_amount: billing.standard_amount,
        total_amount: billing.total_amount,
        input_price_per_mtoken: billing.input_price_per_mtoken,
        output_price_per_mtoken: billing.output_price_per_mtoken,
        cache_read_price_per_mtoken: billing.cache_read_price_per_mtoken,
        service_tier: billing.service_tier,
        service_tier_display: billing.service_tier_display,
        multiplier: billing.multiplier,
        input_amount_display: format_billing_amount(billing.input_amount),
        output_amount_display: format_billing_amount(billing.output_amount),
        cache_read_amount_display: format_billing_amount(billing.cache_read_amount),
        standard_amount_display: format_billing_amount(billing.standard_amount),
        total_amount_display: format_billing_amount(billing.total_amount),
        input_price_display: format_token_price(billing.input_price_per_mtoken),
        output_price_display: format_token_price(billing.output_price_per_mtoken),
        cache_read_price_display: format_token_price(billing.cache_read_price_per_mtoken),
        multiplier_display: format_multiplier(billing.multiplier),
    })
}

impl From<UsageRecordSummary> for UsageRecordSummaryData {
    fn from(summary: UsageRecordSummary) -> Self {
        Self {
            total_requests: format_compact_number(summary.total_requests),
            input_tokens: format_tokens(summary.input_tokens),
            output_tokens: format_tokens(summary.output_tokens),
            cached_tokens: format_tokens(summary.cached_tokens),
            total_tokens: format_tokens(summary.total_tokens),
            average_latency_ms: format_duration_ms_f64(summary.average_latency_ms),
        }
    }
}

impl From<UsageRecordBreakdown> for UsageRecordBreakdownData {
    fn from(item: UsageRecordBreakdown) -> Self {
        Self::from_item(item, 0)
    }
}

impl UsageRecordBreakdownData {
    fn from_item(item: UsageRecordBreakdown, total_tokens_total: u64) -> Self {
        Self {
            name: item.name,
            request_count: format_compact_number(item.request_count),
            request_count_value: item.request_count,
            input_tokens: format_tokens(item.input_tokens),
            output_tokens: format_tokens(item.output_tokens),
            cached_tokens: format_tokens(item.cached_tokens),
            total_tokens: format_tokens(item.total_tokens),
            total_tokens_value: item.total_tokens,
            total_tokens_total: format_tokens(total_tokens_total),
            total_tokens_total_value: total_tokens_total,
            standard_billing_amount: format_billing_amount(item.standard_billing_amount),
            actual_billing_amount: format_billing_amount(item.actual_billing_amount),
            account_billing_amount: format_billing_amount(item.account_billing_amount),
            average_latency_ms: format_duration_ms_f64(item.average_latency_ms),
        }
    }
}

fn usage_record_breakdown_items(items: Vec<UsageRecordBreakdown>) -> Vec<UsageRecordBreakdownData> {
    let total_tokens_total = items
        .iter()
        .map(|item| item.total_tokens)
        .fold(0_u64, u64::saturating_add);
    items
        .into_iter()
        .map(|item| UsageRecordBreakdownData::from_item(item, total_tokens_total))
        .collect()
}

impl From<UsageRecordTrendPoint> for UsageRecordTrendPointData {
    fn from(point: UsageRecordTrendPoint) -> Self {
        let cache_hit_rate_value = if point.input_tokens > 0 {
            (point.cached_tokens.min(point.input_tokens) as f64 / point.input_tokens as f64 * 100.0)
                .round()
        } else {
            0.0
        };
        Self {
            date: point.date,
            input_tokens: format_tokens(point.input_tokens),
            input_tokens_value: point.input_tokens,
            output_tokens: format_tokens(point.output_tokens),
            output_tokens_value: point.output_tokens,
            cached_tokens: format_tokens(point.cached_tokens),
            cached_tokens_value: point.cached_tokens,
            total_tokens: format_tokens(point.total_tokens),
            total_tokens_value: point.total_tokens,
            cache_hit_rate: format_percent(cache_hit_rate_value),
            cache_hit_rate_value,
            standard_billing_amount: format_billing_amount(point.standard_billing_amount),
            actual_billing_amount: format_billing_amount(point.actual_billing_amount),
            average_latency_ms: format_duration_ms_f64(point.average_latency_ms),
            average_latency_ms_value: point.average_latency_ms,
        }
    }
}
