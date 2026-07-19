//! 从 `model_requests`、`ops_events` 与账号公共投影读取观测事实。

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    str::FromStr,
};

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder, Row};

use crate::{DecimalAmount, StoreError, StoreResult, postgres_unavailable, require_nonempty};

const MAX_PAGE_SIZE: u16 = 100;
const MAX_FILTER_BYTES: usize = 256;
const MAX_SEARCH_BYTES: usize = 512;
const MAX_ACCOUNT_IDS: usize = 200;
const DASHBOARD_ACCOUNT_LIMIT: u16 = 50;
const DIAGNOSTIC_LIMIT: i64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservabilityRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl ObservabilityRange {
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> StoreResult<Self> {
        if end.signed_duration_since(start) <= TimeDelta::zero() {
            return Err(invalid("time range must be positive"));
        }
        Ok(Self { start, end })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservabilityPageSize(u16);

impl ObservabilityPageSize {
    pub fn new(value: u16) -> StoreResult<Self> {
        if value == 0 || value > MAX_PAGE_SIZE {
            return Err(invalid("page size must be between 1 and 100"));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservabilityPageNumber(u32);

impl ObservabilityPageNumber {
    pub fn new(value: u32) -> StoreResult<Self> {
        if value == 0 {
            return Err(invalid("page number must be positive"));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservabilityCursor {
    pub observed_at: DateTime<Utc>,
    pub stable_id: String,
}

impl ObservabilityCursor {
    pub fn new(observed_at: DateTime<Utc>, stable_id: impl Into<String>) -> StoreResult<Self> {
        let stable_id = stable_id.into();
        validate_text(&stable_id, MAX_FILTER_BYTES, "cursor ID")?;
        Ok(Self {
            observed_at,
            stable_id,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageRecordFilter {
    pub client_api_key_ref: Option<String>,
    pub request_id: Option<String>,
    pub provider_account_ref: Option<String>,
    pub operation: Option<String>,
    pub provider_kind: Option<String>,
    pub model: Option<String>,
    pub outcome: Option<String>,
    pub status_code: Option<u16>,
    pub transport: Option<String>,
    pub attempt_index: Option<u32>,
    pub response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub search: Option<String>,
}

impl UsageRecordFilter {
    pub fn validate(&self) -> StoreResult<()> {
        for (value, field) in [
            (self.client_api_key_ref.as_deref(), "client API key filter"),
            (self.request_id.as_deref(), "request ID filter"),
            (
                self.provider_account_ref.as_deref(),
                "provider account filter",
            ),
            (self.operation.as_deref(), "operation filter"),
            (self.provider_kind.as_deref(), "provider filter"),
            (self.model.as_deref(), "model filter"),
            (self.transport.as_deref(), "transport filter"),
            (self.response_id.as_deref(), "response ID filter"),
            (
                self.upstream_request_id.as_deref(),
                "upstream request ID filter",
            ),
        ] {
            validate_optional_text(value, MAX_FILTER_BYTES, field)?;
        }
        validate_optional_text(self.search.as_deref(), MAX_SEARCH_BYTES, "search filter")?;
        if self.outcome.as_deref().is_some_and(|outcome| {
            !matches!(
                outcome,
                "running" | "succeeded" | "failed" | "cancelled" | "incomplete"
            )
        }) {
            return Err(invalid("unknown request outcome filter"));
        }
        if self
            .status_code
            .is_some_and(|status| !(100..=599).contains(&status))
        {
            return Err(invalid("status code filter must be between 100 and 599"));
        }
        if self
            .attempt_index
            .is_some_and(|index| index == 0 || i32::try_from(index).is_err())
        {
            return Err(invalid("attempt index filter is out of range"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecordQuery {
    pub range: ObservabilityRange,
    pub filter: UsageRecordFilter,
    pub cursor: Option<ObservabilityCursor>,
    pub page: ObservabilityPageNumber,
    pub page_size: ObservabilityPageSize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpsErrorFilter {
    pub client_api_key_ref: Option<String>,
    pub request_id: Option<String>,
    pub provider_account_ref: Option<String>,
    pub provider_kind: Option<String>,
    pub operation: Option<String>,
    pub model: Option<String>,
    pub transport: Option<String>,
    pub attempt_index: Option<u32>,
    pub response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub failure_kind: Option<String>,
    pub status_code: Option<u16>,
    pub search: Option<String>,
}

impl OpsErrorFilter {
    pub fn validate(&self) -> StoreResult<()> {
        for (value, field) in [
            (self.client_api_key_ref.as_deref(), "client API key filter"),
            (self.request_id.as_deref(), "request ID filter"),
            (
                self.provider_account_ref.as_deref(),
                "provider account filter",
            ),
            (self.provider_kind.as_deref(), "provider filter"),
            (self.operation.as_deref(), "operation filter"),
            (self.model.as_deref(), "model filter"),
            (self.transport.as_deref(), "transport filter"),
            (self.response_id.as_deref(), "response ID filter"),
            (
                self.upstream_request_id.as_deref(),
                "upstream request ID filter",
            ),
            (self.failure_kind.as_deref(), "failure kind filter"),
        ] {
            validate_optional_text(value, MAX_FILTER_BYTES, field)?;
        }
        validate_optional_text(self.search.as_deref(), MAX_SEARCH_BYTES, "search filter")?;
        if self
            .status_code
            .is_some_and(|status| !(100..=599).contains(&status))
        {
            return Err(invalid("status code filter must be between 100 and 599"));
        }
        if self
            .attempt_index
            .is_some_and(|index| index == 0 || i32::try_from(index).is_err())
        {
            return Err(invalid("attempt index filter is out of range"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsErrorQuery {
    pub range: ObservabilityRange,
    pub filter: OpsErrorFilter,
    pub cursor: Option<ObservabilityCursor>,
    pub page: ObservabilityPageNumber,
    pub page_size: ObservabilityPageSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticDimension {
    Provider,
    Model,
    Account,
    ApiKey,
    Transport,
    Failure,
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrencyCostTotal {
    pub currency: String,
    pub amount: DecimalAmount,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CostCoverage {
    pub provider_reported_count: u64,
    pub calculated_count: u64,
    pub unavailable_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestMetrics {
    pub request_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub cancelled_count: u64,
    pub incomplete_count: u64,
    pub caller_error_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub first_token_latency_sum: u64,
    pub first_token_latency_count: u64,
    pub latency_sum: u64,
    pub latency_count: u64,
    pub max_latency_ms: Option<u64>,
    pub min_latency_ms: Option<u64>,
    /// 分母：`input_tokens is not null`，即上游确实报告过 input token 事实的请求。
    pub cache_eligible_request_count: u64,
    /// 分子：分母集合中 `cached_tokens > 0` 的请求。
    pub cache_hit_request_count: u64,
    pub latency_percentiles: LatencyPercentiles,
    pub first_token_latency_percentiles: LatencyPercentiles,
}

impl RequestMetrics {
    /// 请求级 cache hit rate；没有 input token 事实时返回 `None`。
    #[must_use]
    pub fn cache_hit_request_rate(&self) -> Option<f64> {
        (self.cache_eligible_request_count > 0)
            .then(|| self.cache_hit_request_count as f64 / self.cache_eligible_request_count as f64)
    }
}

/// PostgreSQL `percentile_cont` 的非负、有限毫秒值；bits 保留插值小数且可安全比较。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PercentileMilliseconds(u64);

impl PercentileMilliseconds {
    fn new(value: f64) -> StoreResult<Self> {
        if !value.is_finite() || value < 0.0 {
            return Err(postgres_unavailable("decode latency percentile"));
        }
        Ok(Self(value.to_bits()))
    }

    #[must_use]
    pub const fn as_f64(self) -> f64 {
        f64::from_bits(self.0)
    }
}

impl std::fmt::Debug for PercentileMilliseconds {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_f64().fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LatencyPercentiles {
    pub p50_ms: Option<PercentileMilliseconds>,
    pub p95_ms: Option<PercentileMilliseconds>,
    pub p99_ms: Option<PercentileMilliseconds>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttemptMetrics {
    pub attempt_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub cancelled_count: u64,
    pub incomplete_count: u64,
    pub rate_limited_count: u64,
    pub auth_failure_count: u64,
    pub provider_5xx_count: u64,
    pub cost_coverage: CostCoverage,
    pub costs: Vec<CurrencyCostTotal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationGranularity {
    FifteenMinutes,
    Hour,
    Day,
}

impl ObservationGranularity {
    #[must_use]
    pub const fn seconds(self) -> i64 {
        match self {
            Self::FifteenMinutes => 15 * 60,
            Self::Hour => 60 * 60,
            Self::Day => 24 * 60 * 60,
        }
    }

    const fn sql_interval(self) -> &'static str {
        match self {
            Self::FifteenMinutes => "15 minutes",
            Self::Hour => "1 hour",
            Self::Day => "1 day",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestMetricPoint {
    pub bucket_start: DateTime<Utc>,
    pub granularity: ObservationGranularity,
    pub metrics: RequestMetrics,
    pub cost_coverage: CostCoverage,
    pub costs: Vec<CurrencyCostTotal>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderAccountMetrics {
    pub total: u64,
    pub enabled: u64,
    pub unavailable: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountUsageObservation {
    pub account_id: String,
    pub provider_instance_id: String,
    pub provider_kind: String,
    pub name: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub enabled: bool,
    pub availability: String,
    pub request_count: u64,
    pub success_count: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost_coverage: CostCoverage,
    pub costs: Vec<CurrencyCostTotal>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub models: Vec<ProviderAccountModelUsageObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountModelUsageObservation {
    pub model: String,
    pub request_count: u64,
    pub success_count: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost_coverage: CostCoverage,
    pub costs: Vec<CurrencyCostTotal>,
    pub last_used_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountUsageQuery {
    pub range: ObservabilityRange,
    pub account_ids: Option<Vec<String>>,
    pub limit: u16,
}

impl ProviderAccountUsageQuery {
    pub fn for_accounts(range: ObservabilityRange, account_ids: Vec<String>) -> StoreResult<Self> {
        if account_ids.is_empty() || account_ids.len() > MAX_ACCOUNT_IDS {
            return Err(invalid(
                "account usage query requires between 1 and 200 IDs",
            ));
        }
        validate_account_ids(&account_ids)?;
        Ok(Self {
            range,
            limit: u16::try_from(account_ids.len())
                .map_err(|_| invalid("account usage query is too large"))?,
            account_ids: Some(account_ids),
        })
    }

    pub fn recent(range: ObservabilityRange, limit: u16) -> StoreResult<Self> {
        if limit == 0 || usize::from(limit) > MAX_ACCOUNT_IDS {
            return Err(invalid("account usage limit must be between 1 and 200"));
        }
        Ok(Self {
            range,
            account_ids: None,
            limit,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardObservation {
    pub range: ObservabilityRange,
    pub requests: RequestMetrics,
    pub attempts: AttemptMetrics,
    pub provider_accounts: ProviderAccountMetrics,
    pub trend: Vec<RequestMetricPoint>,
    pub account_usage: Vec<ProviderAccountUsageObservation>,
    pub recent_requests: Vec<UsageRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    pub id: String,
    pub client_api_key_ref: String,
    pub config_revision: u64,
    pub protocol: String,
    pub operation: String,
    pub endpoint: String,
    pub client_transport: String,
    pub requested_model_id: String,
    pub input_token_estimate: u64,
    pub provider_instance_id: Option<String>,
    pub provider_kind: Option<String>,
    pub provider_account_ref: Option<String>,
    pub provider_account_name: Option<String>,
    pub provider_account_email: Option<String>,
    pub upstream_model_id: Option<String>,
    pub upstream_transport: Option<String>,
    pub http_version: Option<String>,
    pub attempt_count: u32,
    pub upstream_send_state: String,
    pub downstream_committed_at: Option<DateTime<Utc>>,
    pub outcome: String,
    pub client_status_code: Option<u16>,
    pub upstream_status_code: Option<u16>,
    pub client_response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub upstream_response_id: Option<String>,
    pub error_kind: Option<String>,
    pub provider_error_code: Option<String>,
    pub error_message: Option<String>,
    pub retry_after_ms: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost_source: String,
    pub cost_amount: Option<DecimalAmount>,
    pub cost_currency: Option<String>,
    pub transport_decision_wait_ms: Option<u64>,
    pub connect_ms: Option<u64>,
    pub headers_ms: Option<u64>,
    pub first_event_ms: Option<u64>,
    pub first_reasoning_ms: Option<u64>,
    pub first_text_ms: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub provider_processing_ms: Option<u64>,
    pub latency_ms: Option<u64>,
    pub client_ip: Option<String>,
    pub user_agent: Option<String>,
    pub reasoning_effort: Option<String>,
    pub reasoning_preset: Option<String>,
    pub request_kind: Option<String>,
    pub subagent_kind: Option<String>,
    pub compact: bool,
    pub started_at: DateTime<Utc>,
    pub deadline_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecordPage {
    pub items: Vec<UsageRecord>,
    pub total: u64,
    pub next_cursor: Option<ObservabilityCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageAttemptObservation {
    pub source: String,
    pub id: String,
    pub attempt_index: u32,
    pub component: String,
    pub operation: String,
    pub provider_instance_id: Option<String>,
    pub provider_kind: Option<String>,
    pub provider_account_ref: Option<String>,
    pub upstream_model_id: Option<String>,
    pub upstream_transport: Option<String>,
    pub upstream_send_state: Option<String>,
    pub outcome: String,
    pub downstream_committed: bool,
    pub status_code: Option<u16>,
    pub provider_error_code: Option<String>,
    pub failure_kind: Option<String>,
    pub retry_after_ms: Option<u64>,
    pub upstream_request_id: Option<String>,
    pub latency_ms: Option<u64>,
    pub message: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost_source: Option<String>,
    pub cost_amount: Option<DecimalAmount>,
    pub cost_currency: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecordDetail {
    pub request: UsageRecord,
    pub attempts: Vec<UsageAttemptObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderObservation {
    pub provider_kind: String,
    pub request_count: u64,
    pub attempt_count: u64,
    pub failure_count: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageOverview {
    pub range: ObservabilityRange,
    pub requests: RequestMetrics,
    pub attempts: AttemptMetrics,
    pub providers: Vec<ProviderObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticObservation {
    pub name: String,
    pub request_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub attempt_count: u64,
    pub total_tokens: u64,
    pub average_latency_ms: Option<u64>,
    pub cost_coverage: CostCoverage,
    pub costs: Vec<CurrencyCostTotal>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsErrorRecord {
    pub source: String,
    pub event_id: String,
    pub request_id: Option<String>,
    pub attempt_index: Option<u32>,
    pub client_api_key_ref: Option<String>,
    pub component: String,
    pub operation: String,
    pub provider_instance_id: Option<String>,
    pub provider_kind: Option<String>,
    pub provider_account_ref: Option<String>,
    pub upstream_model_id: Option<String>,
    pub upstream_transport: Option<String>,
    pub failure_kind: String,
    pub status_code: Option<u16>,
    pub provider_error_code: Option<String>,
    pub client_response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub latency_ms: Option<u64>,
    pub message: String,
    pub occurrence_count: u32,
    pub occurred_at: DateTime<Utc>,
    pub stable_sort_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsErrorPage {
    pub items: Vec<OpsErrorRecord>,
    pub total: u64,
    pub next_cursor: Option<ObservabilityCursor>,
}

#[async_trait]
pub trait ObservabilityRepository: Send + Sync {
    async fn dashboard_summary(
        &self,
        range: ObservabilityRange,
    ) -> StoreResult<DashboardObservation>;
    async fn dashboard_trend(
        &self,
        range: ObservabilityRange,
    ) -> StoreResult<Vec<RequestMetricPoint>>;
    async fn usage_trend(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
    ) -> StoreResult<Vec<RequestMetricPoint>>;
    async fn provider_account_usage(
        &self,
        query: ProviderAccountUsageQuery,
    ) -> StoreResult<Vec<ProviderAccountUsageObservation>>;
    async fn list_usage_records(&self, query: UsageRecordQuery) -> StoreResult<UsageRecordPage>;
    async fn usage_record_detail(&self, request_id: &str) -> StoreResult<UsageRecordDetail>;
    async fn usage_summary(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
    ) -> StoreResult<UsageOverview>;
    async fn usage_diagnostics(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
        dimension: DiagnosticDimension,
    ) -> StoreResult<Vec<DiagnosticObservation>>;
    async fn list_ops_errors(&self, query: OpsErrorQuery) -> StoreResult<OpsErrorPage>;
}

#[derive(Clone)]
pub struct PgObservabilityRepository {
    pool: PgPool,
}

impl PgObservabilityRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ObservabilityRepository for PgObservabilityRepository {
    async fn dashboard_summary(
        &self,
        range: ObservabilityRange,
    ) -> StoreResult<DashboardObservation> {
        let filter = UsageRecordFilter::default();
        let requests = request_metrics(&self.pool, range, &filter).await?;
        let attempts = attempt_metrics(&self.pool, range, &filter).await?;
        let provider_accounts = provider_account_metrics(&self.pool).await?;
        let trend = request_metric_series(&self.pool, range, &filter).await?;
        let account_usage = provider_account_usage(
            &self.pool,
            ProviderAccountUsageQuery::recent(range, DASHBOARD_ACCOUNT_LIMIT)?,
        )
        .await?;
        let recent_requests = list_usage_records(
            &self.pool,
            UsageRecordQuery {
                range,
                filter: UsageRecordFilter {
                    outcome: Some("succeeded".to_owned()),
                    ..UsageRecordFilter::default()
                },
                cursor: None,
                page: ObservabilityPageNumber::new(1)?,
                page_size: ObservabilityPageSize::new(10)?,
            },
        )
        .await?
        .items;
        Ok(DashboardObservation {
            range,
            requests,
            attempts,
            provider_accounts,
            trend,
            account_usage,
            recent_requests,
        })
    }

    async fn dashboard_trend(
        &self,
        range: ObservabilityRange,
    ) -> StoreResult<Vec<RequestMetricPoint>> {
        request_metric_series(&self.pool, range, &UsageRecordFilter::default()).await
    }

    async fn usage_trend(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
    ) -> StoreResult<Vec<RequestMetricPoint>> {
        request_metric_series(&self.pool, range, &filter).await
    }

    async fn provider_account_usage(
        &self,
        query: ProviderAccountUsageQuery,
    ) -> StoreResult<Vec<ProviderAccountUsageObservation>> {
        provider_account_usage(&self.pool, query).await
    }

    async fn list_usage_records(&self, query: UsageRecordQuery) -> StoreResult<UsageRecordPage> {
        list_usage_records(&self.pool, query).await
    }

    async fn usage_record_detail(&self, request_id: &str) -> StoreResult<UsageRecordDetail> {
        usage_record_detail(&self.pool, request_id).await
    }

    async fn usage_summary(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
    ) -> StoreResult<UsageOverview> {
        filter.validate()?;
        let requests = request_metrics(&self.pool, range, &filter).await?;
        let attempts = attempt_metrics(&self.pool, range, &filter).await?;
        let providers = provider_observations(&self.pool, range, &filter).await?;
        Ok(UsageOverview {
            range,
            requests,
            attempts,
            providers,
        })
    }

    async fn usage_diagnostics(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
        dimension: DiagnosticDimension,
    ) -> StoreResult<Vec<DiagnosticObservation>> {
        usage_diagnostics(&self.pool, range, &filter, dimension).await
    }

    async fn list_ops_errors(&self, query: OpsErrorQuery) -> StoreResult<OpsErrorPage> {
        list_ops_errors(&self.pool, query).await
    }
}

async fn request_metrics(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
) -> StoreResult<RequestMetrics> {
    filter.validate()?;
    let mut query = QueryBuilder::<Postgres>::new(
        "select count(*)::bigint as request_count,
                count(*) filter (where outcome = 'succeeded')::bigint as success_count,
                count(*) filter (where outcome = 'failed')::bigint as failure_count,
                count(*) filter (where outcome = 'cancelled')::bigint as cancelled_count,
                count(*) filter (where outcome = 'incomplete')::bigint as incomplete_count,
                count(*) filter (where client_status_code between 400 and 499)::bigint
                  as caller_error_count,
                coalesce(sum(input_tokens), 0)::bigint as input_tokens,
                coalesce(sum(output_tokens), 0)::bigint as output_tokens,
                coalesce(sum(cached_tokens), 0)::bigint as cached_tokens,
                coalesce(sum(cache_write_tokens), 0)::bigint as cache_write_tokens,
                coalesce(sum(reasoning_tokens), 0)::bigint as reasoning_tokens,
                coalesce(sum(total_tokens), 0)::bigint as total_tokens,
                coalesce(sum(first_token_ms), 0)::bigint as first_token_latency_sum,
                count(first_token_ms)::bigint as first_token_latency_count,
                coalesce(sum(latency_ms), 0)::bigint as latency_sum,
                count(latency_ms)::bigint as latency_count,
                max(latency_ms)::bigint as max_latency_ms,
                min(latency_ms)::bigint as min_latency_ms,
                count(*) filter (where input_tokens is not null)::bigint
                  as cache_eligible_request_count,
                count(*) filter (where input_tokens is not null and cached_tokens > 0)::bigint
                  as cache_hit_request_count,
                percentile_cont(0.50) within group (order by latency_ms)
                  as latency_p50_ms,
                percentile_cont(0.95) within group (order by latency_ms)
                  as latency_p95_ms,
                percentile_cont(0.99) within group (order by latency_ms)
                  as latency_p99_ms,
                percentile_cont(0.50) within group (order by first_token_ms)
                  as first_token_p50_ms,
                percentile_cont(0.95) within group (order by first_token_ms)
                  as first_token_p95_ms,
                percentile_cont(0.99) within group (order by first_token_ms)
                  as first_token_p99_ms
         from model_requests mr where mr.started_at >= ",
    );
    query.push_bind(range.start);
    query.push(" and mr.started_at < ");
    query.push_bind(range.end);
    push_usage_filter(&mut query, filter, "mr");
    let row = query
        .build()
        .fetch_one(pool)
        .await
        .map_err(|_| postgres_unavailable("load request metrics"))?;
    request_metrics_from_row(&row)
}

async fn request_metric_series(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
) -> StoreResult<Vec<RequestMetricPoint>> {
    filter.validate()?;
    let granularity = granularity_for(range);
    let mut query = QueryBuilder::<Postgres>::new("select date_bin(");
    query.push_bind(granularity.sql_interval());
    query.push(
        "::interval, mr.started_at, timestamptz '1970-01-01 00:00:00+00') as bucket_start,
                count(*)::bigint as request_count,
                count(*) filter (where outcome = 'succeeded')::bigint as success_count,
                count(*) filter (where outcome = 'failed')::bigint as failure_count,
                count(*) filter (where outcome = 'cancelled')::bigint as cancelled_count,
                count(*) filter (where outcome = 'incomplete')::bigint as incomplete_count,
                count(*) filter (where client_status_code between 400 and 499)::bigint
                  as caller_error_count,
                coalesce(sum(input_tokens), 0)::bigint as input_tokens,
                coalesce(sum(output_tokens), 0)::bigint as output_tokens,
                coalesce(sum(cached_tokens), 0)::bigint as cached_tokens,
                coalesce(sum(cache_write_tokens), 0)::bigint as cache_write_tokens,
                coalesce(sum(reasoning_tokens), 0)::bigint as reasoning_tokens,
                coalesce(sum(total_tokens), 0)::bigint as total_tokens,
                coalesce(sum(first_token_ms), 0)::bigint as first_token_latency_sum,
                count(first_token_ms)::bigint as first_token_latency_count,
                coalesce(sum(latency_ms), 0)::bigint as latency_sum,
                count(latency_ms)::bigint as latency_count,
                max(latency_ms)::bigint as max_latency_ms,
                min(latency_ms)::bigint as min_latency_ms,
                count(*) filter (where input_tokens is not null)::bigint
                  as cache_eligible_request_count,
                count(*) filter (where input_tokens is not null and cached_tokens > 0)::bigint
                  as cache_hit_request_count,
                percentile_cont(0.50) within group (order by latency_ms)
                  as latency_p50_ms,
                percentile_cont(0.95) within group (order by latency_ms)
                  as latency_p95_ms,
                percentile_cont(0.99) within group (order by latency_ms)
                  as latency_p99_ms,
                percentile_cont(0.50) within group (order by first_token_ms)
                  as first_token_p50_ms,
                percentile_cont(0.95) within group (order by first_token_ms)
                  as first_token_p95_ms,
                percentile_cont(0.99) within group (order by first_token_ms)
                  as first_token_p99_ms,
                count(*) filter (where cost_source = 'provider_reported')::bigint
                  as provider_reported_count,
                count(*) filter (where cost_source = 'calculated')::bigint
                  as calculated_count,
                count(*) filter (where cost_source = 'unavailable')::bigint
                  as unavailable_count
         from model_requests mr where mr.started_at >= ",
    );
    query.push_bind(range.start);
    query.push(" and mr.started_at < ");
    query.push_bind(range.end);
    push_usage_filter(&mut query, filter, "mr");
    query.push(" group by bucket_start order by bucket_start");
    let rows = query
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load request metric series"))?;

    let mut points = BTreeMap::new();
    for row in rows {
        let bucket_start = get(&row, "bucket_start")?;
        points.insert(
            bucket_start,
            RequestMetricPoint {
                bucket_start,
                granularity,
                metrics: request_metrics_from_row(&row)?,
                cost_coverage: coverage_from_row(&row)?,
                costs: Vec::new(),
            },
        );
    }
    let bucket_costs = request_costs_by_bucket(pool, range, filter, granularity).await?;
    for (bucket, costs) in bucket_costs {
        if let Some(point) = points.get_mut(&bucket) {
            point.costs = costs;
        }
    }
    fill_metric_gaps(range, granularity, points)
}

async fn request_costs_by_bucket(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
    granularity: ObservationGranularity,
) -> StoreResult<BTreeMap<DateTime<Utc>, Vec<CurrencyCostTotal>>> {
    let mut query = QueryBuilder::<Postgres>::new("select date_bin(");
    query.push_bind(granularity.sql_interval());
    query.push(
        "::interval, mr.started_at, timestamptz '1970-01-01 00:00:00+00') as bucket_start,
                mr.cost_currency, sum(mr.cost_amount)::text as amount
         from model_requests mr
         where mr.started_at >= ",
    );
    query.push_bind(range.start);
    query.push(" and mr.started_at < ");
    query.push_bind(range.end);
    query.push(" and mr.cost_amount is not null and mr.cost_currency is not null");
    push_usage_filter(&mut query, filter, "mr");
    query.push(" group by bucket_start, mr.cost_currency order by bucket_start, mr.cost_currency");
    let rows = query
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load request costs by bucket"))?;
    let mut result = BTreeMap::<DateTime<Utc>, Vec<CurrencyCostTotal>>::new();
    for row in rows {
        result
            .entry(get(&row, "bucket_start")?)
            .or_default()
            .push(cost_from_row(&row)?);
    }
    Ok(result)
}

async fn attempt_metrics(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
) -> StoreResult<AttemptMetrics> {
    filter.validate()?;
    let mut query = QueryBuilder::<Postgres>::new(
        "with selected_requests as (
           select mr.* from model_requests mr where mr.started_at >= ",
    );
    query.push_bind(range.start);
    query.push(" and mr.started_at < ");
    query.push_bind(range.end);
    push_usage_filter(&mut query, filter, "mr");
    query.push(
        "), failures as (
           select coalesce(oe.occurrence_count, 1)::bigint as occurrences,
                  oe.failure_kind, oe.status_code
           from ops_events oe
           join selected_requests sr on sr.id = oe.model_request_id
           union all
           select 1::bigint, coalesce(sr.error_kind, 'failed'),
                  coalesce(sr.upstream_status_code, sr.client_status_code)
           from selected_requests sr
           where sr.outcome = 'failed' and sr.attempt_count > 0
         )
         select coalesce((select sum(attempt_count) from selected_requests), 0)::bigint
                  as attempt_count,
                coalesce((select count(*) from selected_requests
                          where outcome = 'succeeded' and attempt_count > 0), 0)::bigint
                  as success_count,
                coalesce((select sum(occurrences) from failures), 0)::bigint as failure_count,
                coalesce((select count(*) from selected_requests
                          where outcome = 'cancelled' and attempt_count > 0), 0)::bigint
                  as cancelled_count,
                coalesce((select count(*) from selected_requests
                          where outcome = 'incomplete' and attempt_count > 0), 0)::bigint
                  as incomplete_count,
                coalesce((select sum(occurrences) from failures
                          where failure_kind in ('rate_limited', 'quota_exhausted')
                             or status_code = 429), 0)::bigint as rate_limited_count,
                coalesce((select sum(occurrences) from failures
                          where failure_kind in ('authentication', 'authorization',
                                                 'invalid_credential')
                             or status_code in (401, 403)), 0)::bigint as auth_failure_count,
                coalesce((select sum(occurrences) from failures
                          where status_code between 500 and 599), 0)::bigint
                  as provider_5xx_count,
                coalesce((select count(*) from selected_requests
                          where cost_source = 'provider_reported'), 0)::bigint
                  as provider_reported_count,
                coalesce((select count(*) from selected_requests
                          where cost_source = 'calculated'), 0)::bigint
                  as calculated_count,
                coalesce((select count(*) from selected_requests
                          where cost_source = 'unavailable'), 0)::bigint
                  as unavailable_count",
    );
    let row = query
        .build()
        .fetch_one(pool)
        .await
        .map_err(|_| postgres_unavailable("load attempt metrics"))?;
    Ok(AttemptMetrics {
        attempt_count: unsigned(&row, "attempt_count")?,
        success_count: unsigned(&row, "success_count")?,
        failure_count: unsigned(&row, "failure_count")?,
        cancelled_count: unsigned(&row, "cancelled_count")?,
        incomplete_count: unsigned(&row, "incomplete_count")?,
        rate_limited_count: unsigned(&row, "rate_limited_count")?,
        auth_failure_count: unsigned(&row, "auth_failure_count")?,
        provider_5xx_count: unsigned(&row, "provider_5xx_count")?,
        cost_coverage: coverage_from_row(&row)?,
        costs: request_costs(pool, range, filter).await?,
    })
}

async fn request_costs(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
) -> StoreResult<Vec<CurrencyCostTotal>> {
    let mut query = QueryBuilder::<Postgres>::new(
        "select mr.cost_currency, sum(mr.cost_amount)::text as amount
         from model_requests mr where mr.started_at >= ",
    );
    query.push_bind(range.start);
    query.push(" and mr.started_at < ");
    query.push_bind(range.end);
    query.push(" and mr.cost_amount is not null and mr.cost_currency is not null");
    push_usage_filter(&mut query, filter, "mr");
    query.push(" group by mr.cost_currency order by mr.cost_currency");
    query
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load request costs"))?
        .iter()
        .map(cost_from_row)
        .collect()
}

async fn provider_account_metrics(pool: &PgPool) -> StoreResult<ProviderAccountMetrics> {
    let row = sqlx::query(
        "select count(*)::bigint as total,
                count(*) filter (where enabled)::bigint as enabled,
                count(*) filter (where not enabled or availability <> 'ready')::bigint
                  as unavailable
         from provider_accounts",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| postgres_unavailable("load provider account metrics"))?;
    Ok(ProviderAccountMetrics {
        total: unsigned(&row, "total")?,
        enabled: unsigned(&row, "enabled")?,
        unavailable: unsigned(&row, "unavailable")?,
    })
}

async fn provider_observations(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
) -> StoreResult<Vec<ProviderObservation>> {
    let mut query = QueryBuilder::<Postgres>::new(
        "select coalesce(mr.provider_kind, 'unrouted') as provider_kind,
                count(*)::bigint as request_count,
                coalesce(sum(mr.attempt_count), 0)::bigint as attempt_count,
                count(*) filter (where mr.outcome = 'failed')::bigint as failure_count,
                coalesce(sum(mr.total_tokens), 0)::bigint as total_tokens
         from model_requests mr where mr.started_at >= ",
    );
    query.push_bind(range.start);
    query.push(" and mr.started_at < ");
    query.push_bind(range.end);
    push_usage_filter(&mut query, filter, "mr");
    query.push(" group by coalesce(mr.provider_kind, 'unrouted') order by request_count desc");
    let rows = query
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load provider observations"))?;
    rows.iter()
        .map(|row| {
            Ok(ProviderObservation {
                provider_kind: get(row, "provider_kind")?,
                request_count: unsigned(row, "request_count")?,
                attempt_count: unsigned(row, "attempt_count")?,
                failure_count: unsigned(row, "failure_count")?,
                total_tokens: unsigned(row, "total_tokens")?,
            })
        })
        .collect()
}

fn request_metrics_from_row(row: &sqlx::postgres::PgRow) -> StoreResult<RequestMetrics> {
    Ok(RequestMetrics {
        request_count: unsigned(row, "request_count")?,
        success_count: unsigned(row, "success_count")?,
        failure_count: unsigned(row, "failure_count")?,
        cancelled_count: unsigned(row, "cancelled_count")?,
        incomplete_count: unsigned(row, "incomplete_count")?,
        caller_error_count: unsigned(row, "caller_error_count")?,
        input_tokens: unsigned(row, "input_tokens")?,
        output_tokens: unsigned(row, "output_tokens")?,
        cached_tokens: unsigned(row, "cached_tokens")?,
        cache_write_tokens: unsigned(row, "cache_write_tokens")?,
        reasoning_tokens: unsigned(row, "reasoning_tokens")?,
        total_tokens: unsigned(row, "total_tokens")?,
        first_token_latency_sum: unsigned(row, "first_token_latency_sum")?,
        first_token_latency_count: unsigned(row, "first_token_latency_count")?,
        latency_sum: unsigned(row, "latency_sum")?,
        latency_count: unsigned(row, "latency_count")?,
        max_latency_ms: optional_unsigned(row, "max_latency_ms")?,
        min_latency_ms: optional_unsigned(row, "min_latency_ms")?,
        cache_eligible_request_count: unsigned(row, "cache_eligible_request_count")?,
        cache_hit_request_count: unsigned(row, "cache_hit_request_count")?,
        latency_percentiles: LatencyPercentiles {
            p50_ms: optional_percentile(row, "latency_p50_ms")?,
            p95_ms: optional_percentile(row, "latency_p95_ms")?,
            p99_ms: optional_percentile(row, "latency_p99_ms")?,
        },
        first_token_latency_percentiles: LatencyPercentiles {
            p50_ms: optional_percentile(row, "first_token_p50_ms")?,
            p95_ms: optional_percentile(row, "first_token_p95_ms")?,
            p99_ms: optional_percentile(row, "first_token_p99_ms")?,
        },
    })
}

fn optional_percentile(
    row: &sqlx::postgres::PgRow,
    column: &str,
) -> StoreResult<Option<PercentileMilliseconds>> {
    row.try_get::<Option<f64>, _>(column)
        .map_err(|_| postgres_unavailable("decode latency percentile"))?
        .map(PercentileMilliseconds::new)
        .transpose()
}

fn coverage_from_row(row: &sqlx::postgres::PgRow) -> StoreResult<CostCoverage> {
    Ok(CostCoverage {
        provider_reported_count: unsigned(row, "provider_reported_count")?,
        calculated_count: unsigned(row, "calculated_count")?,
        unavailable_count: unsigned(row, "unavailable_count")?,
    })
}

fn granularity_for(range: ObservabilityRange) -> ObservationGranularity {
    let seconds = range.end.signed_duration_since(range.start).num_seconds();
    if seconds <= 2 * 24 * 60 * 60 {
        ObservationGranularity::FifteenMinutes
    } else if seconds <= 31 * 24 * 60 * 60 {
        ObservationGranularity::Hour
    } else {
        ObservationGranularity::Day
    }
}

fn fill_metric_gaps(
    range: ObservabilityRange,
    granularity: ObservationGranularity,
    mut points: BTreeMap<DateTime<Utc>, RequestMetricPoint>,
) -> StoreResult<Vec<RequestMetricPoint>> {
    let seconds = granularity.seconds();
    let start_epoch = range.start.timestamp().div_euclid(seconds) * seconds;
    let mut bucket = DateTime::from_timestamp(start_epoch, 0)
        .ok_or_else(|| invalid("metric range start is outside supported timestamps"))?;
    let step = TimeDelta::seconds(seconds);
    let mut result = Vec::new();
    while bucket < range.end {
        result.push(points.remove(&bucket).unwrap_or(RequestMetricPoint {
            bucket_start: bucket,
            granularity,
            metrics: RequestMetrics::default(),
            cost_coverage: CostCoverage::default(),
            costs: Vec::new(),
        }));
        bucket = bucket
            .checked_add_signed(step)
            .ok_or_else(|| invalid("metric range exceeds supported timestamps"))?;
    }
    Ok(result)
}

fn push_usage_filter(query: &mut QueryBuilder<Postgres>, filter: &UsageRecordFilter, alias: &str) {
    if let Some(value) = &filter.client_api_key_ref {
        query.push(format!(" and {alias}.client_api_key_ref = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.request_id {
        query.push(format!(" and {alias}.id = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.provider_account_ref {
        query.push(format!(" and {alias}.provider_account_ref = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.operation {
        query.push(format!(" and {alias}.operation = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.provider_kind {
        query.push(format!(" and {alias}.provider_kind = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.model {
        query.push(format!(" and ({alias}.requested_model_id = "));
        query.push_bind(value.clone());
        query.push(format!(" or {alias}.upstream_model_id = "));
        query.push_bind(value.clone());
        query.push(")");
    }
    if let Some(value) = &filter.outcome {
        query.push(format!(" and {alias}.outcome = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = filter.status_code {
        query.push(format!(" and ({alias}.client_status_code = "));
        query.push_bind(i32::from(value));
        query.push(format!(" or {alias}.upstream_status_code = "));
        query.push_bind(i32::from(value));
        query.push(")");
    }
    if let Some(value) = &filter.transport {
        query.push(format!(" and ({alias}.client_transport = "));
        query.push_bind(value.clone());
        query.push(format!(" or {alias}.upstream_transport = "));
        query.push_bind(value.clone());
        query.push(")");
    }
    if let Some(value) = filter.attempt_index {
        query.push(format!(" and ({alias}.attempt_count = "));
        query.push_bind(i32::try_from(value).unwrap_or(i32::MAX));
        query.push(format!(
            " or exists (select 1 from ops_events attempt_event
                          where attempt_event.model_request_id = {alias}.id
                            and attempt_event.attempt_index = "
        ));
        query.push_bind(i32::try_from(value).unwrap_or(i32::MAX));
        query.push("))");
    }
    if let Some(value) = &filter.response_id {
        query.push(format!(" and ({alias}.client_response_id = "));
        query.push_bind(value.clone());
        query.push(format!(" or {alias}.upstream_response_id = "));
        query.push_bind(value.clone());
        query.push(")");
    }
    if let Some(value) = &filter.upstream_request_id {
        query.push(format!(" and {alias}.upstream_request_id = "));
        query.push_bind(value.clone());
    }
    if let Some(value) = &filter.search {
        query.push(format!(
            " and lower(concat_ws(' ', {alias}.id, {alias}.client_api_key_ref,
                    {alias}.requested_model_id, {alias}.provider_kind,
                    {alias}.provider_account_ref, {alias}.upstream_model_id,
                    {alias}.client_response_id, {alias}.upstream_request_id,
                    {alias}.error_kind, {alias}.provider_error_code,
                    {alias}.error_message)) like "
        ));
        query.push_bind(format!("%{}%", value.to_lowercase()));
    }
}

async fn provider_account_usage(
    pool: &PgPool,
    query: ProviderAccountUsageQuery,
) -> StoreResult<Vec<ProviderAccountUsageObservation>> {
    if let Some(account_ids) = &query.account_ids {
        validate_account_ids(account_ids)?;
    }
    if query.limit == 0 || usize::from(query.limit) > MAX_ACCOUNT_IDS {
        return Err(invalid("account usage limit must be between 1 and 200"));
    }

    let mut statement = QueryBuilder::<Postgres>::new(
        "select pa.id, pa.provider_instance_id, pa.provider_kind, pa.name, pa.email,
                pa.plan_type, pa.enabled, pa.availability,
                count(mr.id)::bigint as request_count,
                count(mr.id) filter (where mr.outcome = 'succeeded')::bigint as success_count,
                sum(mr.input_tokens)::bigint as input_tokens,
                sum(mr.output_tokens)::bigint as output_tokens,
                sum(mr.cached_tokens)::bigint as cached_tokens,
                sum(mr.cache_write_tokens)::bigint as cache_write_tokens,
                sum(mr.reasoning_tokens)::bigint as reasoning_tokens,
                sum(mr.total_tokens)::bigint as total_tokens,
                count(mr.id) filter (where mr.cost_source = 'provider_reported')::bigint
                  as provider_reported_count,
                count(mr.id) filter (where mr.cost_source = 'calculated')::bigint
                  as calculated_count,
                count(mr.id) filter (where mr.cost_source = 'unavailable')::bigint
                  as unavailable_count,
                max(mr.started_at) as last_used_at
         from provider_accounts pa
         left join model_requests mr
           on mr.provider_account_ref = pa.id and mr.started_at >= ",
    );
    statement.push_bind(query.range.start);
    statement.push(" and mr.started_at < ");
    statement.push_bind(query.range.end);
    if let Some(account_ids) = &query.account_ids {
        statement.push(" where pa.id = any(");
        statement.push_bind(account_ids.clone());
        statement.push("::text[])");
    }
    statement.push(
        " group by pa.id, pa.provider_instance_id, pa.provider_kind, pa.name, pa.email,
                   pa.plan_type, pa.enabled, pa.availability
          order by max(mr.started_at) desc nulls last, pa.name, pa.id limit ",
    );
    statement.push_bind(i64::from(query.limit));
    let rows = statement
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load provider account usage"))?;

    let mut observations = rows
        .iter()
        .map(provider_account_from_row)
        .collect::<StoreResult<Vec<_>>>()?;
    if observations.is_empty() {
        return Ok(observations);
    }
    let account_ids = observations
        .iter()
        .map(|item| item.account_id.clone())
        .collect::<Vec<_>>();
    let account_costs = account_costs(pool, query.range, &account_ids).await?;
    let model_rows = account_model_rows(pool, query.range, &account_ids).await?;
    let model_costs = account_model_costs(pool, query.range, &account_ids).await?;
    let account_positions = observations
        .iter()
        .enumerate()
        .map(|(index, item)| (item.account_id.clone(), index))
        .collect::<HashMap<_, _>>();
    for observation in &mut observations {
        observation.costs = account_costs
            .get(&observation.account_id)
            .cloned()
            .unwrap_or_default();
    }
    for row in model_rows {
        let account_id: String = get(&row, "provider_account_ref")?;
        let Some(position) = account_positions.get(&account_id).copied() else {
            continue;
        };
        let model: String = get(&row, "model")?;
        observations[position]
            .models
            .push(ProviderAccountModelUsageObservation {
                costs: model_costs
                    .get(&(account_id, model.clone()))
                    .cloned()
                    .unwrap_or_default(),
                model,
                request_count: unsigned(&row, "request_count")?,
                success_count: unsigned(&row, "success_count")?,
                input_tokens: optional_unsigned(&row, "input_tokens")?,
                output_tokens: optional_unsigned(&row, "output_tokens")?,
                cached_tokens: optional_unsigned(&row, "cached_tokens")?,
                cache_write_tokens: optional_unsigned(&row, "cache_write_tokens")?,
                reasoning_tokens: optional_unsigned(&row, "reasoning_tokens")?,
                total_tokens: optional_unsigned(&row, "total_tokens")?,
                cost_coverage: coverage_from_row(&row)?,
                last_used_at: get(&row, "last_used_at")?,
            });
    }
    Ok(observations)
}

async fn account_costs(
    pool: &PgPool,
    range: ObservabilityRange,
    account_ids: &[String],
) -> StoreResult<HashMap<String, Vec<CurrencyCostTotal>>> {
    let rows = sqlx::query(
        "select provider_account_ref, cost_currency, sum(cost_amount)::text as amount
         from model_requests
         where provider_account_ref = any($1::text[])
           and started_at >= $2 and started_at < $3
           and cost_amount is not null and cost_currency is not null
         group by provider_account_ref, cost_currency
         order by provider_account_ref, cost_currency",
    )
    .bind(account_ids)
    .bind(range.start)
    .bind(range.end)
    .fetch_all(pool)
    .await
    .map_err(|_| postgres_unavailable("load provider account costs"))?;
    let mut costs = HashMap::<String, Vec<CurrencyCostTotal>>::new();
    for row in rows {
        costs
            .entry(get(&row, "provider_account_ref")?)
            .or_default()
            .push(cost_from_row(&row)?);
    }
    Ok(costs)
}

async fn account_model_rows(
    pool: &PgPool,
    range: ObservabilityRange,
    account_ids: &[String],
) -> StoreResult<Vec<sqlx::postgres::PgRow>> {
    sqlx::query(
        "select provider_account_ref,
                coalesce(upstream_model_id, requested_model_id) as model,
                count(*)::bigint as request_count,
                count(*) filter (where outcome = 'succeeded')::bigint as success_count,
                sum(input_tokens)::bigint as input_tokens,
                sum(output_tokens)::bigint as output_tokens,
                sum(cached_tokens)::bigint as cached_tokens,
                sum(cache_write_tokens)::bigint as cache_write_tokens,
                sum(reasoning_tokens)::bigint as reasoning_tokens,
                sum(total_tokens)::bigint as total_tokens,
                count(*) filter (where cost_source = 'provider_reported')::bigint
                  as provider_reported_count,
                count(*) filter (where cost_source = 'calculated')::bigint
                  as calculated_count,
                count(*) filter (where cost_source = 'unavailable')::bigint
                  as unavailable_count,
                max(started_at) as last_used_at
         from model_requests
         where provider_account_ref = any($1::text[])
           and started_at >= $2 and started_at < $3
         group by provider_account_ref, coalesce(upstream_model_id, requested_model_id)
         order by provider_account_ref, request_count desc, model",
    )
    .bind(account_ids)
    .bind(range.start)
    .bind(range.end)
    .fetch_all(pool)
    .await
    .map_err(|_| postgres_unavailable("load provider account model usage"))
}

async fn account_model_costs(
    pool: &PgPool,
    range: ObservabilityRange,
    account_ids: &[String],
) -> StoreResult<HashMap<(String, String), Vec<CurrencyCostTotal>>> {
    let rows = sqlx::query(
        "select provider_account_ref,
                coalesce(upstream_model_id, requested_model_id) as model,
                cost_currency, sum(cost_amount)::text as amount
         from model_requests
         where provider_account_ref = any($1::text[])
           and started_at >= $2 and started_at < $3
           and cost_amount is not null and cost_currency is not null
         group by provider_account_ref, coalesce(upstream_model_id, requested_model_id),
                  cost_currency
         order by provider_account_ref, model, cost_currency",
    )
    .bind(account_ids)
    .bind(range.start)
    .bind(range.end)
    .fetch_all(pool)
    .await
    .map_err(|_| postgres_unavailable("load provider account model costs"))?;
    let mut costs = HashMap::<(String, String), Vec<CurrencyCostTotal>>::new();
    for row in rows {
        costs
            .entry((get(&row, "provider_account_ref")?, get(&row, "model")?))
            .or_default()
            .push(cost_from_row(&row)?);
    }
    Ok(costs)
}

fn provider_account_from_row(
    row: &sqlx::postgres::PgRow,
) -> StoreResult<ProviderAccountUsageObservation> {
    Ok(ProviderAccountUsageObservation {
        account_id: get(row, "id")?,
        provider_instance_id: get(row, "provider_instance_id")?,
        provider_kind: get(row, "provider_kind")?,
        name: get(row, "name")?,
        email: get(row, "email")?,
        plan_type: get(row, "plan_type")?,
        enabled: get(row, "enabled")?,
        availability: get(row, "availability")?,
        request_count: unsigned(row, "request_count")?,
        success_count: unsigned(row, "success_count")?,
        input_tokens: optional_unsigned(row, "input_tokens")?,
        output_tokens: optional_unsigned(row, "output_tokens")?,
        cached_tokens: optional_unsigned(row, "cached_tokens")?,
        cache_write_tokens: optional_unsigned(row, "cache_write_tokens")?,
        reasoning_tokens: optional_unsigned(row, "reasoning_tokens")?,
        total_tokens: optional_unsigned(row, "total_tokens")?,
        cost_coverage: coverage_from_row(row)?,
        costs: Vec::new(),
        last_used_at: get(row, "last_used_at")?,
        models: Vec::new(),
    })
}

const USAGE_RECORD_SELECT: &str =
    "select mr.id, mr.client_api_key_ref, mr.config_revision, mr.protocol, mr.operation,
            mr.endpoint, mr.client_transport, mr.requested_model_id,
            mr.input_token_estimate,             mr.provider_instance_id, mr.provider_kind, mr.provider_account_ref,
            pa.name as provider_account_name, pa.email as provider_account_email,
            mr.upstream_model_id, mr.upstream_transport, mr.http_version,
            mr.attempt_count, mr.upstream_send_state, mr.downstream_committed_at,
            mr.outcome, mr.client_status_code, mr.upstream_status_code,
            mr.client_response_id, mr.upstream_request_id, mr.upstream_response_id,
            mr.error_kind, mr.provider_error_code, mr.error_message, mr.retry_after_ms,
            mr.input_tokens, mr.output_tokens, mr.cached_tokens, mr.cache_write_tokens,
            mr.reasoning_tokens, mr.total_tokens, mr.cost_source, mr.cost_amount::text,
            mr.cost_currency, mr.transport_decision_wait_ms, mr.connect_ms, mr.headers_ms,
            mr.first_event_ms, mr.first_reasoning_ms, mr.first_text_ms, mr.first_token_ms,
            mr.provider_processing_ms, mr.latency_ms, mr.client_ip::text as client_ip,
            mr.user_agent, mr.reasoning_effort, mr.reasoning_preset, mr.request_kind,
            mr.subagent_kind, mr.compact, mr.started_at, mr.deadline_at, mr.completed_at
     from model_requests mr
     left join provider_accounts pa on pa.id = mr.provider_account_id";

async fn list_usage_records(
    pool: &PgPool,
    query: UsageRecordQuery,
) -> StoreResult<UsageRecordPage> {
    query.filter.validate()?;
    let total = count_usage_records(pool, query.range, &query.filter).await?;
    let mut statement = QueryBuilder::<Postgres>::new(USAGE_RECORD_SELECT);
    statement.push(" where mr.started_at >= ");
    statement.push_bind(query.range.start);
    statement.push(" and mr.started_at < ");
    statement.push_bind(query.range.end);
    push_usage_filter(&mut statement, &query.filter, "mr");
    if let Some(cursor) = &query.cursor {
        statement.push(" and (mr.started_at, mr.id) < (");
        statement.push_bind(cursor.observed_at);
        statement.push(", ");
        statement.push_bind(cursor.stable_id.clone());
        statement.push(")");
    }
    statement.push(" order by mr.started_at desc, mr.id desc limit ");
    statement.push_bind(i64::from(query.page_size.get()) + 1);
    if query.cursor.is_none() {
        let offset = u64::from(query.page.get() - 1)
            .checked_mul(u64::from(query.page_size.get()))
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| invalid("usage page offset is too large"))?;
        statement.push(" offset ");
        statement.push_bind(offset);
    }
    let rows = statement
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("list usage records"))?;
    let mut items = rows
        .iter()
        .map(usage_record_from_row)
        .collect::<StoreResult<Vec<_>>>()?;
    let has_more = items.len() > usize::from(query.page_size.get());
    if has_more {
        items.pop();
    }
    let next_cursor = if has_more {
        items
            .last()
            .map(|item| ObservabilityCursor::new(item.started_at, item.id.clone()))
            .transpose()?
    } else {
        None
    };
    Ok(UsageRecordPage {
        items,
        total,
        next_cursor,
    })
}

async fn count_usage_records(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
) -> StoreResult<u64> {
    let mut statement = QueryBuilder::<Postgres>::new(
        "select count(*)::bigint from model_requests mr where mr.started_at >= ",
    );
    statement.push_bind(range.start);
    statement.push(" and mr.started_at < ");
    statement.push_bind(range.end);
    push_usage_filter(&mut statement, filter, "mr");
    let total = statement
        .build_query_scalar::<i64>()
        .fetch_one(pool)
        .await
        .map_err(|_| postgres_unavailable("count usage records"))?;
    to_u64(total)
}

async fn usage_record_detail(pool: &PgPool, request_id: &str) -> StoreResult<UsageRecordDetail> {
    require_nonempty("model request", "id", request_id)?;
    validate_text(request_id, MAX_FILTER_BYTES, "request ID")?;
    let mut statement = QueryBuilder::<Postgres>::new(USAGE_RECORD_SELECT);
    statement.push(" where mr.id = ");
    statement.push_bind(request_id.to_owned());
    let row = statement
        .build()
        .fetch_optional(pool)
        .await
        .map_err(|_| postgres_unavailable("load usage record detail"))?
        .ok_or_else(|| StoreError::NotFound {
            entity: "model request",
            id: request_id.to_owned(),
        })?;
    let request = usage_record_from_row(&row)?;
    let rows = sqlx::query(
        "select id, attempt_index, component, operation,
                provider_instance_id, provider_kind, provider_account_ref, upstream_model_id,
                failure_kind, status_code, provider_error_code, retry_after_ms,
                upstream_request_id, latency_ms, message, created_at
         from ops_events where model_request_id = $1
         order by attempt_index, created_at, id",
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
    .map_err(|_| postgres_unavailable("load usage attempt observations"))?;
    let mut attempts = rows
        .iter()
        .map(intermediate_attempt_from_row)
        .collect::<StoreResult<Vec<_>>>()?;
    if request.attempt_count > 0 {
        attempts.push(final_attempt_from_request(&request));
    }
    Ok(UsageRecordDetail { request, attempts })
}

fn intermediate_attempt_from_row(
    row: &sqlx::postgres::PgRow,
) -> StoreResult<UsageAttemptObservation> {
    Ok(UsageAttemptObservation {
        source: "ops_event".to_owned(),
        id: get(row, "id")?,
        attempt_index: to_u32(get(row, "attempt_index")?)?,
        component: get(row, "component")?,
        operation: get(row, "operation")?,
        provider_instance_id: get(row, "provider_instance_id")?,
        provider_kind: get(row, "provider_kind")?,
        provider_account_ref: get(row, "provider_account_ref")?,
        upstream_model_id: get(row, "upstream_model_id")?,
        upstream_transport: None,
        upstream_send_state: None,
        outcome: "failed".to_owned(),
        downstream_committed: false,
        status_code: optional_status(row, "status_code")?,
        provider_error_code: get(row, "provider_error_code")?,
        failure_kind: get(row, "failure_kind")?,
        retry_after_ms: optional_unsigned(row, "retry_after_ms")?,
        upstream_request_id: get(row, "upstream_request_id")?,
        latency_ms: optional_unsigned(row, "latency_ms")?,
        message: get(row, "message")?,
        input_tokens: None,
        output_tokens: None,
        cached_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        cost_source: None,
        cost_amount: None,
        cost_currency: None,
        occurred_at: get(row, "created_at")?,
    })
}

fn final_attempt_from_request(request: &UsageRecord) -> UsageAttemptObservation {
    UsageAttemptObservation {
        source: "model_request".to_owned(),
        id: format!("{}:final", request.id),
        attempt_index: request.attempt_count,
        component: "model_request".to_owned(),
        operation: request.operation.clone(),
        provider_instance_id: request.provider_instance_id.clone(),
        provider_kind: request.provider_kind.clone(),
        provider_account_ref: request.provider_account_ref.clone(),
        upstream_model_id: request.upstream_model_id.clone(),
        upstream_transport: request.upstream_transport.clone(),
        upstream_send_state: Some(request.upstream_send_state.clone()),
        outcome: request.outcome.clone(),
        downstream_committed: request.downstream_committed_at.is_some(),
        status_code: request.upstream_status_code.or(request.client_status_code),
        provider_error_code: request.provider_error_code.clone(),
        failure_kind: request.error_kind.clone(),
        retry_after_ms: request.retry_after_ms,
        upstream_request_id: request.upstream_request_id.clone(),
        latency_ms: request.latency_ms,
        message: request.error_message.clone(),
        input_tokens: request.input_tokens,
        output_tokens: request.output_tokens,
        cached_tokens: request.cached_tokens,
        cache_write_tokens: request.cache_write_tokens,
        reasoning_tokens: request.reasoning_tokens,
        total_tokens: request.total_tokens,
        cost_source: Some(request.cost_source.clone()),
        cost_amount: request.cost_amount.clone(),
        cost_currency: request.cost_currency.clone(),
        occurred_at: request.completed_at.unwrap_or(request.started_at),
    }
}

async fn usage_diagnostics(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
    dimension: DiagnosticDimension,
) -> StoreResult<Vec<DiagnosticObservation>> {
    filter.validate()?;
    let dimension_sql = diagnostic_dimension_sql(dimension);
    let mut statement = QueryBuilder::<Postgres>::new("select ");
    statement.push(dimension_sql);
    statement.push(
        " as dimension_name,
                count(*)::bigint as request_count,
                count(*) filter (where mr.outcome = 'succeeded')::bigint as success_count,
                count(*) filter (where mr.outcome = 'failed')::bigint as failure_count,
                coalesce(sum(mr.attempt_count), 0)::bigint as attempt_count,
                coalesce(sum(mr.total_tokens), 0)::bigint as total_tokens,
                round(avg(mr.latency_ms))::bigint as average_latency_ms,
                count(*) filter (where mr.cost_source = 'provider_reported')::bigint
                  as provider_reported_count,
                count(*) filter (where mr.cost_source = 'calculated')::bigint
                  as calculated_count,
                count(*) filter (where mr.cost_source = 'unavailable')::bigint
                  as unavailable_count
         from model_requests mr where mr.started_at >= ",
    );
    statement.push_bind(range.start);
    statement.push(" and mr.started_at < ");
    statement.push_bind(range.end);
    push_usage_filter(&mut statement, filter, "mr");
    statement.push(" group by dimension_name order by request_count desc, dimension_name limit ");
    statement.push_bind(DIAGNOSTIC_LIMIT);
    let rows = statement
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load usage diagnostics"))?;
    let mut observations = rows
        .iter()
        .map(|row| {
            Ok(DiagnosticObservation {
                name: get(row, "dimension_name")?,
                request_count: unsigned(row, "request_count")?,
                success_count: unsigned(row, "success_count")?,
                failure_count: unsigned(row, "failure_count")?,
                attempt_count: unsigned(row, "attempt_count")?,
                total_tokens: unsigned(row, "total_tokens")?,
                average_latency_ms: optional_unsigned(row, "average_latency_ms")?,
                cost_coverage: coverage_from_row(row)?,
                costs: Vec::new(),
            })
        })
        .collect::<StoreResult<Vec<_>>>()?;
    let positions = observations
        .iter()
        .enumerate()
        .map(|(index, item)| (item.name.clone(), index))
        .collect::<HashMap<_, _>>();
    let costs = diagnostic_costs(pool, range, filter, dimension_sql).await?;
    for (name, values) in costs {
        if let Some(position) = positions.get(&name).copied() {
            observations[position].costs = values;
        }
    }
    Ok(observations)
}

async fn diagnostic_costs(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &UsageRecordFilter,
    dimension_sql: &'static str,
) -> StoreResult<HashMap<String, Vec<CurrencyCostTotal>>> {
    let mut statement = QueryBuilder::<Postgres>::new("select ");
    statement.push(dimension_sql);
    statement.push(
        " as dimension_name, mr.cost_currency, sum(mr.cost_amount)::text as amount
         from model_requests mr where mr.started_at >= ",
    );
    statement.push_bind(range.start);
    statement.push(" and mr.started_at < ");
    statement.push_bind(range.end);
    statement.push(" and mr.cost_amount is not null and mr.cost_currency is not null");
    push_usage_filter(&mut statement, filter, "mr");
    statement.push(
        " group by dimension_name, mr.cost_currency order by dimension_name, mr.cost_currency",
    );
    let rows = statement
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("load diagnostic costs"))?;
    let mut result = HashMap::<String, Vec<CurrencyCostTotal>>::new();
    for row in rows {
        result
            .entry(get(&row, "dimension_name")?)
            .or_default()
            .push(cost_from_row(&row)?);
    }
    Ok(result)
}

fn diagnostic_dimension_sql(dimension: DiagnosticDimension) -> &'static str {
    match dimension {
        DiagnosticDimension::Provider => "coalesce(mr.provider_kind, 'unrouted')",
        DiagnosticDimension::Model => {
            "coalesce(mr.upstream_model_id, mr.requested_model_id, 'unknown')"
        }
        DiagnosticDimension::Account => "coalesce(mr.provider_account_ref, 'unrouted')",
        DiagnosticDimension::ApiKey => "mr.client_api_key_ref",
        DiagnosticDimension::Transport => {
            "coalesce(mr.upstream_transport, mr.client_transport, 'unknown')"
        }
        DiagnosticDimension::Failure => "coalesce(mr.error_kind, 'none')",
        DiagnosticDimension::Status => {
            "coalesce(mr.upstream_status_code, mr.client_status_code)::text"
        }
    }
}

const OPS_ERRORS_CTE: &str = "with errors as (
       select 'model_request'::text as source,
              mr.id as event_id, mr.id as request_id,
              nullif(mr.attempt_count, 0) as attempt_index,
              mr.client_api_key_ref, 'model_request'::text as component, mr.operation,
              mr.provider_instance_id, mr.provider_kind,
              mr.provider_account_ref, mr.upstream_model_id, mr.upstream_transport,
              coalesce(mr.error_kind, 'failed') as failure_kind,
              coalesce(mr.upstream_status_code, mr.client_status_code) as status_code,
              mr.provider_error_code, mr.client_response_id, mr.upstream_request_id,
              mr.latency_ms, coalesce(mr.error_message, mr.error_kind, 'request failed') as message,
              1::integer as occurrence_count,
              coalesce(mr.completed_at, mr.started_at) as occurred_at,
              'model_request:' || mr.id as stable_sort_id
       from model_requests mr where mr.outcome = 'failed'
       union all
       select 'ops_event'::text, oe.id, oe.model_request_id, oe.attempt_index,
              mr.client_api_key_ref, oe.component, oe.operation,
              oe.provider_instance_id, oe.provider_kind, oe.provider_account_ref,
              oe.upstream_model_id, null::text, oe.failure_kind, oe.status_code,
              oe.provider_error_code, mr.client_response_id, oe.upstream_request_id,
              oe.latency_ms, oe.message, oe.occurrence_count, oe.created_at,
              'ops_event:' || oe.id
       from ops_events oe left join model_requests mr on mr.id = oe.model_request_id
     )";

async fn list_ops_errors(pool: &PgPool, query: OpsErrorQuery) -> StoreResult<OpsErrorPage> {
    query.filter.validate()?;
    let total = count_ops_errors(pool, query.range, &query.filter).await?;
    let mut statement = QueryBuilder::<Postgres>::new(OPS_ERRORS_CTE);
    statement.push(
        " select source, event_id, request_id, attempt_index, client_api_key_ref,
                 component, operation, provider_instance_id,
                 provider_kind, provider_account_ref, upstream_model_id, upstream_transport,
                 failure_kind, status_code, provider_error_code, client_response_id,
                 upstream_request_id, latency_ms, message, occurrence_count, occurred_at,
                 stable_sort_id
          from errors e where e.occurred_at >= ",
    );
    statement.push_bind(query.range.start);
    statement.push(" and e.occurred_at < ");
    statement.push_bind(query.range.end);
    push_ops_filter(&mut statement, &query.filter);
    if let Some(cursor) = &query.cursor {
        statement.push(" and (e.occurred_at, e.stable_sort_id) < (");
        statement.push_bind(cursor.observed_at);
        statement.push(", ");
        statement.push_bind(cursor.stable_id.clone());
        statement.push(")");
    }
    statement.push(" order by e.occurred_at desc, e.stable_sort_id desc limit ");
    statement.push_bind(i64::from(query.page_size.get()) + 1);
    if query.cursor.is_none() {
        let offset = u64::from(query.page.get() - 1)
            .checked_mul(u64::from(query.page_size.get()))
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| invalid("ops error page offset is too large"))?;
        statement.push(" offset ");
        statement.push_bind(offset);
    }
    let rows = statement
        .build()
        .fetch_all(pool)
        .await
        .map_err(|_| postgres_unavailable("list ops errors"))?;
    let mut items = rows
        .iter()
        .map(ops_error_from_row)
        .collect::<StoreResult<Vec<_>>>()?;
    let has_more = items.len() > usize::from(query.page_size.get());
    if has_more {
        items.pop();
    }
    let next_cursor = if has_more {
        items
            .last()
            .map(|item| ObservabilityCursor::new(item.occurred_at, item.stable_sort_id.clone()))
            .transpose()?
    } else {
        None
    };
    Ok(OpsErrorPage {
        items,
        total,
        next_cursor,
    })
}

async fn count_ops_errors(
    pool: &PgPool,
    range: ObservabilityRange,
    filter: &OpsErrorFilter,
) -> StoreResult<u64> {
    let mut statement = QueryBuilder::<Postgres>::new(OPS_ERRORS_CTE);
    statement.push(" select count(*)::bigint from errors e where e.occurred_at >= ");
    statement.push_bind(range.start);
    statement.push(" and e.occurred_at < ");
    statement.push_bind(range.end);
    push_ops_filter(&mut statement, filter);
    let total = statement
        .build_query_scalar::<i64>()
        .fetch_one(pool)
        .await
        .map_err(|_| postgres_unavailable("count ops errors"))?;
    to_u64(total)
}

fn push_ops_filter(statement: &mut QueryBuilder<Postgres>, filter: &OpsErrorFilter) {
    for (column, value) in [
        ("client_api_key_ref", &filter.client_api_key_ref),
        ("request_id", &filter.request_id),
        ("provider_account_ref", &filter.provider_account_ref),
        ("provider_kind", &filter.provider_kind),
        ("operation", &filter.operation),
        ("upstream_transport", &filter.transport),
        ("client_response_id", &filter.response_id),
        ("upstream_request_id", &filter.upstream_request_id),
        ("failure_kind", &filter.failure_kind),
    ] {
        if let Some(value) = value {
            statement.push(format!(" and e.{column} = "));
            statement.push_bind(value.clone());
        }
    }
    if let Some(model) = &filter.model {
        statement.push(" and e.upstream_model_id = ");
        statement.push_bind(model.clone());
    }
    if let Some(index) = filter.attempt_index {
        statement.push(" and e.attempt_index = ");
        statement.push_bind(i32::try_from(index).unwrap_or(i32::MAX));
    }
    if let Some(status) = filter.status_code {
        statement.push(" and e.status_code = ");
        statement.push_bind(i32::from(status));
    }
    if let Some(search) = &filter.search {
        statement.push(
            " and lower(concat_ws(' ', e.event_id, e.request_id, e.client_api_key_ref,
                    e.component, e.operation, e.provider_kind, e.provider_account_ref,
                    e.upstream_model_id, e.failure_kind, e.provider_error_code,
                    e.upstream_request_id, e.message)) like ",
        );
        statement.push_bind(format!("%{}%", search.to_lowercase()));
    }
}

fn usage_record_from_row(row: &sqlx::postgres::PgRow) -> StoreResult<UsageRecord> {
    Ok(UsageRecord {
        id: get(row, "id")?,
        client_api_key_ref: get(row, "client_api_key_ref")?,
        config_revision: unsigned(row, "config_revision")?,
        protocol: get(row, "protocol")?,
        operation: get(row, "operation")?,
        endpoint: get(row, "endpoint")?,
        client_transport: get(row, "client_transport")?,
        requested_model_id: get(row, "requested_model_id")?,
        input_token_estimate: unsigned(row, "input_token_estimate")?,
        provider_instance_id: get(row, "provider_instance_id")?,
        provider_kind: get(row, "provider_kind")?,
        provider_account_ref: get(row, "provider_account_ref")?,
        provider_account_name: get(row, "provider_account_name")?,
        provider_account_email: get(row, "provider_account_email")?,
        upstream_model_id: get(row, "upstream_model_id")?,
        upstream_transport: get(row, "upstream_transport")?,
        http_version: get(row, "http_version")?,
        attempt_count: to_u32(get(row, "attempt_count")?)?,
        upstream_send_state: get(row, "upstream_send_state")?,
        downstream_committed_at: get(row, "downstream_committed_at")?,
        outcome: get(row, "outcome")?,
        client_status_code: optional_status(row, "client_status_code")?,
        upstream_status_code: optional_status(row, "upstream_status_code")?,
        client_response_id: get(row, "client_response_id")?,
        upstream_request_id: get(row, "upstream_request_id")?,
        upstream_response_id: get(row, "upstream_response_id")?,
        error_kind: get(row, "error_kind")?,
        provider_error_code: get(row, "provider_error_code")?,
        error_message: get(row, "error_message")?,
        retry_after_ms: optional_unsigned(row, "retry_after_ms")?,
        input_tokens: optional_unsigned(row, "input_tokens")?,
        output_tokens: optional_unsigned(row, "output_tokens")?,
        cached_tokens: optional_unsigned(row, "cached_tokens")?,
        cache_write_tokens: optional_unsigned(row, "cache_write_tokens")?,
        reasoning_tokens: optional_unsigned(row, "reasoning_tokens")?,
        total_tokens: optional_unsigned(row, "total_tokens")?,
        cost_source: get(row, "cost_source")?,
        cost_amount: optional_decimal(row, "cost_amount")?,
        cost_currency: get(row, "cost_currency")?,
        transport_decision_wait_ms: optional_unsigned(row, "transport_decision_wait_ms")?,
        connect_ms: optional_unsigned(row, "connect_ms")?,
        headers_ms: optional_unsigned(row, "headers_ms")?,
        first_event_ms: optional_unsigned(row, "first_event_ms")?,
        first_reasoning_ms: optional_unsigned(row, "first_reasoning_ms")?,
        first_text_ms: optional_unsigned(row, "first_text_ms")?,
        first_token_ms: optional_unsigned(row, "first_token_ms")?,
        provider_processing_ms: optional_unsigned(row, "provider_processing_ms")?,
        latency_ms: optional_unsigned(row, "latency_ms")?,
        client_ip: get(row, "client_ip")?,
        user_agent: get(row, "user_agent")?,
        reasoning_effort: get(row, "reasoning_effort")?,
        reasoning_preset: get(row, "reasoning_preset")?,
        request_kind: get(row, "request_kind")?,
        subagent_kind: get(row, "subagent_kind")?,
        compact: get(row, "compact")?,
        started_at: get(row, "started_at")?,
        deadline_at: get(row, "deadline_at")?,
        completed_at: get(row, "completed_at")?,
    })
}

fn ops_error_from_row(row: &sqlx::postgres::PgRow) -> StoreResult<OpsErrorRecord> {
    Ok(OpsErrorRecord {
        source: get(row, "source")?,
        event_id: get(row, "event_id")?,
        request_id: get(row, "request_id")?,
        attempt_index: get::<Option<i32>>(row, "attempt_index")?
            .map(to_u32)
            .transpose()?,
        client_api_key_ref: get(row, "client_api_key_ref")?,
        component: get(row, "component")?,
        operation: get(row, "operation")?,
        provider_instance_id: get(row, "provider_instance_id")?,
        provider_kind: get(row, "provider_kind")?,
        provider_account_ref: get(row, "provider_account_ref")?,
        upstream_model_id: get(row, "upstream_model_id")?,
        upstream_transport: get(row, "upstream_transport")?,
        failure_kind: get(row, "failure_kind")?,
        status_code: optional_status(row, "status_code")?,
        provider_error_code: get(row, "provider_error_code")?,
        client_response_id: get(row, "client_response_id")?,
        upstream_request_id: get(row, "upstream_request_id")?,
        latency_ms: optional_unsigned(row, "latency_ms")?,
        message: get(row, "message")?,
        occurrence_count: to_u32(get(row, "occurrence_count")?)?,
        occurred_at: get(row, "occurred_at")?,
        stable_sort_id: get(row, "stable_sort_id")?,
    })
}

fn cost_from_row(row: &sqlx::postgres::PgRow) -> StoreResult<CurrencyCostTotal> {
    Ok(CurrencyCostTotal {
        currency: get(row, "cost_currency")?,
        amount: DecimalAmount::from_str(&get::<String>(row, "amount")?)?,
    })
}

fn optional_decimal(
    row: &sqlx::postgres::PgRow,
    column: &'static str,
) -> StoreResult<Option<DecimalAmount>> {
    get::<Option<String>>(row, column)?
        .map(|value| DecimalAmount::from_str(&value))
        .transpose()
}

fn validate_account_ids(account_ids: &[String]) -> StoreResult<()> {
    let mut unique = BTreeSet::new();
    for account_id in account_ids {
        validate_text(account_id, MAX_FILTER_BYTES, "provider account ID")?;
        if !unique.insert(account_id.as_str()) {
            return Err(invalid("account usage query contains duplicate IDs"));
        }
    }
    Ok(())
}

fn validate_optional_text(
    value: Option<&str>,
    max_bytes: usize,
    field: &'static str,
) -> StoreResult<()> {
    value.map_or(Ok(()), |value| validate_text(value, max_bytes, field))
}

fn validate_text(value: &str, max_bytes: usize, field: &'static str) -> StoreResult<()> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(invalid(&format!("{field} is invalid")));
    }
    Ok(())
}

fn get<'r, T>(row: &'r sqlx::postgres::PgRow, column: &'static str) -> StoreResult<T>
where
    T: sqlx::Decode<'r, Postgres> + sqlx::Type<Postgres>,
{
    row.try_get(column).map_err(|_| invalid(column))
}

fn unsigned(row: &sqlx::postgres::PgRow, column: &'static str) -> StoreResult<u64> {
    to_u64(get(row, column)?)
}

fn optional_unsigned(
    row: &sqlx::postgres::PgRow,
    column: &'static str,
) -> StoreResult<Option<u64>> {
    get::<Option<i64>>(row, column)?.map(to_u64).transpose()
}

fn optional_status(row: &sqlx::postgres::PgRow, column: &'static str) -> StoreResult<Option<u16>> {
    get::<Option<i32>>(row, column)?
        .map(|value| {
            u16::try_from(value)
                .ok()
                .filter(|value| (100..=599).contains(value))
                .ok_or_else(|| invalid("status code is outside its supported range"))
        })
        .transpose()
}

fn to_u64(value: i64) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| invalid("numeric observation is negative"))
}

fn to_u32(value: i32) -> StoreResult<u32> {
    u32::try_from(value).map_err(|_| invalid("integer observation is negative"))
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidData {
        entity: "observability",
        message: message.to_owned(),
    }
}
