//! 使用记录 HTTP 处理器。

use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::{
    admin::monitoring::{
        billing,
        usage_record::{UsageRecord, UsageRecordLevel},
        usage_record_store::{
            AdminUsageRecordError, AdminUsageRecordFilter, UsageRecordBreakdown,
            UsageRecordEndpointSource, UsageRecordModelSource, UsageRecordSummary,
            UsageRecordTrendPoint,
        },
    },
    admin::{
        auth::session::require_admin_auth,
        response::{AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse},
    },
    infra::{
        format::{
            format_compact_number, format_duration_ms, format_duration_ms_f64, format_multiplier,
            format_number, format_percent, format_precise_cost, format_rate, format_token_price,
            format_tokens,
        },
        json::{clamp_limit, clamp_page, NumberedPage, Page},
        time::china_datetime,
    },
    runtime::state::AppState,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageRecordsQuery {
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
    token_details: UsageRecordTokenDetailsData,
    cost_details: Option<UsageRecordCostDetailsData>,
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
pub(crate) struct UsageRecordCostDetailsData {
    input_cost: f64,
    output_cost: f64,
    cache_read_cost: f64,
    total_cost: f64,
    billed_cost: f64,
    original_cost: f64,
    input_price_per_mtoken: f64,
    output_price_per_mtoken: f64,
    cache_read_price_per_mtoken: f64,
    service_tier: Option<String>,
    service_tier_display: String,
    multiplier: f64,
    input_cost_display: String,
    output_cost_display: String,
    cache_read_cost_display: String,
    total_cost_display: String,
    billed_cost_display: String,
    original_cost_display: String,
    input_price_display: String,
    output_price_display: String,
    cache_read_price_display: String,
    multiplier_display: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageRecordSummaryData {
    total_requests: String,
    error_requests: String,
    error_rate: String,
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
    cost: String,
    actual_cost: String,
    account_cost: String,
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
    cache_creation_tokens: String,
    cache_creation_tokens_value: u64,
    cached_tokens: String,
    cached_tokens_value: u64,
    total_tokens: String,
    total_tokens_value: u64,
    cache_hit_rate: String,
    cache_hit_rate_value: f64,
    cost: String,
    actual_cost: String,
    average_latency_ms: String,
    average_latency_ms_value: Option<f64>,
}

/// `GET /api/admin/usage/records`
pub(crate) async fn usage_records(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UsageRecordsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let limit = clamp_limit(query.page_size.or(query.limit).unwrap_or(50));
    let page = query.page;
    let use_numbered_page = page.is_some() || query.page_size.is_some();
    let cursor = query.cursor.clone();
    let filter = filter_from_query(query)?;

    if use_numbered_page {
        return match state
            .services
            .usage_records
            .list_page(clamp_page(page.unwrap_or(1)), limit, filter)
            .await
        {
            Ok(page) => {
                let account_emails = account_email_map(&state, &page.items).await?;
                let page = NumberedPage {
                    items: usage_record_items(page.items, &account_emails),
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

    match state
        .services
        .usage_records
        .list(cursor, limit, filter)
        .await
    {
        Ok(page) => {
            let account_emails = account_email_map(&state, &page.items).await?;
            let page = Page {
                items: usage_record_items(page.items, &account_emails),
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

/// `GET /api/admin/usage/records/summary`
pub(crate) async fn usage_records_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UsageRecordsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
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
    headers: HeaderMap,
    Query(query): Query<UsageRecordDistributionQuery>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
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
    headers: HeaderMap,
    Query(query): Query<UsageRecordDistributionQuery>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let source = endpoint_source_from_query(query.source)?;
    let filter = filter_from_query(query.records)?;
    match state
        .services
        .usage_records
        .endpoint_distribution(filter, source)
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
    headers: HeaderMap,
    Query(query): Query<UsageRecordsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let filter = filter_from_query(query)?;
    match state.services.usage_records.token_trend(filter).await {
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

/// `GET /api/admin/usage/records/insights/latency-trend`
pub(crate) async fn usage_records_latency_trend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UsageRecordsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let filter = filter_from_query(query)?;
    match state.services.usage_records.latency_trend(filter).await {
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
    headers: HeaderMap,
    Query(query): Query<UsageRecordDetailQuery>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    match state.services.usage_records.get(&query.id).await {
        Ok(Some(log)) => {
            let account_emails = account_email_map(&state, std::slice::from_ref(&log)).await?;
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(usage_record_data(log, &account_emails)),
            ))
        }
        Ok(None) => Err(AdminError::new(
            StatusCode::NOT_FOUND,
            40401,
            "Usage record not found",
        )),
        Err(error) => Err(log_error(&error)),
    }
}

/// `POST /api/admin/usage/records/delete`
pub(crate) async fn clear_usage_records(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
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

fn filter_from_query(query: UsageRecordsQuery) -> Result<AdminUsageRecordFilter, AdminError> {
    Ok(AdminUsageRecordFilter {
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
        start_time: optional_datetime(query.start_time)?,
        end_time: optional_datetime(query.end_time)?,
    })
}

fn model_source_from_query(value: Option<String>) -> Result<UsageRecordModelSource, AdminError> {
    match non_empty(value).as_deref().unwrap_or("requested") {
        "requested" => Ok(UsageRecordModelSource::Requested),
        "upstream" => Ok(UsageRecordModelSource::Upstream),
        "mapping" => Ok(UsageRecordModelSource::Mapping),
        _ => Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40003,
            "Invalid model distribution source",
        )),
    }
}

fn endpoint_source_from_query(
    value: Option<String>,
) -> Result<UsageRecordEndpointSource, AdminError> {
    match non_empty(value).as_deref().unwrap_or("inbound") {
        "inbound" => Ok(UsageRecordEndpointSource::Inbound),
        "upstream" => Ok(UsageRecordEndpointSource::Upstream),
        "path" => Ok(UsageRecordEndpointSource::Path),
        _ => Err(AdminError::new(
            StatusCode::BAD_REQUEST,
            40004,
            "Invalid endpoint distribution source",
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
        .map_err(|_| AdminError::new(StatusCode::BAD_REQUEST, 40002, "Invalid time range"))
}

fn log_error(error: &AdminUsageRecordError) -> AdminError {
    match error {
        AdminUsageRecordError::List
        | AdminUsageRecordError::Get
        | AdminUsageRecordError::Clear
        | AdminUsageRecordError::Append
        | AdminUsageRecordError::Trim => {
            AdminError::new(StatusCode::INTERNAL_SERVER_ERROR, 50001, error.to_string())
        }
    }
}

fn level_from_query(value: Option<String>) -> Result<Option<UsageRecordLevel>, String> {
    let Some(value) = non_empty(value) else {
        return Ok(None);
    };
    match value.as_str() {
        "debug" => Ok(Some(UsageRecordLevel::Debug)),
        "info" => Ok(Some(UsageRecordLevel::Info)),
        "warn" => Ok(Some(UsageRecordLevel::Warn)),
        "error" => Ok(Some(UsageRecordLevel::Error)),
        other => Err(format!("Unsupported log level: {other}")),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) async fn account_email_map(
    state: &AppState,
    items: &[UsageRecord],
) -> Result<HashMap<String, String>, AdminError> {
    let mut account_ids = items
        .iter()
        .filter_map(|item| item.account_id.as_deref())
        .map(str::to_string)
        .collect::<Vec<_>>();
    account_ids.sort_unstable();
    account_ids.dedup();

    if account_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut builder = QueryBuilder::<Sqlite>::new("select id, email from accounts where id in (");
    let mut separated = builder.separated(", ");
    for account_id in &account_ids {
        separated.push_bind(account_id);
    }
    separated.push_unseparated(")");

    let rows = builder
        .build()
        .fetch_all(state.services.background_tasks.accounts.pool())
        .await
        .map_err(|error| {
            AdminError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                50002,
                format!("Failed to load usage record accounts: {error}"),
            )
        })?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            row.get::<Option<String>, _>("email")
                .map(|email| email.trim().to_string())
                .filter(|email| !email.is_empty())
                .map(|email| (row.get::<String, _>("id"), email))
        })
        .collect())
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
    let account_email = record
        .account_id
        .as_deref()
        .and_then(|account_id| account_emails.get(account_id))
        .cloned()
        .or_else(|| metadata_string(&record.metadata, &["accountEmail", "account_email"]));
    let (requested_model, upstream_model) = usage_record_models(&record);
    let client_ip = metadata_string(&record.metadata, &["clientIp", "ipAddress", "ip_address"]);
    let user_agent = metadata_string(&record.metadata, &["userAgent", "user_agent"]);
    let reasoning_effort =
        metadata_string(&record.metadata, &["reasoningEffort", "reasoning_effort"]);
    let token_details = usage_token_details(&record.metadata);
    let cost_details = usage_cost_details(&record, upstream_model.as_deref(), &token_details);
    let first_token_latency_ms = metadata_i64(
        &record.metadata,
        &[
            "firstTokenMs",
            "first_token_ms",
            "firstTokenLatencyMs",
            "first_token_latency_ms",
        ],
    );
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
        token_details,
        cost_details,
        first_token_latency_ms,
        first_token_latency_ms_display,
        latency_ms_display,
        record,
    }
}

fn usage_record_models(record: &UsageRecord) -> (Option<String>, Option<String>) {
    let stored_model = record
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty());
    let requested_model = metadata_string(&record.metadata, &["requestedModel"])
        .or_else(|| stored_model.map(ToString::to_string));
    let upstream_model = metadata_string(&record.metadata, &["upstreamModel"])
        .or_else(|| requested_model.clone())
        .or_else(|| stored_model.map(ToString::to_string));

    (requested_model, upstream_model)
}

fn usage_token_details(metadata: &Value) -> UsageRecordTokenDetailsData {
    let input_tokens = metadata_usage_u64(metadata, "inputTokens");
    let output_tokens = metadata_usage_u64(metadata, "outputTokens");
    let cached_tokens = metadata_usage_u64(metadata, "cachedTokens");
    let reasoning_tokens = metadata_usage_u64(metadata, "reasoningTokens");
    let total_tokens =
        metadata_usage_u64(metadata, "totalTokens").max(input_tokens.saturating_add(output_tokens));

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

fn usage_cost_details(
    record: &UsageRecord,
    upstream_model: Option<&str>,
    tokens: &UsageRecordTokenDetailsData,
) -> Option<UsageRecordCostDetailsData> {
    if tokens.input_tokens == 0 && tokens.output_tokens == 0 && tokens.cached_tokens == 0 {
        return None;
    }

    let model = upstream_model
        .or(record.model.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let service_tier = usage_service_tier(&record.metadata);
    let breakdown = billing::calculate_cost_breakdown(
        tokens.input_tokens,
        tokens.output_tokens,
        tokens.cached_tokens,
        model,
        service_tier.as_deref(),
    );
    let original_cost = breakdown.input_cost + breakdown.output_cost + breakdown.cache_read_cost;

    Some(UsageRecordCostDetailsData {
        input_cost: breakdown.input_cost,
        output_cost: breakdown.output_cost,
        cache_read_cost: breakdown.cache_read_cost,
        total_cost: breakdown.total_cost,
        billed_cost: breakdown.total_cost,
        original_cost,
        input_price_per_mtoken: breakdown.input_price_per_mtoken,
        output_price_per_mtoken: breakdown.output_price_per_mtoken,
        cache_read_price_per_mtoken: breakdown.cache_read_price_per_mtoken,
        service_tier: breakdown.service_tier.clone(),
        service_tier_display: breakdown
            .service_tier
            .as_deref()
            .map(format_service_tier)
            .unwrap_or_else(|| "Default".to_string()),
        multiplier: breakdown.tier_multiplier,
        input_cost_display: format_precise_cost(breakdown.input_cost),
        output_cost_display: format_precise_cost(breakdown.output_cost),
        cache_read_cost_display: format_precise_cost(breakdown.cache_read_cost),
        total_cost_display: format_precise_cost(breakdown.total_cost),
        billed_cost_display: format_precise_cost(breakdown.total_cost),
        original_cost_display: format_precise_cost(original_cost),
        input_price_display: format_token_price(breakdown.input_price_per_mtoken),
        output_price_display: format_token_price(breakdown.output_price_per_mtoken),
        cache_read_price_display: format_token_price(breakdown.cache_read_price_per_mtoken),
        multiplier_display: format_multiplier(breakdown.tier_multiplier),
    })
}

fn usage_service_tier(metadata: &Value) -> Option<String> {
    metadata_string(
        metadata,
        &[
            "billingServiceTier",
            "billing_service_tier",
            "serviceTier",
            "service_tier",
        ],
    )
}

fn metadata_usage_u64(metadata: &Value, key: &str) -> u64 {
    metadata
        .get("usage")
        .and_then(|usage| metadata_u64_at(usage, key))
        .or_else(|| metadata_u64_at(metadata, key))
        .unwrap_or_default()
}

fn metadata_u64_at(value: &Value, key: &str) -> Option<u64> {
    value
        .get(key)
        .and_then(|value| value.as_u64().or_else(|| value.as_i64()?.try_into().ok()))
}

fn metadata_string(metadata: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        metadata
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn metadata_i64(metadata: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| metadata.get(*key).and_then(Value::as_i64))
        .filter(|value| *value >= 0)
}

fn format_service_tier(value: &str) -> String {
    match value {
        "priority" | "fast" => "Fast".to_string(),
        "flex" => "Flex".to_string(),
        "default" => "Default".to_string(),
        other => other.to_string(),
    }
}

impl From<UsageRecordSummary> for UsageRecordSummaryData {
    fn from(summary: UsageRecordSummary) -> Self {
        Self {
            total_requests: format_compact_number(summary.total_requests),
            error_requests: format_compact_number(summary.error_requests),
            error_rate: if summary.total_requests > 0 {
                format_rate(Some(
                    summary.error_requests as f64 / summary.total_requests as f64,
                ))
            } else {
                "—".to_string()
            },
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
        UsageRecordBreakdownData::from_item(item, 0)
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
            cost: format_precise_cost(item.cost),
            actual_cost: format_precise_cost(item.actual_cost),
            account_cost: format_precise_cost(item.account_cost),
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
        let prompt_tokens = point
            .input_tokens
            .saturating_add(point.cache_creation_tokens)
            .saturating_add(point.cached_tokens);
        let cache_hit_rate_value = if prompt_tokens > 0 {
            (point.cached_tokens as f64 / prompt_tokens as f64 * 100.0).round()
        } else {
            0.0
        };
        Self {
            date: point.date,
            input_tokens: format_tokens(point.input_tokens),
            input_tokens_value: point.input_tokens,
            output_tokens: format_tokens(point.output_tokens),
            output_tokens_value: point.output_tokens,
            cache_creation_tokens: format_tokens(point.cache_creation_tokens),
            cache_creation_tokens_value: point.cache_creation_tokens,
            cached_tokens: format_tokens(point.cached_tokens),
            cached_tokens_value: point.cached_tokens,
            total_tokens: format_tokens(point.total_tokens),
            total_tokens_value: point.total_tokens,
            cache_hit_rate: format_percent(cache_hit_rate_value),
            cache_hit_rate_value,
            cost: format_precise_cost(point.cost),
            actual_cost: format_precise_cost(point.actual_cost),
            average_latency_ms: format_duration_ms_f64(point.average_latency_ms),
            average_latency_ms_value: point.average_latency_ms,
        }
    }
}
