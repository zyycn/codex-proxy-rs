//! Dashboard、用量与诊断查询的 wire、service port 和固定路由。

use async_trait::async_trait;
use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminResponse, AdminServiceError, AdminServiceErrorKind,
    AdminSessionState, WireValidationError,
};

/// 观测列表默认页大小。
pub const DEFAULT_PAGE_SIZE: u16 = 50;
/// 观测列表允许的最大页大小。
pub const MAX_PAGE_SIZE: u16 = 100;
/// 游标编码在 HTTP 层解码前允许的最大长度。
pub const MAX_CURSOR_BYTES: usize = 1024;

/// Dashboard 查询参数。
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DashboardQuery {
    pub kind: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
}

impl DashboardQuery {
    /// 解析 dashboard 趋势类型。
    pub fn trend_kind(&self) -> Result<TrendKind, WireValidationError> {
        TrendKind::parse(self.kind.as_deref())
    }
}

/// 逻辑请求列表、汇总和洞察查询参数。
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageQuery {
    pub page: Option<u32>,
    pub page_size: Option<u16>,
    pub cursor: Option<String>,
    pub kind: Option<String>,
    pub outcome: Option<String>,
    pub client_api_key_id: Option<String>,
    pub provider: Option<String>,
    pub request_id: Option<String>,
    pub account_id: Option<String>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub status_code: Option<i64>,
    pub transport: Option<String>,
    pub attempt_index: Option<i64>,
    pub response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub search: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
}

impl UsageQuery {
    /// 校验分页字段，不改变仓储层的分页 owner。
    pub fn validate_page(&self) -> Result<(u32, u16), WireValidationError> {
        let page = self.page.unwrap_or(1);
        if page == 0 {
            return Err(WireValidationError::new("page"));
        }
        let page_size = self.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
        if page_size == 0 || page_size > MAX_PAGE_SIZE {
            return Err(WireValidationError::new("pageSize"));
        }
        Ok((page, page_size))
    }

    /// 校验游标的 wire 边界；编码和排序语义由应用层负责。
    pub fn validate_cursor(&self) -> Result<(), WireValidationError> {
        validate_cursor(self.cursor.as_deref())
    }
}

/// 详情查询参数。
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DetailQuery {
    pub id: String,
}

impl DetailQuery {
    /// 校验详情 ID，错误不回显输入值。
    pub fn validate(&self) -> Result<(), WireValidationError> {
        require_text(&self.id, "id")
    }
}

/// 诊断聚合查询参数。
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticsQuery {
    pub dimension: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub status_code: Option<i64>,
    pub search: Option<String>,
}

impl DiagnosticsQuery {
    /// 解析诊断维度。
    pub fn dimension(&self) -> Result<DiagnosticDimension, WireValidationError> {
        DiagnosticDimension::parse(self.dimension.as_deref())
    }
}

/// 运维错误查询参数。
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpsQuery {
    pub page: Option<u32>,
    pub page_size: Option<u16>,
    pub cursor: Option<String>,
    pub kind: Option<String>,
    pub client_api_key_id: Option<String>,
    pub provider: Option<String>,
    pub request_id: Option<String>,
    pub account_id: Option<String>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub status_code: Option<i64>,
    pub client_status_code: Option<i64>,
    pub upstream_status_code: Option<i64>,
    pub transport: Option<String>,
    pub attempt_index: Option<i64>,
    pub failure_class: Option<String>,
    pub response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub search: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
}

impl OpsQuery {
    /// 校验分页字段。
    pub fn validate_page(&self) -> Result<(u32, u16), WireValidationError> {
        let page = self.page.unwrap_or(1);
        if page == 0 {
            return Err(WireValidationError::new("page"));
        }
        let page_size = self.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
        if page_size == 0 || page_size > MAX_PAGE_SIZE {
            return Err(WireValidationError::new("pageSize"));
        }
        Ok((page, page_size))
    }

    /// 校验游标的 wire 边界。
    pub fn validate_cursor(&self) -> Result<(), WireValidationError> {
        validate_cursor(self.cursor.as_deref())
    }
}

/// Dashboard 趋势类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrendKind {
    Usage,
    Latency,
    Errors,
}

impl TrendKind {
    /// 从 query 值解析趋势类型。
    pub fn parse(value: Option<&str>) -> Result<Self, WireValidationError> {
        match trimmed(value) {
            None | Some("usage") => Ok(Self::Usage),
            Some("latency") => Ok(Self::Latency),
            Some("errors") => Ok(Self::Errors),
            Some(_) => Err(WireValidationError::new("kind")),
        }
    }
}

/// 诊断聚合维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticDimension {
    Model,
    Account,
    ApiKey,
    Provider,
    Transport,
    Failure,
    Status,
}

impl DiagnosticDimension {
    /// 从 query 值解析诊断维度。
    pub fn parse(value: Option<&str>) -> Result<Self, WireValidationError> {
        match trimmed(value) {
            None | Some("model") => Ok(Self::Model),
            Some("account") => Ok(Self::Account),
            Some("apiKey" | "api_key") => Ok(Self::ApiKey),
            Some("provider") => Ok(Self::Provider),
            Some("transport") => Ok(Self::Transport),
            Some("failureClass" | "failure_class") => Ok(Self::Failure),
            Some("status") => Ok(Self::Status),
            Some(_) => Err(WireValidationError::new("dimension")),
        }
    }

    /// 返回终态响应中的稳定维度名称。
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Account => "account",
            Self::ApiKey => "apiKey",
            Self::Provider => "provider",
            Self::Transport => "transport",
            Self::Failure => "failureClass",
            Self::Status => "status",
        }
    }
}

/// 解析 RFC3339 时间；错误不回显原始值。
pub fn parse_datetime(value: Option<&str>) -> Result<Option<DateTime<Utc>>, WireValidationError> {
    let Some(value) = trimmed(value) else {
        return Ok(None);
    };
    DateTime::parse_from_rfc3339(value)
        .map(|value| Some(value.with_timezone(&Utc)))
        .map_err(|_| WireValidationError::new("timeRange"))
}

/// 解析 HTTP 状态码。
pub fn parse_status(value: Option<i64>) -> Result<Option<u16>, WireValidationError> {
    value
        .map(|value| {
            u16::try_from(value)
                .ok()
                .filter(|value| (100..=599).contains(value))
                .ok_or_else(|| WireValidationError::new("statusCode"))
        })
        .transpose()
}

/// 解析尝试序号。
pub fn parse_attempt_index(value: Option<i64>) -> Result<Option<u32>, WireValidationError> {
    value
        .map(|value| {
            u32::try_from(value)
                .ok()
                .filter(|value| *value > 0 && *value <= i32::MAX as u32)
                .ok_or_else(|| WireValidationError::new("attemptIndex"))
        })
        .transpose()
}

fn require_text(value: &str, field: &'static str) -> Result<(), WireValidationError> {
    if value.trim().is_empty() {
        Err(WireValidationError::new(field))
    } else {
        Ok(())
    }
}

fn validate_cursor(value: Option<&str>) -> Result<(), WireValidationError> {
    if value.is_some_and(|value| value.is_empty() || value.len() > MAX_CURSOR_BYTES) {
        return Err(WireValidationError::new("cursor"));
    }
    Ok(())
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// 观测列表游标的稳定 wire 形状。
#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CursorWire {
    pub observed_at: DateTime<Utc>,
    pub stable_id: String,
}

/// 观测列表分页元数据。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageMeta {
    pub page: u32,
    pub page_size: u16,
    pub total: u64,
    pub total_pages: u32,
}

impl PageMeta {
    /// 由应用层已校验的分页事实构造响应元数据。
    #[must_use]
    pub const fn new(page: u32, page_size: u16, total: u64, total_pages: u32) -> Self {
        Self {
            page,
            page_size,
            total,
            total_pages,
        }
    }
}

/// 观测列表响应数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageData<T> {
    pub items: Vec<T>,
    pub page: PageMeta,
    pub next_cursor: Option<String>,
}

/// Token 详情展示。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenDetailsView {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub input_tokens_display: String,
    pub output_tokens_display: String,
    pub cached_tokens_display: String,
    pub cache_write_tokens_display: String,
    pub reasoning_tokens_display: String,
    pub total_tokens_display: String,
}

/// 按货币展示的成本。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostView {
    pub currency: String,
    pub estimated_amount: String,
}

/// 成本覆盖状态计数。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostCoverageView {
    pub known: u64,
    pub partial: u64,
    pub unknown: u64,
    pub not_billable: u64,
}

/// Provider 受控价格规则生成的单次请求费用明细展示。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingView {
    pub input_amount_display: String,
    pub output_amount_display: String,
    pub cache_read_amount_display: String,
    pub cache_write_amount_display: String,
    pub standard_amount_display: String,
    pub total_amount_display: String,
    pub input_price_display: String,
    pub output_price_display: String,
    pub cache_read_price_display: String,
    pub cache_write_price_display: String,
    pub service_tier_display: String,
    pub multiplier_display: String,
}

/// 单条逻辑请求展示。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordView {
    pub id: String,
    pub request_id: String,
    pub client_api_key_id: Option<String>,
    pub kind: String,
    pub provider: Option<String>,
    pub account_id: Option<String>,
    pub account_email: Option<String>,
    pub route: Option<String>,
    pub model: String,
    pub requested_model: Option<String>,
    pub upstream_model: Option<String>,
    pub service_tier: Option<String>,
    pub status_code: Option<i64>,
    pub transport: Option<String>,
    pub attempt_index: Option<i64>,
    pub attempt_count: u64,
    pub response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub latency_ms: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub message: String,
    pub metadata: UsageRecordMetadataView,
    pub created_at: DateTime<Utc>,
    pub created_at_display: String,
    pub client_ip: Option<String>,
    pub user_agent: Option<String>,
    pub reasoning_effort: Option<String>,
    pub reasoning_preset: Option<String>,
    pub compact: Option<bool>,
    pub request_kind: Option<String>,
    pub subagent_kind: Option<String>,
    pub token_details: TokenDetailsView,
    pub billing: Option<BillingView>,
    pub costs: Vec<CostView>,
    pub cost_coverage: CostCoverageView,
    pub first_token_latency_ms: Option<u64>,
    pub first_token_latency_ms_display: String,
    pub latency_ms_display: String,
    pub logical_outcome: String,
}

/// 逻辑请求安全元数据；不承载原始请求体或上游错误正文。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordMetadataView {
    pub protocol: String,
    pub logical_outcome: String,
    pub attempt_count: u64,
}

/// 单次上游尝试展示。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageAttemptView {
    pub id: String,
    pub attempt_index: u32,
    pub trigger: String,
    pub provider: String,
    pub provider_instance_id: String,
    pub model: String,
    pub transport: String,
    pub send_state: String,
    pub outcome: String,
    pub downstream_committed: bool,
    pub status_code: Option<u16>,
    pub provider_error_code: Option<String>,
    pub failure_class: Option<String>,
    pub cost_estimate_status: String,
    pub estimated_cost_amount: Option<String>,
    pub estimated_cost_currency: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub latency_ms: Option<u64>,
    pub credential_name: Option<String>,
    pub account_email: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// 逻辑请求详情与其尝试列表。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordDetailView {
    #[serde(flatten)]
    pub request: UsageRecordView,
    pub attempts: Vec<UsageAttemptView>,
}

/// Dashboard 趋势数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrendData {
    pub kind: TrendKind,
    pub points: Vec<TrendPointView>,
    pub summary: Vec<TrendSummaryView>,
}

/// Dashboard 单个趋势桶。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrendPointView {
    pub time: String,
    pub bucket: DateTime<Utc>,
    pub label: String,
    pub requests: String,
    pub requests_value: u64,
    pub input_tokens: String,
    pub input_tokens_value: u64,
    pub output_tokens: String,
    pub output_tokens_value: u64,
    pub cached_tokens: String,
    pub cached_tokens_value: u64,
    pub cache_hit_rate_value: f64,
    pub tokens_value: u64,
    pub errors: String,
    pub errors_value: u64,
    pub latency: String,
    pub latency_value: Option<u64>,
    pub first_token_latency: String,
    pub first_token_latency_value: Option<u64>,
    pub max_latency: String,
    pub max_latency_value: Option<u64>,
    pub min_latency: String,
    pub min_latency_value: Option<u64>,
    pub success_rate: String,
    pub success_rate_value: Option<f64>,
}

/// Dashboard 趋势摘要。
#[derive(Debug, Serialize)]
pub struct TrendSummaryView {
    pub label: String,
    pub value: String,
    pub ratio: Option<String>,
}

/// Dashboard 卡片集合。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCardsView {
    pub credentials: DashboardCredentialsCardView,
    pub traffic: DashboardTrafficCardView,
    pub tokens: DashboardTokensCardView,
    pub cache: DashboardCacheCardView,
}

/// Dashboard 上游凭据卡片。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCredentialsCardView {
    pub total: String,
    pub total_value: u64,
    pub enabled: String,
    pub enabled_value: u64,
    pub unavailable: String,
    pub unavailable_value: u64,
}

/// Dashboard 流量卡片。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTrafficCardView {
    pub today_requests: String,
    pub today_requests_value: u64,
    pub yesterday_requests_value: u64,
    pub total_requests: String,
}

/// Dashboard token 卡片。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTokensCardView {
    pub today_tokens: String,
    pub today_tokens_value: u64,
    pub yesterday_tokens_value: u64,
    pub total_tokens: String,
    pub total_billing_amount_usd: String,
}

/// Dashboard 缓存卡片。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCacheCardView {
    pub today_hit_rate: String,
    pub today_hit_rate_value: Option<f64>,
    pub yesterday_hit_rate_value: Option<f64>,
    pub total_hit_rate: String,
    pub total_cached_tokens: String,
    pub average_first_token_latency_ms: String,
}

/// Dashboard 凭据用量摘要。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCredentialUsageView {
    pub id: String,
    pub display_name: String,
    pub plan_type: Option<String>,
    pub tokens: String,
    pub tokens_value: Option<u64>,
    pub last_used: String,
    pub provider: String,
    pub availability: String,
    pub request_count: u64,
}

/// 旧 Dashboard 账号概览卡片所需的 Provider 安全投影。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardAccountUsageView {
    pub id: String,
    pub email: String,
    pub plan_type: Option<String>,
    pub tokens: String,
    pub quota_used_percent: Option<f64>,
    pub last_used: String,
}

/// Provider 账号池的持久事实汇总。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardPoolSummaryView {
    pub total: u64,
    pub active: u64,
    pub expired: u64,
    pub quota_exhausted: u64,
    pub refreshing: Option<u64>,
    pub disabled: u64,
    pub banned: u64,
}

/// 同 target 账号调度容量；Redis 未提供聚合事实时运行中槽位保持空值。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCapacityInfoView {
    pub max_concurrent_per_account: u64,
    pub total_slots: u64,
    pub used_slots: Option<u64>,
    pub available_slots: Option<u64>,
}

/// 逻辑请求指标展示。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestMetricsView {
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
}

/// 上游尝试指标展示。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptMetricsView {
    pub attempt_count: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub cancelled_count: u64,
    pub incomplete_count: u64,
    pub rate_limited_count: u64,
    pub auth_failure_count: u64,
    pub provider5xx_count: u64,
    pub cost_coverage: CostCoverageView,
    pub costs: Vec<CostView>,
}

/// 健康时间线单点。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthTimelinePointView {
    pub time: String,
    pub status: String,
    pub reliability_display: String,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub incomplete_requests: u64,
    pub caller_error_requests: u64,
}

/// Dashboard 健康时间线。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthTimelineView {
    pub title: String,
    pub description: String,
    pub reliability_display: String,
    pub status: String,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub incomplete_requests: u64,
    pub caller_error_requests: u64,
    pub points: Vec<HealthTimelinePointView>,
}

/// Dashboard 展示的实际 Codex 请求画像。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardWireProfileView {
    pub originator: String,
    pub codex_version: String,
    pub desktop_version: String,
    pub desktop_build: String,
    pub target: DashboardWireTargetView,
    pub user_agent: String,
    pub verified_at: DateTime<Utc>,
    pub release: DashboardDesktopReleaseView,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardWireTargetView {
    pub os_type: String,
    pub os_version: String,
    pub arch: String,
    pub terminal: String,
}

/// 发布检查与启动画像分离；未检查时使用明确的 `unchecked` 状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardDesktopReleaseView {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_build: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_system_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardware_requirements: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_present: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Dashboard 汇总响应数据。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardDataView {
    pub cards: DashboardCardsView,
    pub trend: TrendData,
    pub health_timeline: HealthTimelineView,
    pub wire_profile: DashboardWireProfileView,
    pub account_usage: Vec<DashboardAccountUsageView>,
    pub credential_usage: Vec<DashboardCredentialUsageView>,
    pub usage_records: Vec<UsageRecordView>,
    pub pool_summary: DashboardPoolSummaryView,
    pub capacity_info: DashboardCapacityInfoView,
    pub rotation_strategy: String,
    pub logical_requests: RequestMetricsView,
    pub attempts: AttemptMetricsView,
    pub costs: Vec<CostView>,
}

/// 用量汇总响应数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummaryView {
    pub total_requests: String,
    pub input_tokens: String,
    pub output_tokens: String,
    pub cached_tokens: String,
    pub cache_write_tokens: String,
    pub total_tokens: String,
    pub average_latency_ms: String,
    pub logical_requests: RequestMetricsView,
    pub attempts: AttemptMetricsView,
}

/// 洞察健康趋势点。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewHealthPointView {
    pub bucket: DateTime<Utc>,
    pub label: String,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub incomplete_requests: u64,
    pub caller_error_requests: u64,
    pub error_rate: f64,
}

/// 洞察健康摘要。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewHealthView {
    pub total_requests: u64,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub cancelled_requests: u64,
    pub incomplete_requests: u64,
    pub caller_error_requests: u64,
    pub success_rate: f64,
    pub request_change_rate: Option<f64>,
    pub success_rate_change: Option<f64>,
    pub points: Vec<OverviewHealthPointView>,
}

/// 洞察性能趋势点。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewPerformancePointView {
    pub bucket: DateTime<Utc>,
    pub label: String,
    pub latency_p50_ms: Option<f64>,
    pub latency_p95_ms: Option<f64>,
    pub latency_p99_ms: Option<f64>,
    pub first_token_p50_ms: Option<f64>,
    pub first_token_p95_ms: Option<f64>,
    pub first_token_p99_ms: Option<f64>,
}

/// 洞察性能摘要。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewPerformanceView {
    pub latency_p50_ms: Option<f64>,
    pub latency_p95_ms: Option<f64>,
    pub latency_p99_ms: Option<f64>,
    pub first_token_p50_ms: Option<f64>,
    pub first_token_p95_ms: Option<f64>,
    pub first_token_p99_ms: Option<f64>,
    pub latency_coverage: f64,
    pub first_token_coverage: f64,
    pub points: Vec<OverviewPerformancePointView>,
}

/// 洞察成本趋势点。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewCostPointView {
    pub bucket: DateTime<Utc>,
    pub label: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost: Option<String>,
    pub standard_cost: Option<String>,
    pub cached_token_rate: f64,
    pub cache_hit_request_rate: Option<f64>,
}

/// 洞察成本摘要。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewCostView {
    pub estimated_cost: Option<String>,
    pub standard_cost: Option<String>,
    pub cost_per_request: Option<String>,
    pub tokens_per_request: f64,
    pub cached_token_rate: f64,
    pub cache_hit_request_rate: Option<f64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
    pub points: Vec<OverviewCostPointView>,
    pub costs: Vec<CostView>,
    pub coverage: CostCoverageView,
}

/// Provider 维度洞察摘要。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOverviewView {
    pub provider: String,
    pub request_count: u64,
    pub attempt_count: u64,
    pub failure_count: u64,
    pub total_tokens: u64,
}

/// 用量洞察总响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageInsightsOverviewView {
    pub granularity: String,
    pub health: OverviewHealthView,
    pub performance: OverviewPerformanceView,
    pub cost: OverviewCostView,
    pub attempts: AttemptMetricsView,
    pub providers: Vec<ProviderOverviewView>,
}

/// 诊断聚合项目。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticItemView {
    pub name: String,
    pub request_count: u64,
    pub success_count: u64,
    pub error_count: u64,
    pub error_rate: f64,
    pub request_share: f64,
    pub average_latency_ms: Option<u64>,
    pub estimated_cost: Option<String>,
    pub attempt_count: u64,
    pub total_tokens: u64,
}

/// 诊断聚合响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsView {
    pub dimension: String,
    pub items: Vec<DiagnosticItemView>,
}

/// 运维错误项目。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsErrorView {
    pub id: String,
    pub request_id: Option<String>,
    pub client_api_key_id: Option<String>,
    pub kind: String,
    pub provider: Option<String>,
    pub account_id: Option<String>,
    pub route: String,
    pub model: Option<String>,
    pub status_code: Option<i64>,
    pub client_status_code: Option<i64>,
    pub upstream_status_code: Option<i64>,
    pub transport: Option<String>,
    pub attempt_index: Option<u32>,
    pub failure_class: String,
    pub response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub latency_ms: Option<u64>,
    pub message: String,
    pub metadata: OpsErrorMetadataView,
    pub created_at: DateTime<Utc>,
    pub created_at_display: String,
}

/// 运维错误安全元数据；只保留可查询的标识，不回显秘密材料。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsErrorMetadataView {
    pub source: String,
    pub component: String,
    pub attempt_id: Option<String>,
    pub provider_instance_id: Option<String>,
    pub account_label: Option<String>,
}

#[async_trait]
pub trait ObservabilityAdminService: Send + Sync {
    async fn dashboard_summary(
        &self,
        query: DashboardQuery,
    ) -> Result<DashboardDataView, AdminServiceError>;
    async fn dashboard_trend(&self, query: DashboardQuery) -> Result<TrendData, AdminServiceError>;
    async fn usage_records(
        &self,
        query: UsageQuery,
    ) -> Result<PageData<UsageRecordView>, AdminServiceError>;
    async fn usage_record_detail(
        &self,
        query: DetailQuery,
    ) -> Result<UsageRecordDetailView, AdminServiceError>;
    async fn usage_records_summary(
        &self,
        query: UsageQuery,
    ) -> Result<UsageSummaryView, AdminServiceError>;
    async fn usage_insights_overview(
        &self,
        query: UsageQuery,
    ) -> Result<UsageInsightsOverviewView, AdminServiceError>;
    async fn usage_insights_diagnostics(
        &self,
        query: DiagnosticsQuery,
    ) -> Result<DiagnosticsView, AdminServiceError>;
    async fn ops_errors(
        &self,
        query: OpsQuery,
    ) -> Result<PageData<OpsErrorView>, AdminServiceError>;
}

pub trait ObservabilityAdminState: AdminSessionState {
    fn observability_admin_service(&self) -> &dyn ObservabilityAdminService;
}

pub fn router<S>() -> Router<S>
where
    S: ObservabilityAdminState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/dashboard/summary", get(dashboard_summary::<S>))
        .route("/api/admin/dashboard/trend", get(dashboard_trend::<S>))
        .route("/api/admin/usage/records", get(usage_records::<S>))
        .route(
            "/api/admin/usage/records/detail",
            get(usage_record_detail::<S>),
        )
        .route(
            "/api/admin/usage/records/summary",
            get(usage_records_summary::<S>),
        )
        .route(
            "/api/admin/usage/records/insights/overview",
            get(usage_insights_overview::<S>),
        )
        .route(
            "/api/admin/usage/records/insights/diagnostics",
            get(usage_insights_diagnostics::<S>),
        )
        .route("/api/admin/ops/errors", get(ops_errors::<S>))
}

macro_rules! query_handler {
    ($name:ident, $query:ty, $data:ty, $method:ident) => {
        async fn $name<S>(
            _auth: AdminAuth,
            State(state): State<S>,
            Query(query): Query<$query>,
        ) -> Result<impl IntoResponse, AdminError>
        where
            S: ObservabilityAdminState + Send + Sync,
        {
            let data: $data = state
                .observability_admin_service()
                .$method(query)
                .await
                .map_err(map_service_error)?;
            Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
        }
    };
}

query_handler!(
    dashboard_summary,
    DashboardQuery,
    DashboardDataView,
    dashboard_summary
);
query_handler!(dashboard_trend, DashboardQuery, TrendData, dashboard_trend);
query_handler!(
    usage_records,
    UsageQuery,
    PageData<UsageRecordView>,
    usage_records
);
query_handler!(
    usage_record_detail,
    DetailQuery,
    UsageRecordDetailView,
    usage_record_detail
);
query_handler!(
    usage_records_summary,
    UsageQuery,
    UsageSummaryView,
    usage_records_summary
);
query_handler!(
    usage_insights_overview,
    UsageQuery,
    UsageInsightsOverviewView,
    usage_insights_overview
);
query_handler!(
    usage_insights_diagnostics,
    DiagnosticsQuery,
    DiagnosticsView,
    usage_insights_diagnostics
);
query_handler!(ops_errors, OpsQuery, PageData<OpsErrorView>, ops_errors);

fn map_service_error(error: AdminServiceError) -> AdminError {
    match error.kind() {
        AdminServiceErrorKind::Invalid => AdminError::bad_request(error.to_string()),
        AdminServiceErrorKind::NotFound => AdminError::not_found(error.to_string()),
        AdminServiceErrorKind::Conflict => AdminError::conflict(error.to_string()),
        AdminServiceErrorKind::Unavailable => {
            AdminError::service_unavailable("Observability repository unavailable")
        }
        AdminServiceErrorKind::Internal => AdminError::internal(error.to_string()),
    }
}
