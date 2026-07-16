//! 管理端使用统计与请求记录查询服务。

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
use thiserror::Error;

use crate::{
    infra::{format::optional_nonnegative_i64_to_u64, json::NumberedPage},
    telemetry::{
        billing,
        usage::insights::{
            RequestHealthTimeBucket, UsageDiagnosticsDimension, UsageDiagnosticsInsights,
            UsageInsightsOverview, diagnostics, health_timeline, overview,
        },
        usage::store::{
            PgUsageRecordStore, PgUsageRecordStoreResult, UsageRecordFilter, push_filter,
        },
        usage::types::UsageRecord,
    },
};

/// 管理端使用记录查询过滤器。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageQueryFilter {
    /// 事件类别。
    pub kind: Option<String>,
    /// 调用方客户端 API key ID。
    pub client_api_key_id: Option<String>,
    /// 上游 provider。
    pub provider: Option<String>,
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
    /// 上游响应 ID。
    pub response_id: Option<String>,
    /// 上游请求 ID。
    pub upstream_request_id: Option<String>,
    /// 搜索关键词。
    pub search: Option<String>,
    /// 起始时间。
    pub start_time: Option<DateTime<Utc>>,
    /// 结束时间。
    pub end_time: Option<DateTime<Utc>>,
}

impl From<UsageQueryFilter> for UsageRecordFilter {
    fn from(filter: UsageQueryFilter) -> Self {
        Self {
            kind: filter.kind,
            client_api_key_id: filter.client_api_key_id,
            provider: filter.provider,
            request_id: filter.request_id,
            account_id: filter.account_id,
            route: filter.route,
            model: filter.model,
            status_code: filter.status_code,
            transport: filter.transport,
            attempt_index: filter.attempt_index,
            response_id: filter.response_id,
            upstream_request_id: filter.upstream_request_id,
            search: filter.search,
            start_time: filter.start_time,
            end_time: filter.end_time,
        }
    }
}

/// 清空使用记录结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearUsageRecords {
    /// 清理数量。
    pub cleared: u64,
}

/// 管理端使用记录计费明细。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UsageRecordBilling {
    pub input_amount: f64,
    pub output_amount: f64,
    pub cache_read_amount: f64,
    pub cache_write_amount: f64,
    pub standard_amount: f64,
    pub total_amount: f64,
    pub input_price_per_mtoken: f64,
    pub output_price_per_mtoken: f64,
    pub cache_read_price_per_mtoken: f64,
    pub cache_write_price_per_mtoken: f64,
    pub service_tier: Option<String>,
    pub service_tier_display: String,
    pub multiplier: f64,
}

/// 管理端使用记录错误。
#[derive(Debug, Error)]
pub enum UsageQueryError {
    /// 列表失败。
    #[error("failed to list usage records")]
    List,
    /// 读取失败。
    #[error("failed to get usage record")]
    Get,
    /// 清空失败。
    #[error("failed to clear usage records")]
    Clear,
    /// 账号关联信息读取失败。
    #[error("failed to load usage record accounts")]
    Accounts,
}

/// 管理端使用统计与请求记录查询服务。
#[derive(Clone)]
pub struct UsageQueryService {
    store: PgUsageRecordStore,
}

impl UsageQueryService {
    /// 构造管理端使用统计与请求记录查询服务。
    pub fn new(store: PgUsageRecordStore) -> Self {
        Self { store }
    }

    /// 查询最近的日志。
    pub async fn list_recent(
        &self,
        limit: u32,
        filter: UsageQueryFilter,
    ) -> Result<Vec<UsageRecord>, UsageQueryError> {
        self.store
            .list_recent(filter.into(), limit)
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 按页码查询日志。
    pub async fn list_page(
        &self,
        page: u32,
        page_size: u32,
        filter: UsageQueryFilter,
    ) -> Result<NumberedPage<UsageRecord>, UsageQueryError> {
        self.store
            .list_page(filter.into(), page, page_size)
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 按 ID 读取日志。
    pub async fn get(&self, id: &str) -> Result<Option<UsageRecord>, UsageQueryError> {
        self.store.get(id).await.map_err(|_| UsageQueryError::Get)
    }

    /// 读取使用记录关联账号的邮箱映射。
    pub async fn account_email_map(
        &self,
        items: &[UsageRecord],
    ) -> Result<HashMap<String, String>, UsageQueryError> {
        self.store
            .account_email_map(items)
            .await
            .map_err(|_| UsageQueryError::Accounts)
    }

    /// 汇总使用记录。
    pub async fn summary(
        &self,
        filter: UsageQueryFilter,
    ) -> Result<UsageRecordSummary, UsageQueryError> {
        self.store
            .summary(filter.into())
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 按账号聚合使用记录。
    pub async fn account_usage(
        &self,
        filter: UsageQueryFilter,
        limit: u32,
    ) -> Result<Vec<UsageRecordAccountUsage>, UsageQueryError> {
        self.store
            .account_usage(filter.into(), limit)
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 按模型来源聚合使用记录分布。
    pub async fn model_distribution(
        &self,
        filter: UsageQueryFilter,
        source: UsageRecordModelSource,
    ) -> Result<Vec<UsageRecordBreakdown>, UsageQueryError> {
        self.store
            .model_distribution(filter.into(), source)
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 按入站端点聚合使用记录分布。
    pub async fn endpoint_distribution(
        &self,
        filter: UsageQueryFilter,
    ) -> Result<Vec<UsageRecordBreakdown>, UsageQueryError> {
        self.store
            .endpoint_distribution(filter.into())
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 聚合使用记录趋势。
    pub async fn trend(
        &self,
        filter: UsageQueryFilter,
    ) -> Result<Vec<UsageRecordTrendPoint>, UsageQueryError> {
        self.store
            .trend(filter.into())
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 读取请求健康、性能与成本观测概览。
    pub async fn insights_overview(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<UsageInsightsOverview, UsageQueryError> {
        overview(self.store.pool(), start, end)
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 读取按最终请求终态去重的 15 分钟健康时间桶。
    pub async fn health_timeline(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<RequestHealthTimeBucket>, UsageQueryError> {
        health_timeline(self.store.pool(), start, end)
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 按维度读取请求热点诊断。
    pub async fn insights_diagnostics(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        dimension: UsageDiagnosticsDimension,
    ) -> Result<UsageDiagnosticsInsights, UsageQueryError> {
        diagnostics(self.store.pool(), start, end, dimension)
            .await
            .map_err(|_| UsageQueryError::List)
    }

    /// 清空日志。
    pub async fn clear(&self) -> Result<ClearUsageRecords, UsageQueryError> {
        self.store
            .clear()
            .await
            .map(|cleared| ClearUsageRecords { cleared })
            .map_err(|_| UsageQueryError::Clear)
    }
}

pub(crate) fn usage_record_billing(
    record: &UsageRecord,
    upstream_model: Option<&str>,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: u64,
) -> Option<UsageRecordBilling> {
    if input_tokens == 0 && output_tokens == 0 && cached_tokens == 0 && cache_write_tokens == 0 {
        return None;
    }

    let model = upstream_model
        .or(record.upstream_model.as_deref())
        .or(Some(record.model.as_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let service_tier = record.service_tier.as_deref();
    let breakdown = billing::calculate_billing(
        input_tokens,
        output_tokens,
        cached_tokens,
        cache_write_tokens,
        model,
        service_tier,
    );

    Some(UsageRecordBilling {
        input_amount: breakdown.input_amount,
        output_amount: breakdown.output_amount,
        cache_read_amount: breakdown.cache_read_amount,
        cache_write_amount: breakdown.cache_write_amount,
        standard_amount: breakdown.standard_amount,
        total_amount: breakdown.total_amount,
        input_price_per_mtoken: breakdown.input_price_per_mtoken,
        output_price_per_mtoken: breakdown.output_price_per_mtoken,
        cache_read_price_per_mtoken: breakdown.cache_read_price_per_mtoken,
        cache_write_price_per_mtoken: breakdown.cache_write_price_per_mtoken,
        service_tier: breakdown.service_tier.clone(),
        service_tier_display: breakdown
            .service_tier
            .as_deref()
            .map(format_service_tier)
            .unwrap_or_else(|| "Default".to_string()),
        multiplier: breakdown.tier_multiplier,
    })
}

fn format_service_tier(value: &str) -> String {
    match value {
        "priority" | "fast" => "Fast".to_string(),
        "flex" => "Flex".to_string(),
        "default" => "Default".to_string(),
        other => other.to_string(),
    }
}

pub(super) async fn count_usage_records(
    pool: &PgPool,
    filter: &UsageRecordFilter,
) -> PgUsageRecordStoreResult<u64> {
    let mut builder = QueryBuilder::<Postgres>::new("select count(*) from usage_records");
    push_filter(&mut builder, filter);
    let total: i64 = builder.build_query_scalar().fetch_one(pool).await?;
    Ok(total.max(0) as u64)
}

pub(super) async fn usage_summary(
    pool: &PgPool,
    filter: &UsageRecordFilter,
) -> PgUsageRecordStoreResult<UsageRecordSummary> {
    let mut builder = QueryBuilder::<Postgres>::new(
        r"
select
  count(*) as total_requests,
  coalesce(sum(input_tokens), 0)::bigint as input_tokens,
  coalesce(sum(output_tokens), 0)::bigint as output_tokens,
  coalesce(sum(cached_tokens), 0)::bigint as cached_tokens,
  coalesce(sum(cache_write_tokens), 0)::bigint as cache_write_tokens,
  avg(latency_ms::double precision) as average_latency_ms
from usage_records",
    );
    push_filter(&mut builder, filter);
    let row = builder.build().fetch_one(pool).await?;
    let input_tokens = nonnegative(row.get("input_tokens"));
    let output_tokens = nonnegative(row.get("output_tokens"));
    let cached_tokens = nonnegative(row.get("cached_tokens"));
    let cache_write_tokens = nonnegative(row.get("cache_write_tokens"));
    Ok(UsageRecordSummary {
        total_requests: nonnegative(row.get("total_requests")),
        input_tokens,
        output_tokens,
        cached_tokens,
        cache_write_tokens,
        total_tokens: input_tokens + output_tokens,
        average_latency_ms: row.get("average_latency_ms"),
    })
}

pub(super) async fn usage_account_usage(
    pool: &PgPool,
    filter: &UsageRecordFilter,
    limit: u32,
) -> PgUsageRecordStoreResult<Vec<UsageRecordAccountUsage>> {
    let mut builder = QueryBuilder::<Postgres>::new(
        r"
select
  account_id,
  coalesce(sum(input_tokens), 0)::bigint as input_tokens,
  coalesce(sum(output_tokens), 0)::bigint as output_tokens,
  max(created_at) as last_used_at
from usage_records",
    );
    push_filter(&mut builder, filter);
    builder.push(" group by account_id order by last_used_at desc, account_id limit ");
    builder.push_bind(i64::from(limit.clamp(1, 50)));
    Ok(builder
        .build()
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|row| {
            let input_tokens = nonnegative(row.get("input_tokens"));
            let output_tokens = nonnegative(row.get("output_tokens"));
            UsageRecordAccountUsage {
                account_id: row.get("account_id"),
                input_tokens,
                output_tokens,
                total_tokens: input_tokens + output_tokens,
                last_used_at: row.get("last_used_at"),
            }
        })
        .collect())
}

pub(super) async fn usage_record_account_email_map(
    pool: &PgPool,
    items: &[UsageRecord],
) -> PgUsageRecordStoreResult<HashMap<String, String>> {
    let mut account_ids = items
        .iter()
        .map(|item| item.account_id.clone())
        .collect::<Vec<_>>();
    account_ids.sort_unstable();
    account_ids.dedup();
    if account_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut builder = QueryBuilder::<Postgres>::new("select id, email from accounts where id in (");
    let mut values = builder.separated(", ");
    for account_id in account_ids {
        values.push_bind(account_id);
    }
    values.push_unseparated(")");
    Ok(builder
        .build()
        .fetch_all(pool)
        .await?
        .into_iter()
        .filter_map(|row| {
            row.get::<Option<String>, _>("email")
                .filter(|email| !email.trim().is_empty())
                .map(|email| (row.get("id"), email))
        })
        .collect())
}

pub(super) async fn usage_breakdown(
    pool: &PgPool,
    filter: &UsageRecordFilter,
    group_expression: &str,
    limit: u32,
) -> PgUsageRecordStoreResult<Vec<UsageRecordBreakdown>> {
    let mut builder = QueryBuilder::<Postgres>::new("select ");
    builder.push(group_expression);
    builder.push(
        r" as name,
  coalesce(nullif(upstream_model, ''), model) as billing_model,
  service_tier,
  count(*) as request_count,
  coalesce(sum(input_tokens), 0)::bigint as input_tokens,
  coalesce(sum(output_tokens), 0)::bigint as output_tokens,
  coalesce(sum(cached_tokens), 0)::bigint as cached_tokens,
  coalesce(sum(cache_write_tokens), 0)::bigint as cache_write_tokens,
  coalesce(sum(latency_ms), 0)::bigint as latency_sum,
  count(latency_ms) as latency_count
from usage_records",
    );
    push_filter(&mut builder, filter);
    builder.push(" group by name, billing_model, service_tier order by request_count desc limit ");
    builder.push_bind(i64::from(limit.clamp(1, 50) * 8));

    let mut grouped = BTreeMap::<String, BreakdownAccumulator>::new();
    for row in builder.build().fetch_all(pool).await? {
        let name: String = row.get("name");
        let input_tokens = nonnegative(row.get("input_tokens"));
        let output_tokens = nonnegative(row.get("output_tokens"));
        let cached_tokens = nonnegative(row.get("cached_tokens"));
        let cache_write_tokens = nonnegative(row.get("cache_write_tokens"));
        let request_count = nonnegative(row.get("request_count"));
        let latency_sum = nonnegative(row.get("latency_sum"));
        let latency_count = nonnegative(row.get("latency_count"));
        let billing_amount = usage_breakdown_billing_amount(
            input_tokens,
            output_tokens,
            cached_tokens,
            cache_write_tokens,
            &row.get::<String, _>("billing_model"),
            row.get::<Option<String>, _>("service_tier").as_deref(),
        );
        grouped
            .entry(name.clone())
            .or_insert_with(|| BreakdownAccumulator::new(name))
            .push(BreakdownSample {
                request_count,
                input_tokens,
                output_tokens,
                cached_tokens,
                cache_write_tokens,
                billing_amount,
                latency_sum,
                latency_count,
            });
    }
    let mut items = grouped
        .into_values()
        .map(BreakdownAccumulator::finish)
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .request_count
            .cmp(&left.request_count)
            .then_with(|| left.name.cmp(&right.name))
    });
    items.truncate(limit.clamp(1, 50) as usize);
    Ok(items)
}

pub(super) async fn usage_trend(
    pool: &PgPool,
    filter: &UsageRecordFilter,
) -> PgUsageRecordStoreResult<Vec<UsageRecordTrendPoint>> {
    let mut builder = QueryBuilder::<Postgres>::new(
        r"
select
  to_char(created_at at time zone 'Asia/Shanghai', 'YYYY-MM-DD') as date,
  coalesce(nullif(upstream_model, ''), model) as billing_model,
  service_tier,
  coalesce(sum(input_tokens), 0)::bigint as input_tokens,
  coalesce(sum(output_tokens), 0)::bigint as output_tokens,
  coalesce(sum(cached_tokens), 0)::bigint as cached_tokens,
  coalesce(sum(cache_write_tokens), 0)::bigint as cache_write_tokens,
  coalesce(sum(latency_ms), 0)::bigint as latency_sum,
  count(latency_ms) as latency_count
from usage_records",
    );
    push_filter(&mut builder, filter);
    builder.push(" group by date, billing_model, service_tier order by date asc");

    let mut days = BTreeMap::<String, UsageTrendAccumulator>::new();
    for row in builder.build().fetch_all(pool).await? {
        let date: String = row.get("date");
        let input_tokens = nonnegative(row.get("input_tokens"));
        let output_tokens = nonnegative(row.get("output_tokens"));
        let cached_tokens = nonnegative(row.get("cached_tokens"));
        let cache_write_tokens = nonnegative(row.get("cache_write_tokens"));
        let billing_amount = usage_breakdown_billing_amount(
            input_tokens,
            output_tokens,
            cached_tokens,
            cache_write_tokens,
            &row.get::<String, _>("billing_model"),
            row.get::<Option<String>, _>("service_tier").as_deref(),
        );
        days.entry(date.clone())
            .or_insert_with(|| UsageTrendAccumulator::new(date))
            .push(UsageTrendSample {
                input_tokens,
                output_tokens,
                cached_tokens,
                cache_write_tokens,
                billing_amount,
                latency_sum: nonnegative(row.get("latency_sum")),
                latency_count: nonnegative(row.get("latency_count")),
            });
    }
    let mut points = days
        .into_values()
        .map(UsageTrendAccumulator::finish)
        .collect::<Vec<_>>();
    if points.len() > 60 {
        points = points.split_off(points.len() - 60);
    }
    Ok(points)
}

pub(super) fn usage_record_from_row(row: &sqlx::postgres::PgRow) -> UsageRecord {
    UsageRecord {
        id: row.get("id"),
        request_id: row.get("request_id"),
        client_api_key_id: row.get("client_api_key_id"),
        kind: row.get("kind"),
        provider: row.get("provider"),
        account_id: row.get("account_id"),
        route: row.get("route"),
        model: row.get("model"),
        requested_model: row.get("requested_model"),
        upstream_model: row.get("upstream_model"),
        service_tier: row.get("service_tier"),
        status_code: row.get::<i32, _>("status_code").into(),
        transport: row.get("transport"),
        attempt_index: row.get("attempt_index"),
        response_id: row.get("response_id"),
        upstream_request_id: row.get("upstream_request_id"),
        latency_ms: row.get("latency_ms"),
        first_token_ms: row.get("first_token_ms"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cached_tokens: row.get("cached_tokens"),
        cache_write_tokens: row.get("cache_write_tokens"),
        reasoning_tokens: row.get("reasoning_tokens"),
        message: row.get("message"),
        metadata: row
            .get::<sqlx::types::Json<serde_json::Value>, _>("metadata_json")
            .0,
        created_at: row.get("created_at"),
    }
}

fn nonnegative(value: Option<i64>) -> u64 {
    optional_nonnegative_i64_to_u64(value)
}

fn usage_breakdown_billing_amount(
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: u64,
    model: &str,
    service_tier: Option<&str>,
) -> f64 {
    billing::calculate_billing_amount(
        input_tokens,
        output_tokens,
        cached_tokens,
        cache_write_tokens,
        model,
        service_tier,
    )
}

struct BreakdownAccumulator {
    item: UsageRecordBreakdown,
    latency_sum: u64,
    latency_count: u64,
}

struct BreakdownSample {
    request_count: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: u64,
    billing_amount: f64,
    latency_sum: u64,
    latency_count: u64,
}

impl BreakdownAccumulator {
    fn new(name: String) -> Self {
        Self {
            item: UsageRecordBreakdown {
                name,
                ..Default::default()
            },
            latency_sum: 0,
            latency_count: 0,
        }
    }

    fn push(&mut self, sample: BreakdownSample) {
        self.item.request_count += sample.request_count;
        self.item.input_tokens += sample.input_tokens;
        self.item.output_tokens += sample.output_tokens;
        self.item.cached_tokens += sample.cached_tokens;
        self.item.cache_write_tokens += sample.cache_write_tokens;
        self.item.total_tokens += sample.input_tokens + sample.output_tokens;
        self.item.standard_billing_amount += sample.billing_amount;
        self.item.actual_billing_amount += sample.billing_amount;
        self.item.account_billing_amount += sample.billing_amount;
        self.latency_sum += sample.latency_sum;
        self.latency_count += sample.latency_count;
    }

    fn finish(mut self) -> UsageRecordBreakdown {
        self.item.average_latency_ms =
            (self.latency_count > 0).then(|| self.latency_sum as f64 / self.latency_count as f64);
        self.item
    }
}

struct UsageTrendAccumulator {
    point: UsageRecordTrendPoint,
    latency_sum: u64,
    latency_count: u64,
}

struct UsageTrendSample {
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: u64,
    billing_amount: f64,
    latency_sum: u64,
    latency_count: u64,
}

impl UsageTrendAccumulator {
    fn new(date: String) -> Self {
        Self {
            point: UsageRecordTrendPoint {
                date,
                ..Default::default()
            },
            latency_sum: 0,
            latency_count: 0,
        }
    }

    fn push(&mut self, sample: UsageTrendSample) {
        self.point.input_tokens += sample.input_tokens;
        self.point.output_tokens += sample.output_tokens;
        self.point.cached_tokens += sample.cached_tokens;
        self.point.cache_write_tokens += sample.cache_write_tokens;
        self.point.total_tokens += sample.input_tokens + sample.output_tokens;
        self.point.standard_billing_amount += sample.billing_amount;
        self.point.actual_billing_amount += sample.billing_amount;
        self.latency_sum += sample.latency_sum;
        self.latency_count += sample.latency_count;
    }

    fn finish(mut self) -> UsageRecordTrendPoint {
        self.point.average_latency_ms =
            (self.latency_count > 0).then(|| self.latency_sum as f64 / self.latency_count as f64);
        self.point
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageRecordSummary {
    pub total_requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub average_latency_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecordAccountUsage {
    pub account_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub last_used_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageRecordModelSource {
    Requested,
    Upstream,
    Mapping,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageRecordBreakdown {
    pub name: String,
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub standard_billing_amount: f64,
    pub actual_billing_amount: f64,
    pub account_billing_amount: f64,
    pub average_latency_ms: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageRecordTrendPoint {
    pub date: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub standard_billing_amount: f64,
    pub actual_billing_amount: f64,
    pub average_latency_ms: Option<f64>,
}
