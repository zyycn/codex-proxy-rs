//! 查询模型、校验与观测端口契约。

use super::*;

pub(crate) const MAX_PAGE_SIZE: u16 = 100;
pub(crate) const MAX_FILTER_BYTES: usize = 256;
pub(crate) const MAX_SEARCH_BYTES: usize = 512;
pub(crate) const MAX_ACCOUNT_IDS: usize = 200;
/// 概览卡只展示最近使用的四个账号；完整账号用量由账号管理页单独查询。
pub(crate) const DASHBOARD_ACCOUNT_LIMIT: u16 = 4;
pub(crate) const DIAGNOSTIC_LIMIT: i64 = 100;
pub(crate) const ACCOUNT_USAGE_TIMELINE_HOURS: i64 = 24;

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
    pub(crate) fn new(value: f64) -> StoreResult<Self> {
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

    pub(crate) const fn sql_interval(self) -> &'static str {
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

/// 已完整交付且由 Provider 计算费用的请求事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalculatedUsageBillingFact {
    pub bucket_start: DateTime<Utc>,
    pub provider_kind: String,
    pub upstream_model_id: String,
    pub service_tier: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub total: CurrencyCostTotal,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderAccountMetrics {
    pub total: u64,
    pub enabled: u64,
    pub unavailable: u64,
    pub active: u64,
    pub rate_limited: u64,
    pub expired: u64,
    pub quota_exhausted: u64,
    pub disabled: u64,
    pub banned: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountRequestBucket {
    pub bucket_start: DateTime<Utc>,
    pub request_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountUsageObservation {
    pub account_id: String,
    pub provider_kind: String,
    pub authentication_kind: String,
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
    pub request_buckets: Vec<ProviderAccountRequestBucket>,
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
    pub(crate) include_hourly_request_buckets: bool,
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
            include_hourly_request_buckets: false,
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
            include_hourly_request_buckets: false,
        })
    }

    pub fn with_hourly_request_buckets(mut self) -> StoreResult<Self> {
        if self.range.end.signed_duration_since(self.range.start)
            > TimeDelta::hours(ACCOUNT_USAGE_TIMELINE_HOURS)
        {
            return Err(invalid("account request timeline cannot exceed 24 hours"));
        }
        self.include_hourly_request_buckets = true;
        Ok(self)
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
    pub provider_kind: Option<String>,
    pub provider_account_ref: Option<String>,
    pub provider_account_name: Option<String>,
    pub provider_account_email: Option<String>,
    pub provider_account_authentication_kind: Option<String>,
    pub upstream_model_id: Option<String>,
    pub upstream_transport: Option<String>,
    pub http_version: Option<String>,
    pub websocket_pool: Option<String>,
    pub service_tier: Option<String>,
    pub provider_metadata_json: Option<String>,
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
    pub provider_account_name: Option<String>,
    pub provider_account_email: Option<String>,
    pub provider_account_authentication_kind: Option<String>,
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
    pub key: String,
    pub name: String,
    pub request_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub attempt_count: u64,
    pub total_tokens: u64,
    pub average_latency_ms: Option<u64>,
    pub latency_p95_ms: Option<u64>,
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
    pub endpoint: Option<String>,
    pub provider_kind: Option<String>,
    pub provider_account_ref: Option<String>,
    pub provider_account_name: Option<String>,
    pub provider_account_email: Option<String>,
    pub provider_account_authentication_kind: Option<String>,
    pub upstream_model_id: Option<String>,
    pub upstream_transport: Option<String>,
    pub failure_kind: String,
    pub client_status_code: Option<u16>,
    pub upstream_status_code: Option<u16>,
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
    async fn usage_calculated_billing_facts(
        &self,
        range: ObservabilityRange,
        filter: UsageRecordFilter,
    ) -> StoreResult<Vec<CalculatedUsageBillingFact>>;
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
