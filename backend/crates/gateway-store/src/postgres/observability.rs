//! 从 `model_requests`、`ops_events` 与账号公共投影读取观测事实。

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    str::FromStr,
};

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use gateway_admin::{
    model::observability as admin_observability,
    ports::store::{AdminStoreResult, ObservabilityStore as AdminObservabilityStore},
};
use sqlx::{PgPool, Postgres, QueryBuilder, Row};

use crate::{
    DecimalAmount, StoreError, StoreResult, admin_store_error, postgres_unavailable,
    require_nonempty,
};

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
            (self.outcome.as_deref(), "outcome filter"),
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
    pub active: u64,
    pub expired: u64,
    pub quota_exhausted: u64,
    pub disabled: u64,
    pub banned: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountUsageObservation {
    pub account_id: String,
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
    pub image_input_tokens: Option<u64>,
    pub image_output_tokens: Option<u64>,
    pub image_request_count: u64,
    pub image_request_failed_count: u64,
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
    pub image_input_tokens: Option<u64>,
    pub image_output_tokens: Option<u64>,
    pub image_request_count: u64,
    pub image_request_failed_count: u64,
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
    pub provider_kind: Option<String>,
    pub provider_account_ref: Option<String>,
    pub provider_account_name: Option<String>,
    pub provider_account_email: Option<String>,
    pub upstream_model_id: Option<String>,
    pub upstream_transport: Option<String>,
    pub http_version: Option<String>,
    pub websocket_pool: Option<String>,
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
    pub image_input_tokens: Option<u64>,
    pub image_output_tokens: Option<u64>,
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
    pub image_generation_requested: bool,
    pub image_generation_succeeded: Option<bool>,
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

/// `gateway-admin` 观测端口的 PostgreSQL adapter。
///
/// SQL 查询与内部投影继续由 [`PgObservabilityRepository`] 唯一拥有；本类型只负责
/// Admin UTC 领域模型与持久化投影之间的无格式化转换。
#[derive(Clone)]
pub struct PgAdminObservabilityStore {
    repository: PgObservabilityRepository,
}

impl PgAdminObservabilityStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            repository: PgObservabilityRepository::new(pool),
        }
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
        let provider_accounts = provider_account_metrics(&self.pool, range.end).await?;
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

#[async_trait]
impl AdminObservabilityStore for PgAdminObservabilityStore {
    async fn dashboard_summary(
        &self,
        range: admin_observability::TimeRange,
    ) -> AdminStoreResult<admin_observability::DashboardObservation> {
        let observation = self
            .repository
            .dashboard_summary(store_range(range)?)
            .await
            .map_err(observability_error)?;
        admin_dashboard_observation(observation)
    }

    async fn dashboard_trend(
        &self,
        range: admin_observability::TimeRange,
    ) -> AdminStoreResult<Vec<admin_observability::RequestMetricPoint>> {
        self.repository
            .dashboard_trend(store_range(range)?)
            .await
            .map_err(observability_error)?
            .into_iter()
            .map(admin_request_metric_point)
            .collect()
    }

    async fn usage_trend(
        &self,
        range: admin_observability::TimeRange,
        filter: admin_observability::UsageFilter,
    ) -> AdminStoreResult<Vec<admin_observability::RequestMetricPoint>> {
        self.repository
            .usage_trend(store_range(range)?, store_usage_filter(filter))
            .await
            .map_err(observability_error)?
            .into_iter()
            .map(admin_request_metric_point)
            .collect()
    }

    async fn list_usage_records(
        &self,
        query: admin_observability::UsageQuery,
    ) -> AdminStoreResult<admin_observability::UsagePage> {
        let page = self
            .repository
            .list_usage_records(store_usage_query(query)?)
            .await
            .map_err(observability_error)?;
        admin_usage_page(page)
    }

    async fn usage_record_detail(
        &self,
        request_id: &str,
    ) -> AdminStoreResult<admin_observability::UsageDetail> {
        let detail = self
            .repository
            .usage_record_detail(request_id)
            .await
            .map_err(observability_error)?;
        admin_usage_detail(detail)
    }

    async fn usage_summary(
        &self,
        range: admin_observability::TimeRange,
        filter: admin_observability::UsageFilter,
    ) -> AdminStoreResult<admin_observability::UsageOverview> {
        let overview = self
            .repository
            .usage_summary(store_range(range)?, store_usage_filter(filter))
            .await
            .map_err(observability_error)?;
        admin_usage_overview(overview)
    }

    async fn usage_diagnostics(
        &self,
        range: admin_observability::TimeRange,
        filter: admin_observability::UsageFilter,
        dimension: admin_observability::DiagnosticDimension,
    ) -> AdminStoreResult<Vec<admin_observability::DiagnosticObservation>> {
        self.repository
            .usage_diagnostics(
                store_range(range)?,
                store_usage_filter(filter),
                store_diagnostic_dimension(dimension),
            )
            .await
            .map_err(observability_error)?
            .into_iter()
            .map(admin_diagnostic_observation)
            .collect()
    }

    async fn list_ops_errors(
        &self,
        query: admin_observability::OpsErrorQuery,
    ) -> AdminStoreResult<admin_observability::OpsErrorPage> {
        let page = self
            .repository
            .list_ops_errors(store_ops_error_query(query)?)
            .await
            .map_err(observability_error)?;
        admin_ops_error_page(page)
    }
}

fn store_range(range: admin_observability::TimeRange) -> AdminStoreResult<ObservabilityRange> {
    ObservabilityRange::new(range.start, range.end).map_err(observability_error)
}

fn store_usage_filter(filter: admin_observability::UsageFilter) -> UsageRecordFilter {
    UsageRecordFilter {
        client_api_key_ref: filter.client_api_key_ref,
        request_id: filter.request_id,
        provider_account_ref: filter.provider_account_ref,
        operation: filter.operation,
        provider_kind: filter.provider_kind,
        model: filter.model,
        outcome: filter.outcome.map(store_request_outcome),
        status_code: filter.status_code,
        transport: filter.transport,
        attempt_index: filter.attempt_index,
        response_id: filter.response_id,
        upstream_request_id: filter.upstream_request_id,
        search: filter.search,
    }
}

fn store_request_outcome(outcome: admin_observability::RequestOutcome) -> String {
    outcome.as_str().to_owned()
}

fn store_cursor(
    cursor: admin_observability::ObservabilityCursor,
) -> AdminStoreResult<ObservabilityCursor> {
    ObservabilityCursor::new(cursor.observed_at, cursor.stable_id).map_err(observability_error)
}

fn store_usage_query(query: admin_observability::UsageQuery) -> AdminStoreResult<UsageRecordQuery> {
    Ok(UsageRecordQuery {
        range: store_range(query.range)?,
        filter: store_usage_filter(query.filter),
        cursor: query.cursor.map(store_cursor).transpose()?,
        page: ObservabilityPageNumber::new(query.page.get()).map_err(observability_error)?,
        page_size: ObservabilityPageSize::new(query.page_size.get())
            .map_err(observability_error)?,
    })
}

fn store_ops_error_filter(filter: admin_observability::OpsErrorFilter) -> OpsErrorFilter {
    OpsErrorFilter {
        client_api_key_ref: filter.client_api_key_ref,
        request_id: filter.request_id,
        provider_account_ref: filter.provider_account_ref,
        provider_kind: filter.provider_kind,
        operation: filter.operation,
        model: filter.model,
        transport: filter.transport,
        attempt_index: filter.attempt_index,
        response_id: filter.response_id,
        upstream_request_id: filter.upstream_request_id,
        failure_kind: filter.failure_kind,
        status_code: filter.status_code,
        search: filter.search,
    }
}

fn store_ops_error_query(
    query: admin_observability::OpsErrorQuery,
) -> AdminStoreResult<OpsErrorQuery> {
    Ok(OpsErrorQuery {
        range: store_range(query.range)?,
        filter: store_ops_error_filter(query.filter),
        cursor: query.cursor.map(store_cursor).transpose()?,
        page: ObservabilityPageNumber::new(query.page.get()).map_err(observability_error)?,
        page_size: ObservabilityPageSize::new(query.page_size.get())
            .map_err(observability_error)?,
    })
}

const fn store_diagnostic_dimension(
    dimension: admin_observability::DiagnosticDimension,
) -> DiagnosticDimension {
    match dimension {
        admin_observability::DiagnosticDimension::Provider => DiagnosticDimension::Provider,
        admin_observability::DiagnosticDimension::Model => DiagnosticDimension::Model,
        admin_observability::DiagnosticDimension::Account => DiagnosticDimension::Account,
        admin_observability::DiagnosticDimension::ApiKey => DiagnosticDimension::ApiKey,
        admin_observability::DiagnosticDimension::Transport => DiagnosticDimension::Transport,
        admin_observability::DiagnosticDimension::Failure => DiagnosticDimension::Failure,
        admin_observability::DiagnosticDimension::Status => DiagnosticDimension::Status,
    }
}

fn admin_dashboard_observation(
    observation: DashboardObservation,
) -> AdminStoreResult<admin_observability::DashboardObservation> {
    let DashboardObservation {
        range,
        requests,
        attempts,
        provider_accounts,
        trend,
        account_usage,
        recent_requests,
    } = observation;
    Ok(admin_observability::DashboardObservation {
        range: admin_range(range),
        requests: admin_request_metrics(requests)?,
        attempts: admin_attempt_metrics(attempts)?,
        provider_accounts: admin_account_pool_metrics(provider_accounts),
        trend: trend
            .into_iter()
            .map(admin_request_metric_point)
            .collect::<AdminStoreResult<_>>()?,
        account_usage: account_usage
            .into_iter()
            .map(admin_dashboard_account_usage)
            .collect::<AdminStoreResult<_>>()?,
        recent_requests: recent_requests
            .into_iter()
            .map(admin_usage_record)
            .collect::<AdminStoreResult<_>>()?,
    })
}

const fn admin_range(range: ObservabilityRange) -> admin_observability::TimeRange {
    admin_observability::TimeRange {
        start: range.start,
        end: range.end,
    }
}

fn admin_request_metrics(
    metrics: RequestMetrics,
) -> AdminStoreResult<admin_observability::RequestMetrics> {
    Ok(admin_observability::RequestMetrics {
        request_count: metrics.request_count,
        success_count: metrics.success_count,
        failure_count: metrics.failure_count,
        cancelled_count: metrics.cancelled_count,
        incomplete_count: metrics.incomplete_count,
        caller_error_count: metrics.caller_error_count,
        input_tokens: metrics.input_tokens,
        output_tokens: metrics.output_tokens,
        cached_tokens: metrics.cached_tokens,
        cache_write_tokens: metrics.cache_write_tokens,
        reasoning_tokens: metrics.reasoning_tokens,
        total_tokens: metrics.total_tokens,
        first_token_latency_sum_ms: metrics.first_token_latency_sum,
        first_token_latency_count: metrics.first_token_latency_count,
        latency_sum_ms: metrics.latency_sum,
        latency_count: metrics.latency_count,
        min_latency_ms: metrics.min_latency_ms,
        max_latency_ms: metrics.max_latency_ms,
        latency_percentiles: admin_latency_percentiles(metrics.latency_percentiles)?,
        first_token_latency_percentiles: admin_latency_percentiles(
            metrics.first_token_latency_percentiles,
        )?,
        cache_eligible_request_count: metrics.cache_eligible_request_count,
        cache_hit_request_count: metrics.cache_hit_request_count,
    })
}

fn admin_latency_percentiles(
    percentiles: LatencyPercentiles,
) -> AdminStoreResult<admin_observability::LatencyPercentiles> {
    Ok(admin_observability::LatencyPercentiles {
        p50_ms: percentiles
            .p50_ms
            .map(|value| admin_observability::PercentileMilliseconds::new(value.as_f64()))
            .transpose()
            .map_err(|_| invalid_admin_percentile())?,
        p95_ms: percentiles
            .p95_ms
            .map(|value| admin_observability::PercentileMilliseconds::new(value.as_f64()))
            .transpose()
            .map_err(|_| invalid_admin_percentile())?,
        p99_ms: percentiles
            .p99_ms
            .map(|value| admin_observability::PercentileMilliseconds::new(value.as_f64()))
            .transpose()
            .map_err(|_| invalid_admin_percentile())?,
    })
}

fn invalid_admin_percentile() -> gateway_admin::ports::store::AdminStoreError {
    observability_error(StoreError::InvalidData {
        entity: "observability latency percentile",
        message: "latency percentile is outside the admin contract".to_owned(),
    })
}

fn admin_attempt_metrics(
    metrics: AttemptMetrics,
) -> AdminStoreResult<admin_observability::AttemptMetrics> {
    Ok(admin_observability::AttemptMetrics {
        attempt_count: metrics.attempt_count,
        success_count: metrics.success_count,
        failure_count: metrics.failure_count,
        cancelled_count: metrics.cancelled_count,
        incomplete_count: metrics.incomplete_count,
        rate_limited_count: metrics.rate_limited_count,
        auth_failure_count: metrics.auth_failure_count,
        provider_5xx_count: metrics.provider_5xx_count,
        cost_coverage: admin_cost_coverage(metrics.cost_coverage),
        costs: admin_currency_costs(metrics.costs)?,
    })
}

const fn admin_cost_coverage(coverage: CostCoverage) -> admin_observability::CostCoverage {
    admin_observability::CostCoverage {
        provider_reported_count: coverage.provider_reported_count,
        calculated_count: coverage.calculated_count,
        partial_count: 0,
        unavailable_count: coverage.unavailable_count,
        not_billable_count: 0,
    }
}

fn admin_currency_costs(
    costs: Vec<CurrencyCostTotal>,
) -> AdminStoreResult<Vec<admin_observability::CurrencyCost>> {
    costs
        .into_iter()
        .map(|cost| {
            Ok(admin_observability::CurrencyCost {
                currency: cost.currency,
                amount: admin_decimal_amount(cost.amount)?,
            })
        })
        .collect()
}

fn admin_decimal_amount(
    amount: DecimalAmount,
) -> AdminStoreResult<admin_observability::DecimalAmount> {
    admin_observability::DecimalAmount::from_str(amount.as_str()).map_err(|_| {
        observability_error(StoreError::InvalidData {
            entity: "observability decimal amount",
            message: "amount is outside the admin numeric contract".to_owned(),
        })
    })
}

fn admin_optional_decimal_amount(
    amount: Option<DecimalAmount>,
) -> AdminStoreResult<Option<admin_observability::DecimalAmount>> {
    amount.map(admin_decimal_amount).transpose()
}

const fn admin_account_pool_metrics(
    metrics: ProviderAccountMetrics,
) -> admin_observability::AccountPoolMetrics {
    admin_observability::AccountPoolMetrics {
        total: metrics.total,
        enabled: metrics.enabled,
        unavailable: metrics.unavailable,
        active: metrics.active,
        expired: metrics.expired,
        quota_exhausted: metrics.quota_exhausted,
        refreshing: None,
        disabled: metrics.disabled,
        banned: metrics.banned,
    }
}

fn admin_dashboard_account_usage(
    usage: ProviderAccountUsageObservation,
) -> AdminStoreResult<admin_observability::DashboardAccountUsage> {
    Ok(admin_observability::DashboardAccountUsage {
        account_id: usage.account_id,
        provider_kind: usage.provider_kind,
        name: usage.name,
        email: usage.email,
        plan_type: usage.plan_type,
        enabled: usage.enabled,
        availability: usage.availability,
        request_count: usage.request_count,
        success_count: usage.success_count,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_tokens: usage.cached_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        image_input_tokens: usage.image_input_tokens,
        image_output_tokens: usage.image_output_tokens,
        image_request_count: usage.image_request_count,
        image_request_failed_count: usage.image_request_failed_count,
        total_tokens: usage.total_tokens,
        cost_coverage: admin_cost_coverage(usage.cost_coverage),
        costs: admin_currency_costs(usage.costs)?,
        last_used_at: usage.last_used_at,
        models: usage
            .models
            .into_iter()
            .map(admin_dashboard_account_model_usage)
            .collect::<AdminStoreResult<_>>()?,
    })
}

fn admin_dashboard_account_model_usage(
    usage: ProviderAccountModelUsageObservation,
) -> AdminStoreResult<admin_observability::DashboardAccountModelUsage> {
    Ok(admin_observability::DashboardAccountModelUsage {
        model: usage.model,
        request_count: usage.request_count,
        success_count: usage.success_count,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_tokens: usage.cached_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        image_input_tokens: usage.image_input_tokens,
        image_output_tokens: usage.image_output_tokens,
        image_request_count: usage.image_request_count,
        image_request_failed_count: usage.image_request_failed_count,
        total_tokens: usage.total_tokens,
        cost_coverage: admin_cost_coverage(usage.cost_coverage),
        costs: admin_currency_costs(usage.costs)?,
        last_used_at: usage.last_used_at,
    })
}

fn admin_request_metric_point(
    point: RequestMetricPoint,
) -> AdminStoreResult<admin_observability::RequestMetricPoint> {
    Ok(admin_observability::RequestMetricPoint {
        bucket_start: point.bucket_start,
        granularity: admin_granularity(point.granularity),
        metrics: admin_request_metrics(point.metrics)?,
        cost_coverage: admin_cost_coverage(point.cost_coverage),
        costs: admin_currency_costs(point.costs)?,
    })
}

const fn admin_granularity(
    granularity: ObservationGranularity,
) -> admin_observability::Granularity {
    match granularity {
        ObservationGranularity::FifteenMinutes => admin_observability::Granularity::FifteenMinutes,
        ObservationGranularity::Hour => admin_observability::Granularity::Hour,
        ObservationGranularity::Day => admin_observability::Granularity::Day,
    }
}

fn admin_usage_page(page: UsageRecordPage) -> AdminStoreResult<admin_observability::UsagePage> {
    Ok(admin_observability::UsagePage {
        items: page
            .items
            .into_iter()
            .map(admin_usage_record)
            .collect::<AdminStoreResult<_>>()?,
        total: page.total,
        next_cursor: page.next_cursor.map(admin_cursor),
    })
}

fn admin_cursor(cursor: ObservabilityCursor) -> admin_observability::ObservabilityCursor {
    admin_observability::ObservabilityCursor {
        observed_at: cursor.observed_at,
        stable_id: cursor.stable_id,
    }
}

fn admin_usage_record(record: UsageRecord) -> AdminStoreResult<admin_observability::UsageRecord> {
    let billing = match (&record.cost_amount, &record.cost_currency) {
        (Some(amount), Some(currency)) => Some(admin_observability::UsageBilling::Total {
            source: record.cost_source.clone(),
            total: admin_observability::CurrencyCost {
                currency: currency.clone(),
                amount: admin_decimal_amount(amount.clone())?,
            },
        }),
        (None, None) => None,
        _ => {
            return Err(observability_error(StoreError::InvalidData {
                entity: "observability request billing",
                message: "cost amount and currency must be present together".to_owned(),
            }));
        }
    };
    Ok(admin_observability::UsageRecord {
        id: record.id,
        client_api_key_ref: record.client_api_key_ref,
        config_revision: record.config_revision,
        protocol: record.protocol,
        operation: record.operation,
        endpoint: record.endpoint,
        client_transport: record.client_transport,
        requested_model_id: record.requested_model_id,
        input_token_estimate: record.input_token_estimate,
        provider_kind: record.provider_kind,
        provider_account_ref: record.provider_account_ref,
        provider_account_name: record.provider_account_name,
        provider_account_email: record.provider_account_email,
        upstream_model_id: record.upstream_model_id,
        upstream_transport: record.upstream_transport,
        http_version: record.http_version,
        websocket_pool: record.websocket_pool,
        attempt_count: record.attempt_count,
        upstream_send_state: record.upstream_send_state,
        downstream_committed_at: record.downstream_committed_at,
        outcome: admin_request_outcome(&record.outcome)?,
        client_status_code: record.client_status_code,
        upstream_status_code: record.upstream_status_code,
        client_response_id: record.client_response_id,
        upstream_request_id: record.upstream_request_id,
        upstream_response_id: record.upstream_response_id,
        error_kind: record.error_kind,
        provider_error_code: record.provider_error_code,
        error_message: record.error_message,
        retry_after_ms: record.retry_after_ms,
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        cached_tokens: record.cached_tokens,
        cache_write_tokens: record.cache_write_tokens,
        reasoning_tokens: record.reasoning_tokens,
        image_input_tokens: record.image_input_tokens,
        image_output_tokens: record.image_output_tokens,
        total_tokens: record.total_tokens,
        cost_source: record.cost_source,
        cost_amount: admin_optional_decimal_amount(record.cost_amount)?,
        cost_currency: record.cost_currency,
        billing,
        transport_decision_wait_ms: record.transport_decision_wait_ms,
        connect_ms: record.connect_ms,
        headers_ms: record.headers_ms,
        first_event_ms: record.first_event_ms,
        first_reasoning_ms: record.first_reasoning_ms,
        first_text_ms: record.first_text_ms,
        first_token_ms: record.first_token_ms,
        provider_processing_ms: record.provider_processing_ms,
        latency_ms: record.latency_ms,
        client_ip: record.client_ip,
        user_agent: record.user_agent,
        reasoning_effort: record.reasoning_effort,
        reasoning_preset: record.reasoning_preset,
        request_kind: record.request_kind,
        subagent_kind: record.subagent_kind,
        compact: record.compact,
        image_generation_requested: record.image_generation_requested,
        image_generation_succeeded: record.image_generation_succeeded,
        started_at: record.started_at,
        deadline_at: record.deadline_at,
        completed_at: record.completed_at,
    })
}

fn admin_request_outcome(outcome: &str) -> AdminStoreResult<admin_observability::RequestOutcome> {
    admin_observability::RequestOutcome::new(outcome.to_owned()).map_err(|_| {
        observability_error(StoreError::InvalidData {
            entity: "observability request outcome",
            message: "invalid request outcome".to_owned(),
        })
    })
}

fn admin_usage_detail(
    detail: UsageRecordDetail,
) -> AdminStoreResult<admin_observability::UsageDetail> {
    Ok(admin_observability::UsageDetail {
        request: admin_usage_record(detail.request)?,
        attempts: detail
            .attempts
            .into_iter()
            .map(admin_usage_attempt)
            .collect::<AdminStoreResult<_>>()?,
    })
}

fn admin_usage_attempt(
    attempt: UsageAttemptObservation,
) -> AdminStoreResult<admin_observability::UsageAttempt> {
    Ok(admin_observability::UsageAttempt {
        source: attempt.source,
        id: attempt.id,
        attempt_index: attempt.attempt_index,
        component: attempt.component,
        operation: attempt.operation,
        provider_kind: attempt.provider_kind,
        provider_account_ref: attempt.provider_account_ref,
        upstream_model_id: attempt.upstream_model_id,
        upstream_transport: attempt.upstream_transport,
        upstream_send_state: attempt.upstream_send_state,
        outcome: admin_request_outcome(&attempt.outcome)?,
        downstream_committed: attempt.downstream_committed,
        status_code: attempt.status_code,
        provider_error_code: attempt.provider_error_code,
        failure_kind: attempt.failure_kind,
        retry_after_ms: attempt.retry_after_ms,
        upstream_request_id: attempt.upstream_request_id,
        latency_ms: attempt.latency_ms,
        message: attempt.message,
        input_tokens: attempt.input_tokens,
        output_tokens: attempt.output_tokens,
        cached_tokens: attempt.cached_tokens,
        cache_write_tokens: attempt.cache_write_tokens,
        reasoning_tokens: attempt.reasoning_tokens,
        total_tokens: attempt.total_tokens,
        cost_source: attempt.cost_source,
        cost_amount: admin_optional_decimal_amount(attempt.cost_amount)?,
        cost_currency: attempt.cost_currency,
        occurred_at: attempt.occurred_at,
    })
}

fn admin_usage_overview(
    overview: UsageOverview,
) -> AdminStoreResult<admin_observability::UsageOverview> {
    Ok(admin_observability::UsageOverview {
        range: admin_range(overview.range),
        requests: admin_request_metrics(overview.requests)?,
        attempts: admin_attempt_metrics(overview.attempts)?,
        providers: overview
            .providers
            .into_iter()
            .map(admin_provider_observation)
            .collect(),
    })
}

fn admin_provider_observation(
    observation: ProviderObservation,
) -> admin_observability::ProviderObservation {
    admin_observability::ProviderObservation {
        provider_kind: observation.provider_kind,
        request_count: observation.request_count,
        attempt_count: observation.attempt_count,
        failure_count: observation.failure_count,
        total_tokens: observation.total_tokens,
    }
}

fn admin_diagnostic_observation(
    observation: DiagnosticObservation,
) -> AdminStoreResult<admin_observability::DiagnosticObservation> {
    Ok(admin_observability::DiagnosticObservation {
        name: observation.name,
        request_count: observation.request_count,
        success_count: observation.success_count,
        failure_count: observation.failure_count,
        attempt_count: observation.attempt_count,
        total_tokens: observation.total_tokens,
        average_latency_ms: observation.average_latency_ms,
        cost_coverage: admin_cost_coverage(observation.cost_coverage),
        costs: admin_currency_costs(observation.costs)?,
    })
}

fn admin_ops_error_page(page: OpsErrorPage) -> AdminStoreResult<admin_observability::OpsErrorPage> {
    Ok(admin_observability::OpsErrorPage {
        items: page.items.into_iter().map(admin_ops_error).collect(),
        total: page.total,
        next_cursor: page.next_cursor.map(admin_cursor),
    })
}

fn admin_ops_error(error: OpsErrorRecord) -> admin_observability::OpsError {
    admin_observability::OpsError {
        source: error.source,
        event_id: error.event_id,
        request_id: error.request_id,
        attempt_index: error.attempt_index,
        client_api_key_ref: error.client_api_key_ref,
        component: error.component,
        operation: error.operation,
        provider_kind: error.provider_kind,
        provider_account_ref: error.provider_account_ref,
        upstream_model_id: error.upstream_model_id,
        upstream_transport: error.upstream_transport,
        failure_kind: error.failure_kind,
        status_code: error.status_code,
        provider_error_code: error.provider_error_code,
        client_response_id: error.client_response_id,
        upstream_request_id: error.upstream_request_id,
        latency_ms: error.latency_ms,
        message: error.message,
        occurrence_count: error.occurrence_count,
        occurred_at: error.occurred_at,
        stable_sort_id: error.stable_sort_id,
    }
}

fn observability_error(error: StoreError) -> gateway_admin::ports::store::AdminStoreError {
    admin_store_error("observability", error)
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

async fn provider_account_metrics(
    pool: &PgPool,
    observed_at: DateTime<Utc>,
) -> StoreResult<ProviderAccountMetrics> {
    let row = sqlx::query(
        "select count(*)::bigint as total,
                count(*) filter (where enabled)::bigint as enabled,
                count(*) filter (where not enabled or availability <> 'ready')::bigint
                  as unavailable,
                count(*) filter (
                  where enabled and availability = 'ready' and access_token_expires_at > $1
                )::bigint as active,
                count(*) filter (
                  where availability = 'expired' or access_token_expires_at <= $1
                )::bigint as expired,
                count(*) filter (where availability = 'quota_exhausted')::bigint
                  as quota_exhausted,
                count(*) filter (where not enabled)::bigint as disabled,
                count(*) filter (where availability = 'banned')::bigint as banned
         from provider_accounts",
    )
    .bind(observed_at)
    .fetch_one(pool)
    .await
    .map_err(|_| postgres_unavailable("load provider account metrics"))?;
    Ok(ProviderAccountMetrics {
        total: unsigned(&row, "total")?,
        enabled: unsigned(&row, "enabled")?,
        unavailable: unsigned(&row, "unavailable")?,
        active: unsigned(&row, "active")?,
        expired: unsigned(&row, "expired")?,
        quota_exhausted: unsigned(&row, "quota_exhausted")?,
        disabled: unsigned(&row, "disabled")?,
        banned: unsigned(&row, "banned")?,
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
        "select pa.id, pa.provider_kind, pa.name, pa.email,
                pa.plan_type, pa.enabled, pa.availability,
                count(mr.id)::bigint as request_count,
                count(mr.id) filter (where mr.outcome = 'succeeded')::bigint as success_count,
                sum(mr.input_tokens)::bigint as input_tokens,
                sum(mr.output_tokens)::bigint as output_tokens,
                sum(mr.cached_tokens)::bigint as cached_tokens,
                sum(mr.cache_write_tokens)::bigint as cache_write_tokens,
                sum(mr.reasoning_tokens)::bigint as reasoning_tokens,
                sum(mr.image_input_tokens)::bigint as image_input_tokens,
                sum(mr.image_output_tokens)::bigint as image_output_tokens,
                count(mr.id) filter (where mr.image_generation_succeeded is true)::bigint
                  as image_request_count,
                count(mr.id) filter (where mr.image_generation_succeeded is false)::bigint
                  as image_request_failed_count,
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
        " group by pa.id, pa.provider_kind, pa.name, pa.email,
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
                image_input_tokens: optional_unsigned(&row, "image_input_tokens")?,
                image_output_tokens: optional_unsigned(&row, "image_output_tokens")?,
                image_request_count: unsigned(&row, "image_request_count")?,
                image_request_failed_count: unsigned(&row, "image_request_failed_count")?,
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
                sum(image_input_tokens)::bigint as image_input_tokens,
                sum(image_output_tokens)::bigint as image_output_tokens,
                count(*) filter (where image_generation_succeeded is true)::bigint
                  as image_request_count,
                count(*) filter (where image_generation_succeeded is false)::bigint
                  as image_request_failed_count,
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
        image_input_tokens: optional_unsigned(row, "image_input_tokens")?,
        image_output_tokens: optional_unsigned(row, "image_output_tokens")?,
        image_request_count: unsigned(row, "image_request_count")?,
        image_request_failed_count: unsigned(row, "image_request_failed_count")?,
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
            mr.input_token_estimate, mr.provider_kind, mr.provider_account_ref,
            pa.name as provider_account_name, pa.email as provider_account_email,
            mr.upstream_model_id, mr.upstream_transport, mr.http_version, mr.websocket_pool,
            mr.attempt_count, mr.upstream_send_state, mr.downstream_committed_at,
            mr.outcome, mr.client_status_code, mr.upstream_status_code,
            mr.client_response_id, mr.upstream_request_id, mr.upstream_response_id,
            mr.error_kind, mr.provider_error_code, mr.error_message, mr.retry_after_ms,
            mr.input_tokens, mr.output_tokens, mr.cached_tokens, mr.cache_write_tokens,
            mr.reasoning_tokens, mr.image_input_tokens, mr.image_output_tokens,
            mr.total_tokens, mr.cost_source, mr.cost_amount::text,
            mr.cost_currency, mr.transport_decision_wait_ms, mr.connect_ms, mr.headers_ms,
            mr.first_event_ms, mr.first_reasoning_ms, mr.first_text_ms, mr.first_token_ms,
            mr.provider_processing_ms, mr.latency_ms, mr.client_ip::text as client_ip,
            mr.user_agent, mr.reasoning_effort, mr.reasoning_preset, mr.request_kind,
            mr.subagent_kind, mr.compact, mr.image_generation_requested,
            mr.image_generation_succeeded, mr.started_at, mr.deadline_at, mr.completed_at
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
                provider_kind, provider_account_ref, upstream_model_id,
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
              mr.provider_kind,
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
              oe.provider_kind, oe.provider_account_ref,
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
                 component, operation, provider_kind, provider_account_ref,
                 upstream_model_id, upstream_transport,
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
        provider_kind: get(row, "provider_kind")?,
        provider_account_ref: get(row, "provider_account_ref")?,
        provider_account_name: get(row, "provider_account_name")?,
        provider_account_email: get(row, "provider_account_email")?,
        upstream_model_id: get(row, "upstream_model_id")?,
        upstream_transport: get(row, "upstream_transport")?,
        http_version: get(row, "http_version")?,
        websocket_pool: get(row, "websocket_pool")?,
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
        image_input_tokens: optional_unsigned(row, "image_input_tokens")?,
        image_output_tokens: optional_unsigned(row, "image_output_tokens")?,
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
        image_generation_requested: get(row, "image_generation_requested")?,
        image_generation_succeeded: get(row, "image_generation_succeeded")?,
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
