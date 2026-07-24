//! 用量、成本、健康与错误诊断的 UTC 语义事实。

use std::{num::NonZeroU32, str::FromStr};

use chrono::{DateTime, TimeDelta, Utc};

use super::{AdminModelError, PageSize};

/// 外部观测查询的 UTC 时间范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl TimeRange {
    /// 创建最长 366 天的正时间范围。
    ///
    /// # Errors
    ///
    /// 范围为空、反向或超过 366 天时返回错误。
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Self, AdminModelError> {
        let duration = end.signed_duration_since(start);
        if duration <= TimeDelta::zero() || duration > TimeDelta::days(366) {
            return Err(AdminModelError::InvalidTimeRange);
        }
        Ok(Self { start, end })
    }
}

/// 从一开始的观测页码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageNumber(NonZeroU32);

impl PageNumber {
    #[must_use]
    pub const fn new(value: NonZeroU32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// 观测记录的稳定键集游标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservabilityCursor {
    pub observed_at: DateTime<Utc>,
    pub stable_id: String,
}

/// 请求结果状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestOutcome {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Incomplete,
    Other(OtherRequestOutcome),
}

/// 未知但有界的请求结果值；私有字段禁止绕过 [`RequestOutcome::new`] 构造。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherRequestOutcome(String);

impl RequestOutcome {
    pub const MAX_BYTES: usize = 256;

    /// 从持久化或 wire 字符串创建可扩展结果语义。
    ///
    /// # Errors
    ///
    /// 空值、超过 256 字节或含控制字符时返回
    /// [`AdminModelError::InvalidRequestOutcome`]。
    pub fn new(value: impl Into<String>) -> Result<Self, AdminModelError> {
        let value = value.into();
        match value.as_str() {
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "incomplete" => Ok(Self::Incomplete),
            _ if value.trim().is_empty()
                || value.len() > Self::MAX_BYTES
                || value.chars().any(char::is_control) =>
            {
                Err(AdminModelError::InvalidRequestOutcome)
            }
            _ => Ok(Self::Other(OtherRequestOutcome(value))),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Incomplete => "incomplete",
            Self::Other(value) => value.as_str(),
        }
    }
}

impl OtherRequestOutcome {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 用量记录过滤条件。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageFilter {
    pub client_api_key_ref: Option<String>,
    pub request_id: Option<String>,
    pub provider_account_ref: Option<String>,
    pub operation: Option<String>,
    pub provider_kind: Option<String>,
    pub model: Option<String>,
    pub outcome: Option<RequestOutcome>,
    pub status_code: Option<u16>,
    pub transport: Option<String>,
    pub attempt_index: Option<u32>,
    pub response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub search: Option<String>,
}

/// 用量记录分页查询。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageQuery {
    pub range: TimeRange,
    pub filter: UsageFilter,
    pub cursor: Option<ObservabilityCursor>,
    pub page: PageNumber,
    pub page_size: PageSize,
}

/// 运维错误过滤条件。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpsErrorFilter {
    pub client_api_key_ref: Option<String>,
    pub request_id: Option<String>,
    pub provider_kind: Option<String>,
    pub provider_account_ref: Option<String>,
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

/// 运维错误分页查询。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsErrorQuery {
    pub range: TimeRange,
    pub filter: OpsErrorFilter,
    pub cursor: Option<ObservabilityCursor>,
    pub page: PageNumber,
    pub page_size: PageSize,
}

/// 用量诊断维度。
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

/// `numeric(20,10)` 的非负规范金额。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DecimalAmount(String);

impl DecimalAmount {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 精确相加两个 `numeric(20,10)` 金额。
    #[must_use]
    pub fn checked_add(&self, other: &Self) -> Option<Self> {
        let sum = scaled_amount(&self.0)?.checked_add(scaled_amount(&other.0)?)?;
        Self::from_scaled(sum)
    }

    /// 将金额按非零请求数均分，保留最多十位小数。
    #[must_use]
    pub fn checked_div_u64(&self, divisor: u64) -> Option<Self> {
        let divisor = u128::from(divisor);
        (divisor != 0)
            .then(|| scaled_amount(&self.0)?.checked_div(divisor))
            .flatten()
            .and_then(Self::from_scaled)
    }

    fn from_scaled(value: u128) -> Option<Self> {
        if value >= 10_u128.pow(20) {
            return None;
        }
        let whole = value / 10_u128.pow(10);
        let fraction = value % 10_u128.pow(10);
        let value = if fraction == 0 {
            whole.to_string()
        } else {
            format!("{whole}.{fraction:010}")
                .trim_end_matches('0')
                .to_owned()
        };
        Some(Self(value))
    }
}

fn scaled_amount(value: &str) -> Option<u128> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let whole = whole.parse::<u128>().ok()?;
    let fraction = format!("{fraction:0<10}").parse::<u128>().ok()?;
    whole.checked_mul(10_u128.pow(10))?.checked_add(fraction)
}

impl FromStr for DecimalAmount {
    type Err = AdminModelError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        let mut parts = input.split('.');
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next();
        let valid = !whole.is_empty()
            && whole.len() <= 10
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && parts.next().is_none()
            && fraction.is_none_or(|value| {
                !value.is_empty()
                    && value.len() <= 10
                    && value.bytes().all(|byte| byte.is_ascii_digit())
            });
        if !valid {
            return Err(AdminModelError::InvalidDecimalAmount);
        }
        let whole = whole.trim_start_matches('0');
        let whole = if whole.is_empty() { "0" } else { whole };
        let fraction = fraction.unwrap_or_default().trim_end_matches('0');
        let canonical = if fraction.is_empty() {
            whole.to_owned()
        } else {
            format!("{whole}.{fraction}")
        };
        Ok(Self(canonical))
    }
}

impl std::fmt::Display for DecimalAmount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 单一币种的成本合计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrencyCost {
    pub currency: String,
    pub amount: DecimalAmount,
}

/// PostgreSQL 连续百分位返回的非负有限毫秒值。
///
/// 以 IEEE-754 bits 保存，既不丢失插值小数，也可安全实现 `Eq`。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PercentileMilliseconds(u64);

impl PercentileMilliseconds {
    /// 校验并保存一个毫秒百分位。
    ///
    /// # Errors
    ///
    /// 非有限值或负值返回 [`AdminModelError::InvalidLatencyPercentile`]。
    pub fn new(value: f64) -> Result<Self, AdminModelError> {
        if !value.is_finite() || value < 0.0 {
            return Err(AdminModelError::InvalidLatencyPercentile);
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

/// 延迟分布的三个稳定百分位。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LatencyPercentiles {
    pub p50_ms: Option<PercentileMilliseconds>,
    pub p95_ms: Option<PercentileMilliseconds>,
    pub p99_ms: Option<PercentileMilliseconds>,
}

/// Provider 价格规则计算所需的持久请求事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderBillingInput {
    pub upstream_model_id: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub total: CurrencyCost,
}

/// Provider 已确认的逐项费用与单价。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalculatedBillingBreakdown {
    pub input_amount: CurrencyCost,
    pub output_amount: CurrencyCost,
    pub cache_read_amount: CurrencyCost,
    pub cache_write_amount: CurrencyCost,
    pub standard_amount: CurrencyCost,
    pub total_amount: CurrencyCost,
    pub input_price_per_million: CurrencyCost,
    pub output_price_per_million: CurrencyCost,
    pub cache_read_price_per_million: CurrencyCost,
    pub cache_write_price_per_million: CurrencyCost,
    pub service_tier: Option<String>,
    pub multiplier_percent: u32,
}

/// 单次请求的费用语义。
///
/// Provider 上报费用或无法恢复逐项价格时保留总额；Provider 验证成功后升级为完整分解。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageBilling {
    Total { source: String, total: CurrencyCost },
    Calculated(Box<CalculatedBillingBreakdown>),
}

/// 计费数据覆盖情况。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CostCoverage {
    pub provider_reported_count: u64,
    pub calculated_count: u64,
    pub partial_count: u64,
    pub unavailable_count: u64,
    pub not_billable_count: u64,
}

impl CostCoverage {
    /// Provider 直接上报或 Provider 规则完整计算的已知成本数。
    #[must_use]
    pub fn known_count(&self) -> u64 {
        self.provider_reported_count
            .saturating_add(self.calculated_count)
    }
}

/// 请求级聚合指标。
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
    pub first_token_latency_sum_ms: u64,
    pub first_token_latency_count: u64,
    pub latency_sum_ms: u64,
    pub latency_count: u64,
    pub min_latency_ms: Option<u64>,
    pub max_latency_ms: Option<u64>,
    pub latency_percentiles: LatencyPercentiles,
    pub first_token_latency_percentiles: LatencyPercentiles,
    pub cache_eligible_request_count: u64,
    pub cache_hit_request_count: u64,
}

/// 上游 attempt 聚合指标。
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
    pub costs: Vec<CurrencyCost>,
}

/// 时间序列粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    FifteenMinutes,
    Hour,
    Day,
}

/// 一段时间桶内的请求指标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestMetricPoint {
    pub bucket_start: DateTime<Utc>,
    pub granularity: Granularity,
    pub metrics: RequestMetrics,
    pub cost_coverage: CostCoverage,
    pub costs: Vec<CurrencyCost>,
}

/// 账号池容量统计。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountPoolMetrics {
    pub total: u64,
    pub enabled: u64,
    pub unavailable: u64,
    pub active: u64,
    pub expired: u64,
    pub quota_exhausted: u64,
    pub refreshing: Option<u64>,
    pub disabled: u64,
    pub banned: u64,
}

/// Dashboard 中一个账号的模型级用量事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardAccountModelUsage {
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
    pub costs: Vec<CurrencyCost>,
    pub last_used_at: DateTime<Utc>,
}

/// Dashboard 中账号的单小时请求数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardAccountRequestBucket {
    pub bucket_start: DateTime<Utc>,
    pub request_count: u64,
}

/// Dashboard 中一个账号的完整用量与公共状态事实。
#[derive(Debug, Clone, PartialEq)]
pub struct DashboardAccountUsage {
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
    pub costs: Vec<CurrencyCost>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub request_buckets: Vec<DashboardAccountRequestBucket>,
    /// Provider 已持久化额度窗口投影出的代表性已用比例。
    ///
    /// `None` 表示上游未提供可比较的百分比，不应伪造为零。
    pub quota_used_percent: Option<f64>,
    pub models: Vec<DashboardAccountModelUsage>,
}

/// Dashboard 当前账号池的可重建运行时槽位事实。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashboardRuntimeSlots {
    /// 与持久账号池指标相同谓词下可参与调度的账号数。
    pub active_accounts: u64,
    /// Redis 可用时的实时 in-flight 槽位总数；不可用时为 `None`。
    pub used_slots: Option<u64>,
}

/// 仪表盘所需的公共观测事实。
#[derive(Debug, Clone, PartialEq)]
pub struct DashboardObservation {
    pub range: TimeRange,
    pub requests: RequestMetrics,
    pub attempts: AttemptMetrics,
    pub provider_accounts: AccountPoolMetrics,
    pub trend: Vec<RequestMetricPoint>,
    pub account_usage: Vec<DashboardAccountUsage>,
    pub recent_requests: Vec<UsageRecord>,
}

/// 一次完整模型请求的公共观测记录。
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
    pub attempt_count: u32,
    pub upstream_send_state: String,
    pub downstream_committed_at: Option<DateTime<Utc>>,
    pub outcome: RequestOutcome,
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
    pub billing: Option<UsageBilling>,
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

/// 用量分页结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsagePage {
    pub items: Vec<UsageRecord>,
    pub total: u64,
    pub next_cursor: Option<ObservabilityCursor>,
}

/// 请求中的一次上游尝试或运维事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageAttempt {
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
    pub outcome: RequestOutcome,
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

/// 一条请求及其全部尝试。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageDetail {
    pub request: UsageRecord,
    pub attempts: Vec<UsageAttempt>,
}

/// 一个 Provider 的聚合用量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderObservation {
    pub provider_kind: String,
    pub request_count: u64,
    pub attempt_count: u64,
    pub failure_count: u64,
    pub total_tokens: u64,
}

/// 用量总览。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageOverview {
    pub range: TimeRange,
    pub requests: RequestMetrics,
    pub attempts: AttemptMetrics,
    pub providers: Vec<ProviderObservation>,
}

/// 用量摘要的用例结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageSummary {
    pub overview: UsageOverview,
    pub average_latency_ms: Option<u64>,
}

/// 单个诊断维度值的聚合结果。
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
    pub costs: Vec<CurrencyCost>,
}

/// 统一运维错误记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsError {
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

/// 运维错误分页结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpsErrorPage {
    pub items: Vec<OpsError>,
    pub total: u64,
    pub next_cursor: Option<ObservabilityCursor>,
}

/// 仪表盘趋势指标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrendKind {
    Usage,
    Latency,
    Errors,
}

/// 一个趋势桶的全部已计算语义，API 只负责时区标签与展示格式。
#[derive(Debug, Clone, PartialEq)]
pub struct TrendPoint {
    pub bucket_start: DateTime<Utc>,
    pub granularity: Granularity,
    pub metrics: RequestMetrics,
    pub service_failure_count: u64,
    pub average_latency_ms: Option<u64>,
    pub average_first_token_latency_ms: Option<u64>,
    pub cached_token_rate: f64,
    pub cache_hit_request_rate: Option<f64>,
    pub success_rate: Option<f64>,
    pub cost_coverage: CostCoverage,
    pub costs: Vec<CurrencyCost>,
}

/// 趋势的无格式化汇总。
#[derive(Debug, Clone, PartialEq)]
pub struct TrendSummary {
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub service_failure_count: u64,
    pub average_latency_ms: Option<u64>,
    pub max_latency_ms: Option<u64>,
    pub min_latency_ms: Option<u64>,
    pub success_rate: Option<f64>,
    pub cache_hit_request_rate: Option<f64>,
    pub costs: Vec<CurrencyCost>,
    pub cost_coverage: CostCoverage,
}

/// 趋势结果；API 再决定图表标签与数字格式。
#[derive(Debug, Clone, PartialEq)]
pub struct Trend {
    pub kind: TrendKind,
    pub points: Vec<TrendPoint>,
    pub summary: TrendSummary,
}

/// 健康时间桶状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Future,
    NoData,
    Unavailable,
    LowSample,
    Unstable,
    Stable,
}

/// 一个 15 分钟健康桶的语义结果。
#[derive(Debug, Clone, PartialEq)]
pub struct HealthTimelinePoint {
    pub bucket_start: DateTime<Utc>,
    pub status: HealthStatus,
    pub reliability_percent: Option<f64>,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub incomplete_requests: u64,
    pub caller_error_requests: u64,
}

/// 中国自然日的固定 96 槽健康时间线。
#[derive(Debug, Clone, PartialEq)]
pub struct HealthTimeline {
    pub reliability_percent: Option<f64>,
    pub status: HealthStatus,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub incomplete_requests: u64,
    pub caller_error_requests: u64,
    pub points: Vec<HealthTimelinePoint>,
}

/// Dashboard 请求画像的目标平台。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardWireTarget {
    pub os_type: String,
    pub os_version: String,
    pub arch: String,
    pub terminal: String,
}

/// Desktop 发布检查状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopReleaseStatus {
    Unchecked,
    Current,
    UpdateAvailable,
    Failed,
}

/// 与请求画像分离的 Desktop 发布检查事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardDesktopRelease {
    pub status: DesktopReleaseStatus,
    pub checked_at: Option<DateTime<Utc>>,
    pub latest_version: Option<String>,
    pub latest_build: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub minimum_system_version: Option<String>,
    pub hardware_requirements: Option<String>,
    pub download_url: Option<String>,
    pub download_size: Option<u64>,
    pub signature_present: Option<bool>,
    pub error: Option<String>,
}

/// Provider 拥有并实际用于上游请求的身份画像快照。
///
/// `attributes` 承载 Provider 特有的可观测字段；公共控制面不解释其语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardWireProfile {
    pub provider: String,
    pub product: String,
    pub version: String,
    pub build: Option<String>,
    pub target: DashboardWireTarget,
    pub user_agent: String,
    pub attributes: Vec<DashboardWireAttribute>,
    pub verified_at: Option<DateTime<Utc>>,
    pub release: Option<DashboardDesktopRelease>,
}

/// Provider 画像的差异字段；展示层只显示标签和值，不解释 Provider 语义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardWireAttribute {
    pub label: String,
    pub value: String,
}

/// 当前账号池的并发调度容量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardCapacity {
    pub max_concurrent_per_account: u64,
    pub total_slots: u64,
    pub used_slots: Option<u64>,
    pub available_slots: Option<u64>,
}

/// Dashboard 某个自然日区间内的卡片计数。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DashboardPeriodMetrics {
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub cached_token_rate: f64,
    pub observed_cached_token_rate: Option<f64>,
}

/// 仪表盘聚合结果。
#[derive(Debug, Clone, PartialEq)]
pub struct DashboardResult {
    pub observation: DashboardObservation,
    pub today: DashboardPeriodMetrics,
    pub yesterday: DashboardPeriodMetrics,
    pub total_billing_usd: Option<DecimalAmount>,
    pub total_cached_token_rate: f64,
    pub average_first_token_latency_ms: Option<u64>,
    pub trend: Trend,
    pub health_timeline: HealthTimeline,
    pub wire_profiles: Vec<DashboardWireProfile>,
    pub capacity: DashboardCapacity,
    pub rotation_strategy: super::settings::RotationStrategy,
}

/// 洞察健康时间点的语义结果。
#[derive(Debug, Clone, PartialEq)]
pub struct UsageInsightsHealthPoint {
    pub bucket_start: DateTime<Utc>,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub incomplete_requests: u64,
    pub caller_error_requests: u64,
    pub error_rate: f64,
}

/// 洞察健康汇总；失败数已排除调用方错误。
#[derive(Debug, Clone, PartialEq)]
pub struct UsageInsightsHealth {
    pub total_requests: u64,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub incomplete_requests: u64,
    pub caller_error_requests: u64,
    pub success_rate: f64,
    pub points: Vec<UsageInsightsHealthPoint>,
}

/// 洞察性能时间点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageInsightsPerformancePoint {
    pub bucket_start: DateTime<Utc>,
    pub latency_percentiles: LatencyPercentiles,
    pub first_token_latency_percentiles: LatencyPercentiles,
}

/// 洞察性能汇总与可观测覆盖率。
#[derive(Debug, Clone, PartialEq)]
pub struct UsageInsightsPerformance {
    pub latency_percentiles: LatencyPercentiles,
    pub first_token_latency_percentiles: LatencyPercentiles,
    pub latency_coverage: f64,
    pub first_token_coverage: f64,
    pub points: Vec<UsageInsightsPerformancePoint>,
}

/// 洞察成本时间点。
#[derive(Debug, Clone, PartialEq)]
pub struct UsageInsightsCostPoint {
    pub bucket_start: DateTime<Utc>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost: Option<DecimalAmount>,
    pub standard_cost: Option<DecimalAmount>,
    pub cached_token_rate: f64,
    pub cache_hit_request_rate: Option<f64>,
}

/// 洞察成本汇总。
#[derive(Debug, Clone, PartialEq)]
pub struct UsageInsightsCost {
    pub estimated_cost: Option<DecimalAmount>,
    pub standard_cost: Option<DecimalAmount>,
    pub cost_per_request: Option<DecimalAmount>,
    pub tokens_per_request: f64,
    pub cached_token_rate: f64,
    pub cache_hit_request_rate: Option<f64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub points: Vec<UsageInsightsCostPoint>,
    pub costs: Vec<CurrencyCost>,
    pub coverage: CostCoverage,
}

/// 用量洞察的完整用例结果；API 只负责标签、时区与字符串格式化。
#[derive(Debug, Clone, PartialEq)]
pub struct UsageInsights {
    pub granularity: Granularity,
    pub health: UsageInsightsHealth,
    pub performance: UsageInsightsPerformance,
    pub cost: UsageInsightsCost,
    pub attempts: AttemptMetrics,
    pub providers: Vec<ProviderObservation>,
}

/// 已完成分母计算的诊断项。
#[derive(Debug, Clone, PartialEq)]
pub struct DiagnosticsItem {
    pub name: String,
    pub request_count: u64,
    pub success_count: u64,
    pub error_count: u64,
    pub error_rate: f64,
    pub request_share: f64,
    pub average_latency_ms: Option<u64>,
    pub estimated_cost: Option<DecimalAmount>,
    pub attempt_count: u64,
    pub total_tokens: u64,
}

/// 诊断结果。
#[derive(Debug, Clone, PartialEq)]
pub struct DiagnosticsResult {
    pub dimension: DiagnosticDimension,
    pub items: Vec<DiagnosticsItem>,
}
