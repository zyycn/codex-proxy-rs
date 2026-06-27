//! 事件日志存储实现（SQLite）。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};
use thiserror::Error;

use crate::admin::monitoring::events::{
    EventLevel, EventLog, EventLogStore, EventLogStoreError, EventLogStoreResult,
};
use crate::infra::json::{decode_cursor, encode_cursor, page_offset, NumberedPage, Page};
use crate::infra::time::china_quarter_hour_start;

/// 默认启用事件日志，便于管理端诊断请求链路。
pub const DEFAULT_EVENT_LOG_ENABLED: bool = true;
/// 默认保留最新事件日志数量。
pub const DEFAULT_EVENT_LOG_CAPACITY: u32 = 2_000;
/// 默认不保存请求/响应体，避免把用户内容写入诊断日志。
pub const DEFAULT_EVENT_LOG_CAPTURE_BODY: bool = false;

/// SQLite 事件日志错误。
#[derive(Debug, Error)]
pub enum SqliteEventLogStoreError {
    /// 数据库错误。
    #[error("sqlite event log database error: {0}")]
    Database(#[from] sqlx::Error),
    /// JSON 错误。
    #[error("sqlite event log json error: {0}")]
    Json(#[from] serde_json::Error),
    /// 时间格式错误。
    #[error("sqlite event log timestamp error: {0}")]
    Timestamp(#[from] chrono::ParseError),
    /// 事件等级非法。
    #[error("invalid event level: {0}")]
    InvalidLevel(String),
    /// 分页游标非法。
    #[error("invalid event log pagination cursor")]
    InvalidCursor,
}

/// SQLite 事件日志结果。
pub type SqliteEventLogStoreResult<T> = Result<T, SqliteEventLogStoreError>;

/// 事件日志查询过滤器。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventLogFilter {
    /// 事件类别。
    pub kind: Option<String>,
    /// 事件等级。
    pub level: Option<EventLevel>,
    /// 请求 ID。
    pub request_id: Option<String>,
    /// 账号 ID。
    pub account_id: Option<String>,
    /// 路由。
    pub route: Option<String>,
    /// 模型。
    pub model: Option<String>,
    /// HTTP 状态码。
    pub status_code: Option<i64>,
    /// 上游传输方式。
    pub transport: Option<String>,
    /// 同一请求内的上游尝试序号。
    pub attempt_index: Option<i64>,
    /// 上游 HTTP 状态码。
    pub upstream_status_code: Option<i64>,
    /// 失败分类。
    pub failure_class: Option<String>,
    /// 上游响应 ID。
    pub response_id: Option<String>,
    /// 上游请求 ID。
    pub upstream_request_id: Option<String>,
    /// 搜索关键词。
    pub search: Option<String>,
}

/// SQLite 事件日志存储。
#[derive(Clone)]
pub struct SqliteEventLogStore {
    pool: SqlitePool,
}

impl SqliteEventLogStore {
    /// 构造存储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 写入事件日志。
    pub async fn append(&self, event: &EventLog) -> SqliteEventLogStoreResult<()> {
        append_event(&self.pool, event).await
    }

    /// 保留最新的指定数量事件日志。
    pub async fn trim_to_capacity(&self, capacity: u32) -> SqliteEventLogStoreResult<u64> {
        trim_to_capacity(&self.pool, capacity).await
    }

    /// 分页查询事件日志。
    pub async fn list(
        &self,
        filter: EventLogFilter,
        cursor: Option<String>,
        limit: u32,
    ) -> SqliteEventLogStoreResult<Page<EventLog>> {
        let fetch_limit = i64::from(limit) + 1;
        let mut builder = QueryBuilder::<Sqlite>::new(
            "select id, request_id, kind, level, account_id, route, model, status_code, transport, attempt_index, upstream_status_code, failure_class, response_id, upstream_request_id, latency_ms, message, metadata_json, created_at from event_logs",
        );
        push_filter(&mut builder, &filter, cursor.as_deref())?;
        builder.push(" order by created_at desc, id desc limit ");
        builder.push_bind(fetch_limit);

        let rows = builder.build().fetch_all(&self.pool).await?;
        let has_next = rows.len() > limit as usize;
        let take_count = rows.len().min(limit as usize);
        let items = rows
            .into_iter()
            .take(take_count)
            .map(|row| event_from_row(&row))
            .collect::<SqliteEventLogStoreResult<Vec<_>>>()?;
        let next_cursor = if has_next {
            items
                .last()
                .map(|event| encode_cursor(&event.created_at.to_rfc3339(), &event.id))
        } else {
            None
        };

        Ok(Page { items, next_cursor })
    }

    /// 按页码查询事件日志。
    pub async fn list_page(
        &self,
        filter: EventLogFilter,
        page: u32,
        page_size: u32,
    ) -> SqliteEventLogStoreResult<NumberedPage<EventLog>> {
        let page_size = page_size.clamp(1, 200);
        let total = count_events(&self.pool, &filter).await?;
        let offset = page_offset(page, page_size);
        let mut builder = QueryBuilder::<Sqlite>::new(
            "select id, request_id, kind, level, account_id, route, model, status_code, transport, attempt_index, upstream_status_code, failure_class, response_id, upstream_request_id, latency_ms, message, metadata_json, created_at from event_logs",
        );
        push_filter(&mut builder, &filter, None)?;
        builder.push(" order by created_at desc, id desc limit ");
        builder.push_bind(i64::from(page_size));
        builder.push(" offset ");
        builder.push_bind(offset.min(i64::MAX as u64) as i64);

        let rows = builder.build().fetch_all(&self.pool).await?;
        let items = rows
            .iter()
            .map(event_from_row)
            .collect::<SqliteEventLogStoreResult<Vec<_>>>()?;

        Ok(NumberedPage {
            items,
            total,
            page: page.max(1),
            page_size,
        })
    }

    /// 按 ID 读取事件日志。
    pub async fn get(&self, id: &str) -> SqliteEventLogStoreResult<Option<EventLog>> {
        let row = sqlx::query(
            "select id, request_id, kind, level, account_id, route, model, status_code, transport, attempt_index, upstream_status_code, failure_class, response_id, upstream_request_id, latency_ms, message, metadata_json, created_at from event_logs where id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| event_from_row(&row)).transpose()
    }

    /// 统计事件日志数量。
    pub async fn count(&self) -> SqliteEventLogStoreResult<u64> {
        let count: (i64,) = sqlx::query_as("select count(*) from event_logs")
            .fetch_one(&self.pool)
            .await?;
        Ok(count.0.max(0) as u64)
    }

    /// 清空事件日志。
    pub async fn clear(&self) -> SqliteEventLogStoreResult<u64> {
        let result = sqlx::query("delete from event_logs")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[async_trait]
impl EventLogStore for SqliteEventLogStore {
    async fn append(&self, event: &EventLog) -> EventLogStoreResult<()> {
        append_event(&self.pool, event).await.map_err(|error| {
            EventLogStoreError::OperationFailed {
                message: error.to_string(),
            }
        })?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct EventLogSummaryFields {
    transport: Option<String>,
    attempt_index: Option<i64>,
    upstream_status_code: Option<i64>,
    failure_class: Option<String>,
    response_id: Option<String>,
    upstream_request_id: Option<String>,
}

fn event_summary_fields(event: &EventLog) -> EventLogSummaryFields {
    EventLogSummaryFields {
        transport: event
            .transport
            .clone()
            .or_else(|| metadata_string(&event.metadata, &["transport"])),
        attempt_index: event
            .attempt_index
            .or_else(|| metadata_i64(&event.metadata, &["attemptIndex", "attempt_index"])),
        upstream_status_code: event.upstream_status_code.or_else(|| {
            metadata_i64(
                &event.metadata,
                &[
                    "upstreamStatus",
                    "upstreamStatusCode",
                    "upstream_status_code",
                ],
            )
        }),
        failure_class: event
            .failure_class
            .clone()
            .or_else(|| metadata_string(&event.metadata, &["failureClass", "failure_class"])),
        response_id: event
            .response_id
            .clone()
            .or_else(|| metadata_string(&event.metadata, &["responseId", "response_id"])),
        upstream_request_id: event.upstream_request_id.clone().or_else(|| {
            metadata_string(
                &event.metadata,
                &[
                    "upstreamRequestId",
                    "upstream_request_id",
                    "openaiRequestId",
                    "cfRay",
                ],
            )
        }),
    }
}

fn metadata_string(metadata: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        metadata
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToString::to_string)
    })
}

fn metadata_i64(metadata: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| metadata.get(*key).and_then(Value::as_i64))
}

async fn append_event(pool: &SqlitePool, event: &EventLog) -> SqliteEventLogStoreResult<()> {
    let summary = event_summary_fields(event);
    sqlx::query(
        "insert into event_logs (id, request_id, kind, level, account_id, route, model, status_code, transport, attempt_index, upstream_status_code, failure_class, response_id, upstream_request_id, latency_ms, message, metadata_json, created_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event.id)
    .bind(&event.request_id)
    .bind(&event.kind)
    .bind(level_to_db(event.level))
    .bind(&event.account_id)
    .bind(&event.route)
    .bind(&event.model)
    .bind(event.status_code)
    .bind(summary.transport)
    .bind(summary.attempt_index)
    .bind(summary.upstream_status_code)
    .bind(summary.failure_class)
    .bind(summary.response_id)
    .bind(summary.upstream_request_id)
    .bind(event.latency_ms)
    .bind(&event.message)
    .bind(event.metadata.to_string())
    .bind(event.created_at.to_rfc3339())
    .execute(pool)
    .await?;
    append_usage_time_bucket(pool, event).await?;
    Ok(())
}

async fn append_usage_time_bucket(
    pool: &SqlitePool,
    event: &EventLog,
) -> SqliteEventLogStoreResult<()> {
    let bucket_start = china_quarter_hour_start(event.created_at);
    let error_count = i64::from(is_error_event(event));
    let input_tokens = metadata_usage_i64(&event.metadata, "inputTokens");
    let output_tokens = metadata_usage_i64(&event.metadata, "outputTokens");
    let cached_tokens = metadata_usage_i64(&event.metadata, "cachedTokens");
    let first_token_latency = metadata_first_token_latency(&event.metadata).unwrap_or(0);
    let first_token_latency_count = i64::from(first_token_latency > 0);
    let latency = event.latency_ms.filter(|value| *value > 0).unwrap_or(0);
    let latency_count = i64::from(latency > 0);
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        r"
insert into usage_time_buckets (
  bucket_start,
  account_id,
  model,
  service_tier,
  request_count,
  error_count,
  input_tokens,
  output_tokens,
  cached_tokens,
  first_token_latency_sum,
  first_token_latency_count,
  latency_sum,
  latency_count,
  max_latency_ms,
  min_latency_ms,
  updated_at
) values (?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
on conflict(bucket_start, account_id, model, service_tier) do update set
  request_count = request_count + excluded.request_count,
  error_count = error_count + excluded.error_count,
  input_tokens = input_tokens + excluded.input_tokens,
  output_tokens = output_tokens + excluded.output_tokens,
  cached_tokens = cached_tokens + excluded.cached_tokens,
  first_token_latency_sum = first_token_latency_sum + excluded.first_token_latency_sum,
  first_token_latency_count = first_token_latency_count + excluded.first_token_latency_count,
  latency_sum = latency_sum + excluded.latency_sum,
  latency_count = latency_count + excluded.latency_count,
  max_latency_ms = max(max_latency_ms, excluded.max_latency_ms),
  min_latency_ms = case
    when min_latency_ms = 0 then excluded.min_latency_ms
    when excluded.min_latency_ms = 0 then min_latency_ms
    else min(min_latency_ms, excluded.min_latency_ms)
  end,
  updated_at = excluded.updated_at",
    )
    .bind(bucket_start.to_rfc3339())
    .bind(event.account_id.as_deref().unwrap_or_default())
    .bind(event.model.as_deref().unwrap_or_default())
    .bind(metadata_service_tier(&event.metadata).unwrap_or_default())
    .bind(error_count)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(cached_tokens)
    .bind(first_token_latency)
    .bind(first_token_latency_count)
    .bind(latency)
    .bind(latency_count)
    .bind(latency)
    .bind(latency)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

fn is_error_event(event: &EventLog) -> bool {
    event.level == EventLevel::Error || event.status_code.is_some_and(|status| status >= 400)
}

fn metadata_usage_i64(metadata: &Value, field: &str) -> i64 {
    metadata
        .get("usage")
        .and_then(|usage| usage.get(field))
        .or_else(|| metadata.get(field))
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .unwrap_or(0)
}

fn metadata_first_token_latency(metadata: &Value) -> Option<i64> {
    [
        "firstTokenMs",
        "first_token_ms",
        "firstTokenLatencyMs",
        "first_token_latency_ms",
    ]
    .into_iter()
    .find_map(|field| {
        metadata
            .get(field)
            .or_else(|| metadata.get("usage").and_then(|usage| usage.get(field)))
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
    })
}

fn metadata_service_tier(metadata: &Value) -> Option<&str> {
    metadata_service_tier_value(metadata)
        .or_else(|| metadata.get("usage").and_then(metadata_service_tier_value))
}

fn metadata_service_tier_value(value: &Value) -> Option<&str> {
    [
        "billingServiceTier",
        "billing_service_tier",
        "actualServiceTier",
        "actual_service_tier",
        "serviceTier",
        "service_tier",
    ]
    .into_iter()
    .find_map(|field| value.get(field).and_then(Value::as_str))
    .map(str::trim)
    .filter(|value| !value.is_empty())
}

async fn trim_to_capacity(pool: &SqlitePool, capacity: u32) -> SqliteEventLogStoreResult<u64> {
    let result = sqlx::query(
        "delete from event_logs where id in (
            select id from event_logs
            order by created_at desc, id desc
            limit -1 offset ?
        )",
    )
    .bind(i64::from(capacity))
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

async fn count_events(
    pool: &SqlitePool,
    filter: &EventLogFilter,
) -> SqliteEventLogStoreResult<u64> {
    let mut builder = QueryBuilder::<Sqlite>::new("select count(*) from event_logs");
    push_filter(&mut builder, filter, None)?;
    let (total,): (i64,) = builder.build_query_as().fetch_one(pool).await?;
    Ok(total.max(0) as u64)
}

fn push_filter(
    builder: &mut QueryBuilder<Sqlite>,
    filter: &EventLogFilter,
    cursor: Option<&str>,
) -> SqliteEventLogStoreResult<()> {
    let mut separated = builder.separated(" and ");
    separated.push(" where 1 = 1");

    if let Some(kind) = filter.kind.as_deref() {
        separated.push("kind = ");
        separated.push_bind_unseparated(kind);
    }
    if let Some(level) = filter.level {
        separated.push("level = ");
        separated.push_bind_unseparated(level_to_db(level));
    }
    if let Some(request_id) = filter.request_id.as_deref() {
        separated.push("request_id = ");
        separated.push_bind_unseparated(request_id);
    }
    if let Some(account_id) = filter.account_id.as_deref() {
        separated.push("account_id = ");
        separated.push_bind_unseparated(account_id);
    }
    if let Some(route) = filter.route.as_deref() {
        separated.push("route = ");
        separated.push_bind_unseparated(route);
    }
    if let Some(model) = filter.model.as_deref() {
        separated.push("model = ");
        separated.push_bind_unseparated(model);
    }
    if let Some(status_code) = filter.status_code {
        separated.push("status_code = ");
        separated.push_bind_unseparated(status_code);
    }
    if let Some(transport) = filter.transport.as_deref() {
        separated.push("transport = ");
        separated.push_bind_unseparated(transport);
    }
    if let Some(attempt_index) = filter.attempt_index {
        separated.push("attempt_index = ");
        separated.push_bind_unseparated(attempt_index);
    }
    if let Some(upstream_status_code) = filter.upstream_status_code {
        separated.push("upstream_status_code = ");
        separated.push_bind_unseparated(upstream_status_code);
    }
    if let Some(failure_class) = filter.failure_class.as_deref() {
        separated.push("failure_class = ");
        separated.push_bind_unseparated(failure_class);
    }
    if let Some(response_id) = filter.response_id.as_deref() {
        separated.push("response_id = ");
        separated.push_bind_unseparated(response_id);
    }
    if let Some(upstream_request_id) = filter.upstream_request_id.as_deref() {
        separated.push("upstream_request_id = ");
        separated.push_bind_unseparated(upstream_request_id);
    }
    if let Some(search) = filter.search.as_deref() {
        let pattern = format!("%{search}%");
        separated.push("(message like ");
        separated.push_bind_unseparated(pattern.clone());
        separated.push_unseparated(" or metadata_json like ");
        separated.push_bind_unseparated(pattern);
        separated.push_unseparated(")");
    }
    if let Some(cursor) = cursor {
        let (created_at, id) =
            decode_cursor(cursor).ok_or(SqliteEventLogStoreError::InvalidCursor)?;
        separated.push("(created_at < ");
        separated.push_bind_unseparated(created_at.clone());
        separated.push_unseparated(" or (created_at = ");
        separated.push_bind_unseparated(created_at);
        separated.push_unseparated(" and id < ");
        separated.push_bind_unseparated(id);
        separated.push_unseparated("))");
    }

    Ok(())
}

fn event_from_row(row: &sqlx::sqlite::SqliteRow) -> SqliteEventLogStoreResult<EventLog> {
    Ok(EventLog {
        id: row.get("id"),
        request_id: row.get("request_id"),
        kind: row.get("kind"),
        level: level_from_db(&row.get::<String, _>("level"))?,
        account_id: row.get("account_id"),
        route: row.get("route"),
        model: row.get("model"),
        status_code: row.get("status_code"),
        transport: row.get("transport"),
        attempt_index: row.get("attempt_index"),
        upstream_status_code: row.get("upstream_status_code"),
        failure_class: row.get("failure_class"),
        response_id: row.get("response_id"),
        upstream_request_id: row.get("upstream_request_id"),
        latency_ms: row.get("latency_ms"),
        message: row.get("message"),
        metadata: serde_json::from_str(&row.get::<String, _>("metadata_json"))?,
        created_at: parse_rfc3339(&row.get::<String, _>("created_at"))?,
    })
}

fn level_to_db(level: EventLevel) -> &'static str {
    match level {
        EventLevel::Debug => "debug",
        EventLevel::Info => "info",
        EventLevel::Warn => "warn",
        EventLevel::Error => "error",
    }
}

fn level_from_db(value: &str) -> SqliteEventLogStoreResult<EventLevel> {
    match value {
        "debug" => Ok(EventLevel::Debug),
        "info" => Ok(EventLevel::Info),
        "warn" => Ok(EventLevel::Warn),
        "error" => Ok(EventLevel::Error),
        other => Err(SqliteEventLogStoreError::InvalidLevel(other.to_string())),
    }
}

fn parse_rfc3339(value: &str) -> SqliteEventLogStoreResult<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

/// 管理端日志服务相关类型。
/// 日志查询过滤器。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminLogFilter {
    /// 事件类别。
    pub kind: Option<String>,
    /// 事件等级。
    pub level: Option<EventLevel>,
    /// 请求 ID。
    pub request_id: Option<String>,
    /// 账号 ID。
    pub account_id: Option<String>,
    /// 路由。
    pub route: Option<String>,
    /// 模型。
    pub model: Option<String>,
    /// HTTP 状态码。
    pub status_code: Option<i64>,
    /// 上游传输方式。
    pub transport: Option<String>,
    /// 同一请求内的上游尝试序号。
    pub attempt_index: Option<i64>,
    /// 上游 HTTP 状态码。
    pub upstream_status_code: Option<i64>,
    /// 失败分类。
    pub failure_class: Option<String>,
    /// 上游响应 ID。
    pub response_id: Option<String>,
    /// 上游请求 ID。
    pub upstream_request_id: Option<String>,
    /// 搜索关键词。
    pub search: Option<String>,
}

impl From<AdminLogFilter> for EventLogFilter {
    fn from(filter: AdminLogFilter) -> Self {
        Self {
            kind: filter.kind,
            level: filter.level,
            request_id: filter.request_id,
            account_id: filter.account_id,
            route: filter.route,
            model: filter.model,
            status_code: filter.status_code,
            transport: filter.transport,
            attempt_index: filter.attempt_index,
            upstream_status_code: filter.upstream_status_code,
            failure_class: filter.failure_class,
            response_id: filter.response_id,
            upstream_request_id: filter.upstream_request_id,
            search: filter.search,
        }
    }
}

/// 日志状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLogState {
    /// 是否启用。
    pub enabled: bool,
    /// 内存容量。
    pub capacity: u32,
    /// 是否捕获请求体。
    pub capture_body: bool,
    /// 已存储数量。
    pub stored_count: u64,
}

/// 日志状态更新。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminLogStateUpdate {
    /// 是否启用。
    pub enabled: Option<bool>,
    /// 日志容量。
    pub capacity: Option<u32>,
    /// 是否捕获请求体。
    pub capture_body: Option<bool>,
}

/// 清空日志结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminClearLogs {
    /// 清理数量。
    pub cleared: u64,
}

/// 管理端日志错误。
#[derive(Debug, Error)]
pub enum AdminLogError {
    /// 列表失败。
    #[error("failed to list event logs")]
    List,
    /// 读取失败。
    #[error("failed to get event log")]
    Get,
    /// 计数失败。
    #[error("failed to count event logs")]
    Count,
    /// 清空失败。
    #[error("failed to clear event logs")]
    Clear,
    /// 写入失败。
    #[error("failed to append event log")]
    Append,
    /// 裁剪失败。
    #[error("failed to trim event logs")]
    Trim,
    /// 日志容量非法。
    #[error("log capacity must be greater than zero")]
    InvalidCapacity,
}

/// 管理端日志服务。
#[derive(Clone)]
pub struct AdminLogService {
    store: SqliteEventLogStore,
    settings: std::sync::Arc<tokio::sync::RwLock<AdminLogSettings>>,
}

#[derive(Debug, Clone, Copy)]
struct AdminLogSettings {
    enabled: bool,
    capacity: u32,
    capture_body: bool,
}

impl AdminLogService {
    /// 构造管理端日志服务。
    pub fn new(
        store: SqliteEventLogStore,
        enabled: bool,
        capacity: u32,
        capture_body: bool,
    ) -> Self {
        Self {
            store,
            settings: std::sync::Arc::new(tokio::sync::RwLock::new(AdminLogSettings {
                enabled,
                capacity,
                capture_body,
            })),
        }
    }

    /// 分页查询日志。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
        filter: AdminLogFilter,
    ) -> Result<Page<EventLog>, AdminLogError> {
        self.store
            .list(filter.into(), cursor, limit)
            .await
            .map_err(|_| AdminLogError::List)
    }

    /// 按页码查询日志。
    pub async fn list_page(
        &self,
        page: u32,
        page_size: u32,
        filter: AdminLogFilter,
    ) -> Result<NumberedPage<EventLog>, AdminLogError> {
        self.store
            .list_page(filter.into(), page, page_size)
            .await
            .map_err(|_| AdminLogError::List)
    }

    /// 按 ID 读取日志。
    pub async fn get(&self, id: &str) -> Result<Option<EventLog>, AdminLogError> {
        self.store.get(id).await.map_err(|_| AdminLogError::Get)
    }

    /// 读取日志状态。
    pub async fn state(&self) -> Result<AdminLogState, AdminLogError> {
        let settings = *self.settings.read().await;
        Ok(AdminLogState {
            enabled: settings.enabled,
            capacity: settings.capacity,
            capture_body: settings.capture_body,
            stored_count: self.store.count().await.map_err(|_| AdminLogError::Count)?,
        })
    }

    /// 更新日志状态。
    pub async fn update_state(
        &self,
        update: AdminLogStateUpdate,
    ) -> Result<AdminLogState, AdminLogError> {
        if matches!(update.capacity, Some(0)) {
            return Err(AdminLogError::InvalidCapacity);
        }

        let trim_capacity = {
            let mut settings = self.settings.write().await;
            if let Some(enabled) = update.enabled {
                settings.enabled = enabled;
            }
            if let Some(capacity) = update.capacity {
                settings.capacity = capacity;
            }
            if let Some(capture_body) = update.capture_body {
                settings.capture_body = capture_body;
            }
            update.capacity
        };

        if let Some(capacity) = trim_capacity {
            self.store
                .trim_to_capacity(capacity)
                .await
                .map_err(|_| AdminLogError::Trim)?;
        }

        self.state().await
    }

    /// 清空日志。
    pub async fn clear(&self) -> Result<AdminClearLogs, AdminLogError> {
        self.store
            .clear()
            .await
            .map(|cleared| AdminClearLogs { cleared })
            .map_err(|_| AdminLogError::Clear)
    }

    /// 记录事件日志。
    pub async fn record(&self, mut event: EventLog) -> Result<(), AdminLogError> {
        let settings = *self.settings.read().await;
        let policy = crate::admin::monitoring::events::EventLogService::new(settings.enabled);
        if !policy.should_record(&event) {
            return Ok(());
        }
        apply_capture_body_policy(&mut event, settings.capture_body);
        self.store
            .append(&event)
            .await
            .map_err(|_| AdminLogError::Append)?;
        self.store
            .trim_to_capacity(settings.capacity)
            .await
            .map_err(|_| AdminLogError::Trim)?;
        Ok(())
    }
}

fn apply_capture_body_policy(event: &mut EventLog, capture_body: bool) {
    if capture_body {
        return;
    }
    let Some(metadata) = event.metadata.as_object_mut() else {
        return;
    };
    for key in [
        "body",
        "rawBody",
        "requestBody",
        "responseBody",
        "upstreamBody",
    ] {
        metadata.remove(key);
    }
}
