//! 查询模型、校验与观测端口契约

use super::*;

pub(crate) const MAX_FILTER_BYTES: usize = 256;
pub(crate) const MAX_SEARCH_BYTES: usize = 512;
pub(crate) const MAX_ACCOUNT_IDS: usize = 200;
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

pub use gateway_admin::model::observability::ObservabilityPageSize;

pub(crate) fn observability_page_offset(
    current_page: u32,
    page_size: ObservabilityPageSize,
) -> StoreResult<i64> {
    let page_index = current_page
        .checked_sub(1)
        .ok_or_else(|| invalid("current page must be positive"))?;
    u64::from(page_index)
        .checked_mul(u64::from(page_size.get()))
        .and_then(|offset| i64::try_from(offset).ok())
        .ok_or_else(|| invalid("page offset is too large"))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageRecordFilter {
    pub routing_group_refs: Vec<String>,
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
        validate_account_ids_if_present(&self.routing_group_refs)?;
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
    pub current_page: u32,
    pub page_size: ObservabilityPageSize,
}

pub use gateway_admin::model::observability::OpsErrorFilter;

pub(crate) fn validate_ops_error_filter(filter: &OpsErrorFilter) -> StoreResult<()> {
    if filter.routing_group_refs.len() > 32
        || filter.routing_group_refs.iter().any(|value| {
            value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
        })
    {
        return Err(invalid("invalid routing group filter"));
    }

    for (value, field) in [
        (
            filter.client_api_key_ref.as_deref(),
            "client API key filter",
        ),
        (filter.request_id.as_deref(), "request ID filter"),
        (
            filter.provider_account_ref.as_deref(),
            "provider account filter",
        ),
        (filter.provider_kind.as_deref(), "provider filter"),
        (filter.operation.as_deref(), "operation filter"),
        (filter.model.as_deref(), "model filter"),
        (filter.transport.as_deref(), "transport filter"),
        (
            filter.upstream_request_id.as_deref(),
            "upstream request ID filter",
        ),
    ] {
        validate_optional_text(value, MAX_FILTER_BYTES, field)?;
    }
    validate_optional_text(filter.search.as_deref(), MAX_SEARCH_BYTES, "search filter")?;
    if filter
        .status_code
        .is_some_and(|status| !(100..=599).contains(&status))
    {
        return Err(invalid("status code filter must be between 100 and 599"));
    }
    if filter
        .attempt_index
        .is_some_and(|index| index == 0 || i32::try_from(index).is_err())
    {
        return Err(invalid("attempt index filter is out of range"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsErrorQuery {
    pub range: ObservabilityRange,
    pub filter: OpsErrorFilter,
    pub current_page: u32,
    pub page_size: ObservabilityPageSize,
}

pub use gateway_admin::model::observability::DiagnosticDimension;

pub use gateway_admin::model::observability::CurrencyCost as CurrencyCostTotal;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CostCoverage {
    pub provider_reported_count: u64,
    pub calculated_count: u64,
    pub unavailable_count: u64,
}

pub use gateway_admin::model::observability::RequestMetrics;

pub use gateway_admin::model::observability::{LatencyPercentiles, PercentileMilliseconds};

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

pub use gateway_admin::model::observability::Granularity as ObservationGranularity;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestMetricPoint {
    pub bucket_start: DateTime<Utc>,
    pub granularity: ObservationGranularity,
    pub metrics: RequestMetrics,
    pub cost_coverage: CostCoverage,
    pub costs: Vec<CurrencyCostTotal>,
}

/// 已完整交付且由 Provider 计算费用的请求事实
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalculatedUsageBillingFact {
    pub billing_snapshot_json: Option<serde_json::Value>,
    pub bucket_start: DateTime<Utc>,
    pub provider_kind: String,
    pub upstream_model_id: String,
    pub service_tier: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub total: CurrencyCostTotal,
}

pub use gateway_admin::model::observability::AccountPoolMetrics as ProviderAccountMetrics;

pub use gateway_admin::model::observability::AccountRequestBucket as ProviderAccountRequestBucket;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountUsageObservation {
    pub account_id: String,
    pub provider_kind: String,
    pub authentication_kind: String,
    pub name: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub request_count: u64,
    pub success_count: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
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
    pub cached_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
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
    pub(crate) request_bucket_range: Option<ObservabilityRange>,
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
            request_bucket_range: None,
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
            request_bucket_range: None,
        })
    }

    pub fn with_hourly_request_buckets(mut self) -> StoreResult<Self> {
        // 小时图独立展示最近 24 个 UTC 小时桶，自然日统计不能限制为 24 小时
        let current_hour =
            DateTime::from_timestamp(self.range.end.timestamp().div_euclid(3600) * 3600, 0)
                .ok_or_else(|| invalid("account request timeline exceeds supported timestamps"))?;
        let start = current_hour
            .checked_sub_signed(TimeDelta::hours(ACCOUNT_USAGE_TIMELINE_HOURS - 1))
            .ok_or_else(|| invalid("account request timeline exceeds supported timestamps"))?;
        self.request_bucket_range = Some(ObservabilityRange {
            start,
            end: self.range.end,
        });
        Ok(self)
    }
}

pub use gateway_admin::model::observability::DashboardTotals;

#[derive(Debug, Clone, PartialEq)]
pub struct DashboardObservation {
    pub analytics: admin_observability::AnalyticsMetadata,
    pub range: ObservabilityRange,
    pub totals: DashboardTotals,
    pub provider_accounts: ProviderAccountMetrics,
    pub runtime_slots: admin_observability::DashboardRuntimeSlots,
    pub trend: Vec<RequestMetricPoint>,
    pub account_usage: Vec<ProviderAccountUsageObservation>,
    pub recent_requests: Vec<UsageListRecord>,
}

/// 使用记录列表所需的窄投影；完整执行、路由和客户端详情按 ID 单独读取
pub use gateway_admin::model::observability::UsageListRecord;

pub use gateway_admin::model::observability::UsageRecord;

pub use gateway_admin::model::observability::UsagePage as UsageRecordPage;

pub use gateway_admin::model::observability::UsageAttempt as UsageAttemptObservation;

pub use gateway_admin::model::observability::UsageDetail as UsageRecordDetail;

pub use gateway_admin::model::observability::ProviderObservation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageOverview {
    pub analytics: admin_observability::AnalyticsMetadata,
    pub range: ObservabilityRange,
    pub requests: RequestMetrics,
    pub attempts: AttemptMetrics,
    pub providers: Vec<ProviderObservation>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticsObservation {
    pub analytics: admin_observability::AnalyticsMetadata,
    pub total_request_count: u64,
    pub items: Vec<DiagnosticObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticObservation {
    pub key: String,
    pub name: String,
    pub account_provider_kind: Option<String>,
    pub account_plan_type: Option<String>,
    pub request_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub attempt_count: u64,
    pub total_tokens: u64,
    pub average_latency_ms: Option<u64>,
    pub latency_p95_ms: Option<u64>,
    pub first_token_p95_ms: Option<u64>,
    pub non_completion_count: u64,
    pub retry_count: u64,
    pub retried_request_count: u64,
    pub cost_coverage: CostCoverage,
    pub costs: Vec<CurrencyCostTotal>,
}

pub use gateway_admin::model::observability::OpsError as OpsErrorRecord;

pub use gateway_admin::model::observability::OpsErrorPage;

fn validate_account_ids_if_present(ids: &[String]) -> StoreResult<()> {
    if ids.is_empty() {
        return Ok(());
    }
    validate_account_ids(ids)
}
