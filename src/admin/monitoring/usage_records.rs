//! 使用记录 HTTP 处理器。

use std::collections::{BTreeMap, HashMap};

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
            UsageRecordInsights, UsageRecordSummary, UsageRecordTrendPoint,
        },
    },
    admin::{
        auth::session::require_admin_auth,
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
    total_requests: u64,
    error_requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    total_tokens: u64,
    average_latency_ms: Option<f64>,
    average_latency_ms_display: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageRecordInsightsData {
    models: Vec<UsageRecordBreakdownData>,
    upstream_models: Vec<UsageRecordBreakdownData>,
    model_mappings: Vec<UsageRecordBreakdownData>,
    endpoints: Vec<UsageRecordBreakdownData>,
    upstream_endpoints: Vec<UsageRecordBreakdownData>,
    endpoint_paths: Vec<UsageRecordBreakdownData>,
    types: Vec<UsageRecordBreakdownData>,
    trend: Vec<UsageRecordTrendPointData>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageRecordBreakdownData {
    name: String,
    request_count: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    total_tokens: u64,
    cost: f64,
    actual_cost: f64,
    account_cost: f64,
    cost_display: String,
    actual_cost_display: String,
    account_cost_display: String,
    average_latency_ms: Option<f64>,
    average_latency_ms_display: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageRecordTrendPointData {
    date: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cached_tokens: u64,
    total_tokens: u64,
    cost: f64,
    actual_cost: f64,
    cost_display: String,
    actual_cost_display: String,
    average_latency_ms: Option<f64>,
    average_latency_ms_display: String,
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
                let model_aliases = state.services.settings.current().model_aliases.clone();
                let page = NumberedPage {
                    items: usage_record_items(page.items, &account_emails, &model_aliases),
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
            let model_aliases = state.services.settings.current().model_aliases.clone();
            let page = Page {
                items: usage_record_items(page.items, &account_emails, &model_aliases),
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

/// `GET /api/admin/usage/records/insights`
pub(crate) async fn usage_records_insights(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UsageRecordsQuery>,
) -> Result<impl IntoResponse, AdminError> {
    require_admin_auth(&state, &headers).await?;
    let filter = filter_from_query(query)?;
    match state.services.usage_records.insights(filter).await {
        Ok(insights) => Ok(AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(UsageRecordInsightsData::from(insights)),
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
            let model_aliases = state.services.settings.current().model_aliases.clone();
            Ok(AdminResponse::new(
                StatusCode::OK,
                AdminEnvelope::ok(usage_record_data(log, &account_emails, &model_aliases)),
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
    model_aliases: &BTreeMap<String, String>,
) -> Vec<UsageRecordData> {
    items
        .into_iter()
        .map(|record| usage_record_data(record, account_emails, model_aliases))
        .collect()
}

fn usage_record_data(
    record: UsageRecord,
    account_emails: &HashMap<String, String>,
    model_aliases: &BTreeMap<String, String>,
) -> UsageRecordData {
    let account_email = record
        .account_id
        .as_deref()
        .and_then(|account_id| account_emails.get(account_id))
        .cloned();
    let (requested_model, upstream_model) = usage_record_models(&record, model_aliases);
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

fn usage_record_models(
    record: &UsageRecord,
    model_aliases: &BTreeMap<String, String>,
) -> (Option<String>, Option<String>) {
    let stored_model = record
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty());
    let alias_target = stored_model
        .and_then(|model| model_aliases.get(model))
        .map(String::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty());

    let requested_model = metadata_string(&record.metadata, &["requestedModel"])
        .or_else(|| alias_target.and_then(|_| stored_model.map(ToString::to_string)));
    let upstream_model = metadata_string(&record.metadata, &["upstreamModel"])
        .or_else(|| alias_target.map(ToString::to_string))
        .or_else(|| {
            requested_model
                .as_ref()
                .and_then(|_| stored_model.map(ToString::to_string))
        });

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
        input_cost_display: format_cost(breakdown.input_cost),
        output_cost_display: format_cost(breakdown.output_cost),
        cache_read_cost_display: format_cost(breakdown.cache_read_cost),
        total_cost_display: format_cost(breakdown.total_cost),
        billed_cost_display: format_cost(breakdown.total_cost),
        original_cost_display: format_cost(original_cost),
        input_price_display: format_token_price(breakdown.input_price_per_mtoken),
        output_price_display: format_token_price(breakdown.output_price_per_mtoken),
        cache_read_price_display: format_token_price(breakdown.cache_read_price_per_mtoken),
        multiplier_display: format!("{:.2}x", breakdown.tier_multiplier),
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

fn format_duration_ms(value: Option<i64>) -> String {
    let Some(value) = value.filter(|value| *value >= 0) else {
        return "—".to_string();
    };

    if value < 1_000 {
        return format!("{value} ms");
    }

    if value < 60_000 {
        let seconds = value as f64 / 1_000.0;
        return if seconds >= 10.0 {
            format!("{seconds:.1} s")
        } else {
            format!("{seconds:.2} s")
        };
    }

    format!("{:.1} min", value as f64 / 60_000.0)
}

fn format_duration_ms_f64(value: Option<f64>) -> String {
    let Some(value) = value.filter(|value| value.is_finite() && *value >= 0.0) else {
        return "—".to_string();
    };
    format_duration_ms(Some(value.round() as i64))
}

fn format_number(value: u64) -> String {
    let text = value.to_string();
    let mut output = String::with_capacity(text.len() + text.len() / 3);
    for (index, ch) in text.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            output.push(',');
        }
        output.push(ch);
    }
    output.chars().rev().collect()
}

fn format_compact_number(value: u64) -> String {
    if value < 1_000 {
        return format_number(value);
    }

    if value < 1_000_000 {
        let value = value as f64 / 1_000.0;
        return if value >= 100.0 {
            format!("{value:.0}K")
        } else {
            format!("{value:.1}K")
        };
    }

    let value = value as f64 / 1_000_000.0;
    if value >= 100.0 {
        format!("{value:.0}M")
    } else {
        format!("{value:.1}M")
    }
}

fn format_cost(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_string();
    }
    format!("${value:.6}")
}

fn format_token_price(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_string();
    }
    format!("${value:.4} / 1M Token")
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
            total_requests: summary.total_requests,
            error_requests: summary.error_requests,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            cached_tokens: summary.cached_tokens,
            total_tokens: summary.total_tokens,
            average_latency_ms: summary.average_latency_ms,
            average_latency_ms_display: format_duration_ms_f64(summary.average_latency_ms),
        }
    }
}

impl From<UsageRecordInsights> for UsageRecordInsightsData {
    fn from(insights: UsageRecordInsights) -> Self {
        Self {
            models: insights
                .models
                .into_iter()
                .map(UsageRecordBreakdownData::from)
                .collect(),
            upstream_models: insights
                .upstream_models
                .into_iter()
                .map(UsageRecordBreakdownData::from)
                .collect(),
            model_mappings: insights
                .model_mappings
                .into_iter()
                .map(UsageRecordBreakdownData::from)
                .collect(),
            endpoints: insights
                .endpoints
                .into_iter()
                .map(UsageRecordBreakdownData::from)
                .collect(),
            upstream_endpoints: insights
                .upstream_endpoints
                .into_iter()
                .map(UsageRecordBreakdownData::from)
                .collect(),
            endpoint_paths: insights
                .endpoint_paths
                .into_iter()
                .map(UsageRecordBreakdownData::from)
                .collect(),
            types: insights
                .types
                .into_iter()
                .map(UsageRecordBreakdownData::from)
                .collect(),
            trend: insights
                .trend
                .into_iter()
                .map(UsageRecordTrendPointData::from)
                .collect(),
        }
    }
}

impl From<UsageRecordBreakdown> for UsageRecordBreakdownData {
    fn from(item: UsageRecordBreakdown) -> Self {
        Self {
            name: item.name,
            request_count: item.request_count,
            input_tokens: item.input_tokens,
            output_tokens: item.output_tokens,
            cached_tokens: item.cached_tokens,
            total_tokens: item.total_tokens,
            cost: item.cost,
            actual_cost: item.actual_cost,
            account_cost: item.account_cost,
            cost_display: format_cost(item.cost),
            actual_cost_display: format_cost(item.actual_cost),
            account_cost_display: format_cost(item.account_cost),
            average_latency_ms: item.average_latency_ms,
            average_latency_ms_display: format_duration_ms_f64(item.average_latency_ms),
        }
    }
}

impl From<UsageRecordTrendPoint> for UsageRecordTrendPointData {
    fn from(point: UsageRecordTrendPoint) -> Self {
        Self {
            date: point.date,
            input_tokens: point.input_tokens,
            output_tokens: point.output_tokens,
            cache_creation_tokens: point.cache_creation_tokens,
            cached_tokens: point.cached_tokens,
            total_tokens: point.total_tokens,
            cost: point.cost,
            actual_cost: point.actual_cost,
            cost_display: format_cost(point.cost),
            actual_cost_display: format_cost(point.actual_cost),
            average_latency_ms: point.average_latency_ms,
            average_latency_ms_display: format_duration_ms_f64(point.average_latency_ms),
        }
    }
}
