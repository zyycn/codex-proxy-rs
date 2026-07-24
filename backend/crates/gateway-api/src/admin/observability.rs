//! Dashboard、用量与诊断查询的 wire 映射和固定路由。

use std::num::NonZeroU32;

use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Timelike as _, Utc};
use gateway_admin::model::{PageSize as DomainPageSize, observability as domain};
use serde::{Deserialize, Serialize};

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminResponse, AdminSessionState, WireValidationError,
    wire::map_admin_service_error,
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

fn domain_trend_kind(kind: TrendKind) -> domain::TrendKind {
    match kind {
        TrendKind::Usage => domain::TrendKind::Usage,
        TrendKind::Latency => domain::TrendKind::Latency,
        TrendKind::Errors => domain::TrendKind::Errors,
    }
}

fn domain_diagnostic_dimension(dimension: DiagnosticDimension) -> domain::DiagnosticDimension {
    match dimension {
        DiagnosticDimension::Model => domain::DiagnosticDimension::Model,
        DiagnosticDimension::Account => domain::DiagnosticDimension::Account,
        DiagnosticDimension::ApiKey => domain::DiagnosticDimension::ApiKey,
        DiagnosticDimension::Provider => domain::DiagnosticDimension::Provider,
        DiagnosticDimension::Transport => domain::DiagnosticDimension::Transport,
        DiagnosticDimension::Failure => domain::DiagnosticDimension::Failure,
        DiagnosticDimension::Status => domain::DiagnosticDimension::Status,
    }
}

fn request_outcome(
    value: Option<String>,
) -> Result<Option<domain::RequestOutcome>, WireValidationError> {
    non_empty(value)
        .map(|value| {
            domain::RequestOutcome::new(value).map_err(|_| WireValidationError::new("outcome"))
        })
        .transpose()
}

fn usage_range(
    start: Option<&str>,
    end: Option<&str>,
) -> Result<domain::TimeRange, WireValidationError> {
    let end = parse_datetime(end)?.unwrap_or_else(Utc::now);
    let start = parse_datetime(start)?.unwrap_or(end - Duration::days(7));
    domain::TimeRange::new(start, end).map_err(|_| WireValidationError::new("timeRange"))
}

fn dashboard_range(
    start: Option<&str>,
    end: Option<&str>,
) -> Result<domain::TimeRange, WireValidationError> {
    let end = parse_datetime(end)?.unwrap_or_else(Utc::now);
    let start = parse_datetime(start)?.unwrap_or_else(|| china_day_start(end) - Duration::days(1));
    domain::TimeRange::new(start, end).map_err(|_| WireValidationError::new("timeRange"))
}

fn dashboard_today_range(
    start: Option<&str>,
    end: Option<&str>,
) -> Result<domain::TimeRange, WireValidationError> {
    let end = parse_datetime(end)?.unwrap_or_else(Utc::now);
    let start = parse_datetime(start)?.unwrap_or_else(|| china_day_start(end));
    domain::TimeRange::new(start, end).map_err(|_| WireValidationError::new("timeRange"))
}

fn usage_filter(query: &UsageQuery) -> Result<domain::UsageFilter, WireValidationError> {
    let outcome = query.outcome.clone().or_else(|| {
        non_empty(query.kind.clone()).filter(|value| {
            matches!(
                value.as_str(),
                "running" | "succeeded" | "failed" | "cancelled" | "incomplete"
            )
        })
    });
    Ok(domain::UsageFilter {
        client_api_key_ref: non_empty(query.client_api_key_id.clone()),
        request_id: non_empty(query.request_id.clone()),
        provider_account_ref: non_empty(query.account_id.clone()),
        operation: non_empty(query.route.clone()),
        provider_kind: non_empty(query.provider.clone()),
        model: non_empty(query.model.clone()),
        outcome: request_outcome(outcome)?,
        status_code: parse_status(query.status_code)?,
        transport: non_empty(query.transport.clone()),
        attempt_index: parse_attempt_index(query.attempt_index)?,
        response_id: non_empty(query.response_id.clone()),
        upstream_request_id: non_empty(query.upstream_request_id.clone()),
        search: non_empty(query.search.clone()),
    })
}

fn usage_command(
    query: &UsageQuery,
) -> Result<(domain::UsageQuery, u32, u16), WireValidationError> {
    let (page, page_size) = query.validate_page()?;
    query.validate_cursor()?;
    let page_number = NonZeroU32::new(page)
        .map(domain::PageNumber::new)
        .ok_or_else(|| WireValidationError::new("page"))?;
    let page_size_value =
        DomainPageSize::new(page_size).map_err(|_| WireValidationError::new("pageSize"))?;
    Ok((
        domain::UsageQuery {
            range: usage_range(query.start_time.as_deref(), query.end_time.as_deref())?,
            filter: usage_filter(query)?,
            cursor: decode_observability_cursor(query.cursor.as_deref())?,
            page: page_number,
            page_size: page_size_value,
        },
        page,
        page_size,
    ))
}

fn ops_command(query: &OpsQuery) -> Result<(domain::OpsErrorQuery, u32, u16), WireValidationError> {
    let (page, page_size) = query.validate_page()?;
    query.validate_cursor()?;
    let page_number = NonZeroU32::new(page)
        .map(domain::PageNumber::new)
        .ok_or_else(|| WireValidationError::new("page"))?;
    let page_size_value =
        DomainPageSize::new(page_size).map_err(|_| WireValidationError::new("pageSize"))?;
    let status_code = parse_status(
        query
            .upstream_status_code
            .or(query.client_status_code)
            .or(query.status_code),
    )?;
    Ok((
        domain::OpsErrorQuery {
            range: usage_range(query.start_time.as_deref(), query.end_time.as_deref())?,
            filter: domain::OpsErrorFilter {
                client_api_key_ref: non_empty(query.client_api_key_id.clone()),
                request_id: non_empty(query.request_id.clone()),
                provider_kind: non_empty(query.provider.clone()),
                provider_account_ref: non_empty(query.account_id.clone()),
                operation: non_empty(query.route.clone()).or_else(|| non_empty(query.kind.clone())),
                model: non_empty(query.model.clone()),
                transport: non_empty(query.transport.clone()),
                attempt_index: parse_attempt_index(query.attempt_index)?,
                response_id: non_empty(query.response_id.clone()),
                upstream_request_id: non_empty(query.upstream_request_id.clone()),
                failure_kind: non_empty(query.failure_class.clone()),
                status_code,
                search: non_empty(query.search.clone()),
            },
            cursor: decode_observability_cursor(query.cursor.as_deref())?,
            page: page_number,
            page_size: page_size_value,
        },
        page,
        page_size,
    ))
}

fn decode_observability_cursor(
    value: Option<&str>,
) -> Result<Option<domain::ObservabilityCursor>, WireValidationError> {
    value
        .map(|encoded| {
            if encoded.is_empty() || encoded.len() > MAX_CURSOR_BYTES {
                return Err(WireValidationError::new("cursor"));
            }
            let bytes = URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| WireValidationError::new("cursor"))?;
            let wire: CursorWire =
                serde_json::from_slice(&bytes).map_err(|_| WireValidationError::new("cursor"))?;
            if wire.stable_id.trim().is_empty() {
                return Err(WireValidationError::new("cursor"));
            }
            Ok(domain::ObservabilityCursor {
                observed_at: wire.observed_at,
                stable_id: wire.stable_id,
            })
        })
        .transpose()
}

fn encode_observability_cursor(
    cursor: &domain::ObservabilityCursor,
) -> Result<String, WireValidationError> {
    let bytes = serde_json::to_vec(&CursorWire {
        observed_at: cursor.observed_at,
        stable_id: cursor.stable_id.clone(),
    })
    .map_err(|_| WireValidationError::new("cursor"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
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
    pub image_input_tokens: Option<u64>,
    pub image_output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub input_tokens_display: String,
    pub output_tokens_display: String,
    pub cached_tokens_display: String,
    pub cache_write_tokens_display: String,
    pub reasoning_tokens_display: String,
    pub image_input_tokens_display: String,
    pub image_output_tokens_display: String,
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
    pub authentication_kind: Option<String>,
    pub account_id: Option<String>,
    pub account_email: Option<String>,
    pub route: String,
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
    pub image_input_tokens: Option<u64>,
    pub image_output_tokens: Option<u64>,
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
    pub requested_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_preset: Option<String>,
    pub compact: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_status_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_status_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_request_id: Option<String>,
    pub websocket_pool: Option<String>,
    pub image_generation_requested: bool,
    pub image_generation_succeeded: Option<bool>,
    pub latency_details: UsageLatencyDetailsView,
}

/// 逻辑请求在上游和输出阶段测得的时延事实。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLatencyDetailsView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_decision_wait_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_connect_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_headers_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_event_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_reasoning_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_text_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_processing_ms: Option<u64>,
}

/// 单次上游尝试展示。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageAttemptView {
    pub id: String,
    pub attempt_index: u32,
    pub trigger: String,
    pub provider: String,
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
    pub provider: String,
    pub authentication_kind: String,
    pub email: String,
    pub plan_type: Option<String>,
    pub tokens: String,
    pub request_count: u64,
    pub request_buckets: Vec<DashboardAccountRequestBucketView>,
    pub quota_used_percent: Option<f64>,
    pub last_used: String,
}

/// Dashboard 账号单小时请求数。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardAccountRequestBucketView {
    pub bucket_start: DateTime<Utc>,
    pub request_count: u64,
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

/// Dashboard 展示的实际 Provider 上游请求身份。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardWireProfileView {
    pub provider: String,
    pub product: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    pub target: DashboardWireTargetView,
    pub user_agent: String,
    pub attributes: Vec<DashboardWireAttributeView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<DashboardDesktopReleaseView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardWireAttributeView {
    pub label: String,
    pub value: String,
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
    pub status: DashboardDesktopReleaseStatusView,
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

/// Dashboard 发布检查的稳定 wire 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardDesktopReleaseStatusView {
    Unchecked,
    Aligned,
    ReviewRequired,
    CheckFailed,
}

impl From<domain::DesktopReleaseStatus> for DashboardDesktopReleaseStatusView {
    fn from(status: domain::DesktopReleaseStatus) -> Self {
        match status {
            domain::DesktopReleaseStatus::Unchecked => Self::Unchecked,
            domain::DesktopReleaseStatus::Current => Self::Aligned,
            domain::DesktopReleaseStatus::UpdateAvailable => Self::ReviewRequired,
            domain::DesktopReleaseStatus::Failed => Self::CheckFailed,
        }
    }
}

/// Dashboard 汇总响应数据。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardDataView {
    pub cards: DashboardCardsView,
    pub trend: TrendData,
    pub health_timeline: HealthTimelineView,
    pub wire_profiles: Vec<DashboardWireProfileView>,
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
    pub account_label: Option<String>,
}

pub fn router<S>() -> Router<S>
where
    S: AdminSessionState + Clone + Send + Sync + 'static,
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
            "/api/admin/usage/insights/overview",
            get(usage_insights_overview::<S>),
        )
        .route(
            "/api/admin/usage/insights/diagnostics",
            get(usage_insights_diagnostics::<S>),
        )
        .route("/api/admin/operations/errors", get(ops_errors::<S>))
}

async fn dashboard_summary<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<DashboardQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let kind = query.trend_kind().map_err(map_wire_error)?;
    let range = dashboard_range(query.start_time.as_deref(), query.end_time.as_deref())
        .map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .observability()
        .dashboard_summary(range, domain_trend_kind(kind))
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(dashboard_view(result, kind)),
    ))
}

async fn dashboard_trend<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<DashboardQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let kind = query.trend_kind().map_err(map_wire_error)?;
    let range = dashboard_today_range(query.start_time.as_deref(), query.end_time.as_deref())
        .map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .observability()
        .dashboard_trend(range, domain_trend_kind(kind))
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(trend_view(result, kind)),
    ))
}

async fn usage_records<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<UsageQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let (command, page, page_size) = usage_command(&query).map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .observability()
        .usage_records(command)
        .await
        .map_err(map_service_error)?;
    let data = usage_page_view(result, page, page_size)
        .map_err(|_| AdminError::internal("Failed to encode observability cursor"))?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

async fn usage_record_detail<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<DetailQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    query.validate().map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .observability()
        .usage_record_detail(query.id.trim())
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(usage_detail_view(result)),
    ))
}

async fn usage_records_summary<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<UsageQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let range = usage_range(query.start_time.as_deref(), query.end_time.as_deref())
        .map_err(map_wire_error)?;
    let filter = usage_filter(&query).map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .observability()
        .usage_summary(range, filter)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(usage_summary_view(result)),
    ))
}

async fn usage_insights_overview<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<UsageQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let range = usage_range(query.start_time.as_deref(), query.end_time.as_deref())
        .map_err(map_wire_error)?;
    let filter = usage_filter(&query).map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .observability()
        .usage_insights(range, filter)
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(usage_insights_view(result)),
    ))
}

async fn usage_insights_diagnostics<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<DiagnosticsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let dimension = query.dimension().map_err(map_wire_error)?;
    let range = usage_range(query.start_time.as_deref(), query.end_time.as_deref())
        .map_err(map_wire_error)?;
    let filter = domain::UsageFilter {
        provider_kind: non_empty(query.provider),
        model: non_empty(query.model),
        status_code: parse_status(query.status_code).map_err(map_wire_error)?,
        search: non_empty(query.search),
        ..domain::UsageFilter::default()
    };
    let result = state
        .admin_services()
        .observability()
        .diagnostics(range, filter, domain_diagnostic_dimension(dimension))
        .await
        .map_err(map_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(diagnostics_view(result, dimension)),
    ))
}

async fn ops_errors<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    Query(query): Query<OpsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: AdminSessionState + Send + Sync,
{
    let (command, page, page_size) = ops_command(&query).map_err(map_wire_error)?;
    let result = state
        .admin_services()
        .observability()
        .ops_errors(command)
        .await
        .map_err(map_service_error)?;
    let data = ops_page_view(result, page, page_size)
        .map_err(|_| AdminError::internal("Failed to encode observability cursor"))?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

fn format_number(value: u64) -> String {
    let text = value.to_string();
    let mut output = String::with_capacity(text.len() + text.len() / 3);
    for (index, character) in text.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            output.push(',');
        }
        output.push(character);
    }
    output.chars().rev().collect()
}

fn format_compact_number(value: u64) -> String {
    if value < 1_000 {
        return format_number(value);
    }
    for (suffix, threshold) in [
        ("P", 1_000_000_000_000_000_u64),
        ("T", 1_000_000_000_000_u64),
        ("B", 1_000_000_000_u64),
        ("M", 1_000_000_u64),
        ("K", 1_000_u64),
    ] {
        if value >= threshold {
            let scaled = value as f64 / threshold as f64;
            return format!("{scaled:.1}{suffix}").replace(".0", "");
        }
    }
    format_number(value)
}

fn display_duration(value: Option<u64>) -> String {
    let Some(value) = value.and_then(|value| i64::try_from(value).ok()) else {
        return "—".to_owned();
    };
    if value < 1_000 {
        format!("{value} ms")
    } else if value < 60_000 {
        format!("{:.2} s", value as f64 / 1_000.0)
    } else if value < 3_600_000 {
        format!("{:.1} min", value as f64 / 60_000.0)
    } else {
        format!("{:.1} h", value as f64 / 3_600_000.0)
    }
}

fn display_rate(value: f64) -> String {
    if value.is_finite() {
        format!("{:.1}%", value * 100.0)
    } else {
        "—".to_owned()
    }
}

fn china_day_start(value: DateTime<Utc>) -> DateTime<Utc> {
    const CHINA_OFFSET_SECONDS: i64 = 8 * 60 * 60;
    let elapsed = (value.timestamp() + CHINA_OFFSET_SECONDS).rem_euclid(24 * 60 * 60);
    value - Duration::seconds(elapsed) - Duration::nanoseconds(i64::from(value.nanosecond()))
}

fn china_datetime(value: &DateTime<Utc>) -> String {
    (*value + Duration::hours(8))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

fn china_label(value: DateTime<Utc>, format: &str) -> String {
    (value + Duration::hours(8)).format(format).to_string()
}

fn outcome_name(outcome: &domain::RequestOutcome) -> &str {
    outcome.as_str()
}

fn request_metrics_view(metrics: &domain::RequestMetrics) -> RequestMetricsView {
    RequestMetricsView {
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
    }
}

fn cost_coverage_view(coverage: &domain::CostCoverage) -> CostCoverageView {
    CostCoverageView {
        known: coverage.known_count(),
        partial: coverage.partial_count,
        unknown: coverage.unavailable_count,
        not_billable: coverage.not_billable_count,
    }
}

fn cost_views(costs: &[domain::CurrencyCost]) -> Vec<CostView> {
    costs
        .iter()
        .map(|cost| CostView {
            currency: cost.currency.clone(),
            estimated_amount: cost.amount.as_str().to_owned(),
        })
        .collect()
}

fn attempt_metrics_view(metrics: &domain::AttemptMetrics) -> AttemptMetricsView {
    AttemptMetricsView {
        attempt_count: metrics.attempt_count,
        success_count: metrics.success_count,
        failure_count: metrics.failure_count,
        cancelled_count: metrics.cancelled_count,
        incomplete_count: metrics.incomplete_count,
        rate_limited_count: metrics.rate_limited_count,
        auth_failure_count: metrics.auth_failure_count,
        provider5xx_count: metrics.provider_5xx_count,
        cost_coverage: cost_coverage_view(&metrics.cost_coverage),
        costs: cost_views(&metrics.costs),
    }
}

fn token_details(record: &domain::UsageRecord) -> TokenDetailsView {
    TokenDetailsView {
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        cached_tokens: record.cached_tokens,
        cache_write_tokens: record.cache_write_tokens,
        reasoning_tokens: record.reasoning_tokens,
        image_input_tokens: record.image_input_tokens,
        image_output_tokens: record.image_output_tokens,
        total_tokens: record.total_tokens,
        input_tokens_display: record
            .input_tokens
            .map_or_else(|| "-".to_owned(), format_number),
        output_tokens_display: record
            .output_tokens
            .map_or_else(|| "-".to_owned(), format_number),
        cached_tokens_display: record
            .cached_tokens
            .map_or_else(|| "-".to_owned(), format_compact_number),
        cache_write_tokens_display: record
            .cache_write_tokens
            .map_or_else(|| "-".to_owned(), format_compact_number),
        reasoning_tokens_display: record
            .reasoning_tokens
            .map_or_else(|| "-".to_owned(), format_number),
        image_input_tokens_display: record
            .image_input_tokens
            .map_or_else(|| "-".to_owned(), format_number),
        image_output_tokens_display: record
            .image_output_tokens
            .map_or_else(|| "-".to_owned(), format_number),
        total_tokens_display: record
            .total_tokens
            .map_or_else(|| "-".to_owned(), format_number),
    }
}

fn format_decimal_currency(amount: &str, currency: &str) -> String {
    if currency == "USD" {
        let value = amount.parse::<f64>().map_or(0.0, |value| value);
        let precision = if value != 0.0 && value.abs() < 0.01 {
            4
        } else {
            2
        };
        format!("${value:.precision$}")
    } else {
        format!("{currency} {amount}")
    }
}

fn format_money(cost: &domain::CurrencyCost) -> String {
    format_decimal_currency(cost.amount.as_str(), &cost.currency)
}

fn format_token_price(cost: &domain::CurrencyCost) -> String {
    if cost.currency != "USD" {
        return format!("{} {} / 1M Token", cost.currency, cost.amount.as_str());
    }
    let value = cost
        .amount
        .as_str()
        .parse::<f64>()
        .map_or(0.0, |value| value);
    format!("${value:.4} / 1M Token")
}

fn format_service_tier(service_tier: Option<&str>) -> String {
    match service_tier {
        Some("priority" | "fast") => "Fast".to_owned(),
        Some("flex") => "Flex".to_owned(),
        Some("default") | None => "Default".to_owned(),
        Some(other) => other.to_owned(),
    }
}

fn billing_view(billing: Option<&domain::UsageBilling>) -> Option<BillingView> {
    match billing? {
        domain::UsageBilling::Total { total, .. } => Some(BillingView {
            input_amount_display: "—".to_owned(),
            output_amount_display: "—".to_owned(),
            cache_read_amount_display: "—".to_owned(),
            cache_write_amount_display: "—".to_owned(),
            standard_amount_display: "—".to_owned(),
            total_amount_display: format_money(total),
            input_price_display: "—".to_owned(),
            output_price_display: "—".to_owned(),
            cache_read_price_display: "—".to_owned(),
            cache_write_price_display: "—".to_owned(),
            service_tier_display: "—".to_owned(),
            multiplier_display: "—".to_owned(),
        }),
        domain::UsageBilling::Calculated(value) => Some(BillingView {
            input_amount_display: format_money(&value.input_amount),
            output_amount_display: format_money(&value.output_amount),
            cache_read_amount_display: format_money(&value.cache_read_amount),
            cache_write_amount_display: format_money(&value.cache_write_amount),
            standard_amount_display: format_money(&value.standard_amount),
            total_amount_display: format_money(&value.total_amount),
            input_price_display: format_token_price(&value.input_price_per_million),
            output_price_display: format_token_price(&value.output_price_per_million),
            cache_read_price_display: format_token_price(&value.cache_read_price_per_million),
            cache_write_price_display: format_token_price(&value.cache_write_price_per_million),
            service_tier_display: format_service_tier(value.service_tier.as_deref()),
            multiplier_display: format!("{:.2}x", f64::from(value.multiplier_percent) / 100.0),
        }),
    }
}

fn usage_record_view(record: domain::UsageRecord) -> UsageRecordView {
    let tokens = token_details(&record);
    let billing = billing_view(record.billing.as_ref());
    let costs = record
        .cost_amount
        .as_ref()
        .zip(record.cost_currency.as_ref())
        .map(|(amount, currency)| {
            vec![CostView {
                currency: currency.clone(),
                estimated_amount: amount.as_str().to_owned(),
            }]
        })
        .unwrap_or_default();
    let status_code = record
        .client_status_code
        .or(record.upstream_status_code)
        .map(i64::from);
    let outcome = outcome_name(&record.outcome).to_owned();
    let message = record
        .error_message
        .clone()
        .unwrap_or_else(|| outcome.clone());
    let first_token_display = display_duration(record.first_token_ms);
    let latency_display = display_duration(record.latency_ms);
    let cost_coverage = match record.cost_source.as_str() {
        "provider_reported" | "calculated" => CostCoverageView {
            known: 1,
            partial: 0,
            unknown: 0,
            not_billable: 0,
        },
        _ => CostCoverageView {
            known: 0,
            partial: 0,
            unknown: 1,
            not_billable: 0,
        },
    };
    let model = record
        .upstream_model_id
        .clone()
        .unwrap_or_else(|| record.requested_model_id.clone());
    let transport = record
        .upstream_transport
        .clone()
        .or_else(|| Some(record.client_transport.clone()));
    let metadata = UsageRecordMetadataView {
        protocol: record.protocol.clone(),
        logical_outcome: outcome.clone(),
        attempt_count: u64::from(record.attempt_count),
        requested_model: record.requested_model_id.clone(),
        upstream_model: record.upstream_model_id.clone(),
        client_ip: record.client_ip.clone(),
        user_agent: record.user_agent.clone(),
        reasoning_effort: record.reasoning_effort.clone(),
        reasoning_preset: record.reasoning_preset.clone(),
        compact: record.compact,
        request_kind: record.request_kind.clone(),
        subagent_kind: record.subagent_kind.clone(),
        transport: transport.clone(),
        http_version: record.http_version.clone(),
        client_status_code: record.client_status_code.map(i64::from),
        upstream_status_code: record.upstream_status_code.map(i64::from),
        response_id: record.client_response_id.clone(),
        upstream_request_id: record.upstream_request_id.clone(),
        websocket_pool: record.websocket_pool.clone(),
        image_generation_requested: record.image_generation_requested,
        image_generation_succeeded: record.image_generation_succeeded,
        latency_details: UsageLatencyDetailsView {
            transport_decision_wait_ms: record.transport_decision_wait_ms,
            ws_connect_ms: record.connect_ms,
            upstream_headers_ms: record.headers_ms,
            first_event_ms: record.first_event_ms,
            first_reasoning_ms: record.first_reasoning_ms,
            first_text_ms: record.first_text_ms,
            first_token_ms: record.first_token_ms,
            openai_processing_ms: record.provider_processing_ms,
        },
    };
    UsageRecordView {
        id: record.id.clone(),
        request_id: record.id,
        client_api_key_id: Some(record.client_api_key_ref),
        kind: record.operation,
        provider: record.provider_kind,
        authentication_kind: record.provider_account_authentication_kind,
        account_id: record.provider_account_ref,
        account_email: record.provider_account_email,
        route: record.endpoint,
        model,
        requested_model: Some(record.requested_model_id),
        upstream_model: record.upstream_model_id,
        service_tier: None,
        status_code,
        transport,
        attempt_index: None,
        attempt_count: u64::from(record.attempt_count),
        response_id: record.client_response_id,
        upstream_request_id: record.upstream_request_id,
        latency_ms: record.latency_ms,
        first_token_ms: record.first_token_ms,
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        cached_tokens: record.cached_tokens,
        cache_write_tokens: record.cache_write_tokens,
        reasoning_tokens: record.reasoning_tokens,
        image_input_tokens: record.image_input_tokens,
        image_output_tokens: record.image_output_tokens,
        message,
        metadata,
        created_at: record.started_at,
        created_at_display: china_datetime(&record.started_at),
        client_ip: record.client_ip,
        user_agent: record.user_agent,
        reasoning_effort: record.reasoning_effort,
        reasoning_preset: record.reasoning_preset,
        compact: Some(record.compact),
        request_kind: record.request_kind,
        subagent_kind: record.subagent_kind,
        token_details: tokens,
        billing,
        costs,
        cost_coverage,
        first_token_latency_ms: record.first_token_ms,
        first_token_latency_ms_display: first_token_display,
        latency_ms_display: latency_display,
        logical_outcome: outcome,
    }
}

fn usage_attempt_view(attempt: domain::UsageAttempt) -> UsageAttemptView {
    let occurred_at = attempt.occurred_at;
    UsageAttemptView {
        id: attempt.id,
        attempt_index: attempt.attempt_index,
        trigger: attempt.source,
        provider: attempt
            .provider_kind
            .unwrap_or_else(|| "unknown".to_owned()),
        model: attempt
            .upstream_model_id
            .unwrap_or_else(|| "unknown".to_owned()),
        transport: attempt
            .upstream_transport
            .unwrap_or_else(|| "unknown".to_owned()),
        send_state: attempt
            .upstream_send_state
            .unwrap_or_else(|| "unknown".to_owned()),
        outcome: outcome_name(&attempt.outcome).to_owned(),
        downstream_committed: attempt.downstream_committed,
        status_code: attempt.status_code,
        provider_error_code: attempt.provider_error_code,
        failure_class: attempt.failure_kind,
        cost_estimate_status: attempt
            .cost_source
            .clone()
            .unwrap_or_else(|| "unavailable".to_owned()),
        estimated_cost_amount: attempt.cost_amount.map(|amount| amount.as_str().to_owned()),
        estimated_cost_currency: attempt.cost_currency,
        input_tokens: attempt.input_tokens,
        output_tokens: attempt.output_tokens,
        cached_tokens: attempt.cached_tokens,
        total_tokens: attempt.total_tokens,
        first_token_ms: None,
        latency_ms: attempt.latency_ms,
        credential_name: attempt.provider_account_ref,
        account_email: None,
        started_at: occurred_at,
        completed_at: Some(occurred_at),
    }
}

fn usage_detail_view(detail: domain::UsageDetail) -> UsageRecordDetailView {
    UsageRecordDetailView {
        request: usage_record_view(detail.request),
        attempts: detail
            .attempts
            .into_iter()
            .map(usage_attempt_view)
            .collect(),
    }
}

fn page_meta(page: u32, page_size: u16, total: u64) -> PageMeta {
    let page_size_u64 = u64::from(page_size);
    let total_pages = total.saturating_add(page_size_u64 - 1) / page_size_u64;
    PageMeta::new(
        page,
        page_size,
        total,
        u32::try_from(total_pages).unwrap_or(u32::MAX),
    )
}

fn usage_page_view(
    page: domain::UsagePage,
    page_number: u32,
    page_size: u16,
) -> Result<PageData<UsageRecordView>, WireValidationError> {
    Ok(PageData {
        items: page.items.into_iter().map(usage_record_view).collect(),
        page: page_meta(page_number, page_size, page.total),
        next_cursor: page
            .next_cursor
            .as_ref()
            .map(encode_observability_cursor)
            .transpose()?,
    })
}

fn ops_error_view(error: domain::OpsError) -> OpsErrorView {
    let status = error.status_code.map(i64::from);
    OpsErrorView {
        id: error.event_id,
        request_id: error.request_id,
        client_api_key_id: error.client_api_key_ref,
        kind: error.operation.clone(),
        provider: error.provider_kind,
        account_id: error.provider_account_ref,
        route: error.operation,
        model: error.upstream_model_id,
        status_code: status,
        client_status_code: None,
        upstream_status_code: None,
        transport: error.upstream_transport,
        attempt_index: error.attempt_index,
        failure_class: error.failure_kind,
        response_id: error.client_response_id,
        upstream_request_id: error.upstream_request_id,
        latency_ms: error.latency_ms,
        message: error.message,
        metadata: OpsErrorMetadataView {
            source: error.source,
            component: error.component,
            attempt_id: None,
            account_label: None,
        },
        created_at: error.occurred_at,
        created_at_display: china_datetime(&error.occurred_at),
    }
}

fn ops_page_view(
    page: domain::OpsErrorPage,
    page_number: u32,
    page_size: u16,
) -> Result<PageData<OpsErrorView>, WireValidationError> {
    Ok(PageData {
        items: page.items.into_iter().map(ops_error_view).collect(),
        page: page_meta(page_number, page_size, page.total),
        next_cursor: page
            .next_cursor
            .as_ref()
            .map(encode_observability_cursor)
            .transpose()?,
    })
}

fn trend_point_view(point: domain::TrendPoint) -> TrendPointView {
    let local_time = china_label(point.bucket_start, "%H:%M");
    let label = china_label(point.bucket_start, "%m-%d %H:%M");
    let success_rate_value = point.success_rate.map(|value| value * 100.0);
    TrendPointView {
        time: local_time,
        bucket: point.bucket_start,
        label,
        requests: format_compact_number(point.metrics.request_count),
        requests_value: point.metrics.request_count,
        input_tokens: format_compact_number(point.metrics.input_tokens),
        input_tokens_value: point.metrics.input_tokens,
        output_tokens: format_compact_number(point.metrics.output_tokens),
        output_tokens_value: point.metrics.output_tokens,
        cached_tokens: format_compact_number(point.metrics.cached_tokens),
        cached_tokens_value: point.metrics.cached_tokens,
        cache_hit_rate_value: point.cached_token_rate,
        tokens_value: point.metrics.total_tokens,
        errors: format_compact_number(point.service_failure_count),
        errors_value: point.service_failure_count,
        latency: display_duration(point.average_latency_ms),
        latency_value: point.average_latency_ms,
        first_token_latency: display_duration(point.average_first_token_latency_ms),
        first_token_latency_value: point.average_first_token_latency_ms,
        max_latency: display_duration(point.metrics.max_latency_ms),
        max_latency_value: point.metrics.max_latency_ms,
        min_latency: display_duration(point.metrics.min_latency_ms),
        min_latency_value: point.metrics.min_latency_ms,
        success_rate: success_rate_value
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}%")),
        success_rate_value,
    }
}

fn trend_summary_view(kind: TrendKind, summary: &domain::TrendSummary) -> Vec<TrendSummaryView> {
    match kind {
        TrendKind::Usage => vec![
            TrendSummaryView {
                label: "输入".to_owned(),
                value: format_compact_number(summary.input_tokens),
                ratio: None,
            },
            TrendSummaryView {
                label: "输出".to_owned(),
                value: format_compact_number(summary.output_tokens),
                ratio: None,
            },
            TrendSummaryView {
                label: "缓存".to_owned(),
                value: format_compact_number(summary.cached_tokens),
                ratio: None,
            },
        ],
        TrendKind::Latency => vec![
            TrendSummaryView {
                label: "平均".to_owned(),
                value: display_duration(summary.average_latency_ms),
                ratio: None,
            },
            TrendSummaryView {
                label: "最高".to_owned(),
                value: display_duration(summary.max_latency_ms),
                ratio: None,
            },
            TrendSummaryView {
                label: "最低".to_owned(),
                value: display_duration(summary.min_latency_ms),
                ratio: None,
            },
        ],
        TrendKind::Errors => vec![
            TrendSummaryView {
                label: "错误数".to_owned(),
                value: format_compact_number(summary.service_failure_count),
                ratio: None,
            },
            TrendSummaryView {
                label: "成功率".to_owned(),
                value: "—".to_owned(),
                ratio: summary
                    .success_rate
                    .map(|value| format!("{:.1}%", value * 100.0)),
            },
            TrendSummaryView {
                label: "总请求".to_owned(),
                value: format_compact_number(summary.request_count),
                ratio: None,
            },
        ],
    }
}

fn trend_view(trend: domain::Trend, kind: TrendKind) -> TrendData {
    TrendData {
        kind,
        summary: trend_summary_view(kind, &trend.summary),
        points: trend.points.into_iter().map(trend_point_view).collect(),
    }
}

fn health_status_name(status: domain::HealthStatus) -> &'static str {
    match status {
        domain::HealthStatus::Future => "future",
        domain::HealthStatus::NoData => "no_data",
        domain::HealthStatus::Unavailable => "unavailable",
        domain::HealthStatus::LowSample => "low_sample",
        domain::HealthStatus::Unstable => "unstable",
        domain::HealthStatus::Stable => "stable",
    }
}

fn reliability_display(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| format!("{value:.1}%"))
}

fn health_timeline_view(timeline: domain::HealthTimeline) -> HealthTimelineView {
    HealthTimelineView {
        title: "请求健康时间线".to_owned(),
        description: "有效请求可用性".to_owned(),
        reliability_display: reliability_display(timeline.reliability_percent),
        status: health_status_name(timeline.status).to_owned(),
        success_requests: timeline.success_requests,
        failed_requests: timeline.failed_requests,
        cancelled_requests: timeline.cancelled_requests,
        incomplete_requests: timeline.incomplete_requests,
        caller_error_requests: timeline.caller_error_requests,
        points: timeline
            .points
            .into_iter()
            .enumerate()
            .map(|(index, point)| {
                let elapsed_minutes = i64::try_from(index).unwrap_or(i64::MAX).saturating_mul(15);
                HealthTimelinePointView {
                    time: format!("{:02}:{:02}", elapsed_minutes / 60, elapsed_minutes % 60),
                    status: health_status_name(point.status).to_owned(),
                    reliability_display: reliability_display(point.reliability_percent),
                    success_requests: point.success_requests,
                    failed_requests: point.failed_requests,
                    cancelled_requests: point.cancelled_requests,
                    incomplete_requests: point.incomplete_requests,
                    caller_error_requests: point.caller_error_requests,
                }
            })
            .collect(),
    }
}

fn wire_profile_view(profile: domain::DashboardWireProfile) -> DashboardWireProfileView {
    DashboardWireProfileView {
        provider: profile.provider,
        product: profile.product,
        version: profile.version,
        build: profile.build,
        target: DashboardWireTargetView {
            os_type: profile.target.os_type,
            os_version: profile.target.os_version,
            arch: profile.target.arch,
            terminal: profile.target.terminal,
        },
        user_agent: profile.user_agent,
        attributes: profile
            .attributes
            .into_iter()
            .map(|attribute| DashboardWireAttributeView {
                label: attribute.label,
                value: attribute.value,
            })
            .collect(),
        verified_at: profile.verified_at,
        release: profile.release.map(|release| DashboardDesktopReleaseView {
            status: release.status.into(),
            checked_at: release.checked_at,
            latest_version: release.latest_version,
            latest_build: release.latest_build,
            published_at: release.published_at,
            minimum_system_version: release.minimum_system_version,
            hardware_requirements: release.hardware_requirements,
            download_url: release.download_url,
            download_size: release.download_size,
            signature_present: release.signature_present,
            error: release.error,
        }),
    }
}

fn relative_time(value: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(value) = value else {
        return "从未使用".to_owned();
    };
    let elapsed = now.signed_duration_since(value);
    if elapsed.num_seconds() < 0 {
        return china_datetime(&value);
    }
    if elapsed.num_seconds() < 60 {
        return "刚刚".to_owned();
    }
    if elapsed.num_minutes() < 60 {
        return format!("{} 分钟前", elapsed.num_minutes());
    }
    if elapsed.num_hours() < 24 {
        return format!("{} 小时前", elapsed.num_hours());
    }
    format!("{} 天前", elapsed.num_days())
}

fn rotation_strategy_name(
    strategy: gateway_admin::model::settings::RotationStrategy,
) -> &'static str {
    match strategy {
        gateway_admin::model::settings::RotationStrategy::Smart => "smart",
        gateway_admin::model::settings::RotationStrategy::QuotaResetPriority => {
            "quota_reset_priority"
        }
        gateway_admin::model::settings::RotationStrategy::RoundRobin => "round_robin",
        gateway_admin::model::settings::RotationStrategy::Sticky => "sticky",
    }
}

fn dashboard_view(result: domain::DashboardResult, kind: TrendKind) -> DashboardDataView {
    let domain::DashboardResult {
        observation,
        today,
        yesterday,
        total_billing_usd,
        total_cached_token_rate,
        average_first_token_latency_ms,
        trend,
        health_timeline,
        wire_profiles,
        capacity,
        rotation_strategy,
    } = result;
    let domain::DashboardObservation {
        range,
        requests,
        attempts,
        provider_accounts,
        trend: _,
        account_usage,
        recent_requests,
    } = observation;
    let mut account_usage_views = Vec::with_capacity(account_usage.len());
    let mut credential_usage_views = Vec::with_capacity(account_usage.len());
    for credential in account_usage {
        account_usage_views.push(DashboardAccountUsageView {
            id: credential.account_id.clone(),
            provider: credential.provider_kind.clone(),
            authentication_kind: credential.authentication_kind.clone(),
            email: credential
                .email
                .clone()
                .unwrap_or_else(|| credential.name.clone()),
            plan_type: credential.plan_type.clone(),
            tokens: credential
                .total_tokens
                .map_or_else(|| "—".to_owned(), format_compact_number),
            request_count: credential.request_count,
            request_buckets: credential
                .request_buckets
                .iter()
                .map(|bucket| DashboardAccountRequestBucketView {
                    bucket_start: bucket.bucket_start,
                    request_count: bucket.request_count,
                })
                .collect(),
            quota_used_percent: credential.quota_used_percent,
            last_used: relative_time(credential.last_used_at, range.end),
        });
        credential_usage_views.push(DashboardCredentialUsageView {
            id: credential.account_id,
            display_name: credential.email.unwrap_or(credential.name),
            plan_type: credential.plan_type,
            tokens: credential
                .total_tokens
                .map_or_else(|| "-".to_owned(), format_compact_number),
            tokens_value: credential.total_tokens,
            last_used: credential
                .last_used_at
                .map_or_else(|| "-".to_owned(), |value| china_datetime(&value)),
            provider: credential.provider_kind,
            availability: credential.availability,
            request_count: credential.request_count,
        });
    }
    DashboardDataView {
        cards: DashboardCardsView {
            credentials: DashboardCredentialsCardView {
                total: format_compact_number(provider_accounts.total),
                total_value: provider_accounts.total,
                enabled: format_compact_number(provider_accounts.enabled),
                enabled_value: provider_accounts.enabled,
                unavailable: format_compact_number(provider_accounts.unavailable),
                unavailable_value: provider_accounts.unavailable,
            },
            traffic: DashboardTrafficCardView {
                today_requests: format_compact_number(today.request_count),
                today_requests_value: today.request_count,
                yesterday_requests_value: yesterday.request_count,
                total_requests: format_compact_number(requests.request_count),
            },
            tokens: DashboardTokensCardView {
                today_tokens: format_compact_number(today.total_tokens),
                today_tokens_value: today.total_tokens,
                yesterday_tokens_value: yesterday.total_tokens,
                total_tokens: format_compact_number(requests.total_tokens),
                total_billing_amount_usd: total_billing_usd
                    .as_ref()
                    .map_or_else(|| "—".to_owned(), |amount| format!("${}", amount.as_str())),
            },
            cache: DashboardCacheCardView {
                today_hit_rate: display_rate(today.cached_token_rate),
                today_hit_rate_value: today.observed_cached_token_rate,
                yesterday_hit_rate_value: yesterday.observed_cached_token_rate,
                total_hit_rate: display_rate(total_cached_token_rate),
                total_cached_tokens: format_compact_number(requests.cached_tokens),
                average_first_token_latency_ms: display_duration(average_first_token_latency_ms),
            },
        },
        trend: trend_view(trend, kind),
        health_timeline: health_timeline_view(health_timeline),
        wire_profiles: wire_profiles.into_iter().map(wire_profile_view).collect(),
        account_usage: account_usage_views,
        credential_usage: credential_usage_views,
        usage_records: recent_requests.into_iter().map(usage_record_view).collect(),
        pool_summary: DashboardPoolSummaryView {
            total: provider_accounts.total,
            active: provider_accounts.active,
            expired: provider_accounts.expired,
            quota_exhausted: provider_accounts.quota_exhausted,
            refreshing: provider_accounts.refreshing,
            disabled: provider_accounts.disabled,
            banned: provider_accounts.banned,
        },
        capacity_info: DashboardCapacityInfoView {
            max_concurrent_per_account: capacity.max_concurrent_per_account,
            total_slots: capacity.total_slots,
            used_slots: capacity.used_slots,
            available_slots: capacity.available_slots,
        },
        rotation_strategy: rotation_strategy_name(rotation_strategy).to_owned(),
        logical_requests: request_metrics_view(&requests),
        attempts: attempt_metrics_view(&attempts),
        costs: cost_views(&attempts.costs),
    }
}

fn usage_summary_view(summary: domain::UsageSummary) -> UsageSummaryView {
    let overview = summary.overview;
    UsageSummaryView {
        total_requests: format_compact_number(overview.requests.request_count),
        input_tokens: format_compact_number(overview.requests.input_tokens),
        output_tokens: format_compact_number(overview.requests.output_tokens),
        cached_tokens: format_compact_number(overview.requests.cached_tokens),
        cache_write_tokens: format_compact_number(overview.requests.cache_write_tokens),
        total_tokens: format_compact_number(overview.requests.total_tokens),
        average_latency_ms: display_duration(summary.average_latency_ms),
        logical_requests: request_metrics_view(&overview.requests),
        attempts: attempt_metrics_view(&overview.attempts),
    }
}

fn usage_insights_view(insights: domain::UsageInsights) -> UsageInsightsOverviewView {
    let health_points = insights
        .health
        .points
        .iter()
        .map(|point| OverviewHealthPointView {
            bucket: point.bucket_start,
            label: china_label(point.bucket_start, "%m-%d %H:%M"),
            success_requests: point.success_requests,
            failed_requests: point.failed_requests,
            cancelled_requests: point.cancelled_requests,
            incomplete_requests: point.incomplete_requests,
            caller_error_requests: point.caller_error_requests,
            error_rate: point.error_rate,
        })
        .collect();
    let performance_points = insights
        .performance
        .points
        .iter()
        .map(|point| OverviewPerformancePointView {
            bucket: point.bucket_start,
            label: china_label(point.bucket_start, "%m-%d %H:%M"),
            latency_p50_ms: point.latency_percentiles.p50_ms.map(|value| value.as_f64()),
            latency_p95_ms: point.latency_percentiles.p95_ms.map(|value| value.as_f64()),
            latency_p99_ms: point.latency_percentiles.p99_ms.map(|value| value.as_f64()),
            first_token_p50_ms: point
                .first_token_latency_percentiles
                .p50_ms
                .map(|value| value.as_f64()),
            first_token_p95_ms: point
                .first_token_latency_percentiles
                .p95_ms
                .map(|value| value.as_f64()),
            first_token_p99_ms: point
                .first_token_latency_percentiles
                .p99_ms
                .map(|value| value.as_f64()),
        })
        .collect();
    let cost_points = insights
        .cost
        .points
        .iter()
        .map(|point| OverviewCostPointView {
            bucket: point.bucket_start,
            label: china_label(point.bucket_start, "%m-%d %H:%M"),
            input_tokens: point.input_tokens,
            output_tokens: point.output_tokens,
            cached_tokens: point.cached_tokens,
            total_tokens: point.total_tokens,
            estimated_cost: point.estimated_cost.as_ref().map(ToString::to_string),
            standard_cost: point.standard_cost.as_ref().map(ToString::to_string),
            cached_token_rate: point.cached_token_rate,
            cache_hit_request_rate: point.cache_hit_request_rate,
        })
        .collect();
    UsageInsightsOverviewView {
        granularity: granularity_name(insights.granularity).to_owned(),
        health: OverviewHealthView {
            total_requests: insights.health.total_requests,
            success_requests: insights.health.success_requests,
            failed_requests: insights.health.failed_requests,
            cancelled_requests: insights.health.cancelled_requests,
            incomplete_requests: insights.health.incomplete_requests,
            caller_error_requests: insights.health.caller_error_requests,
            success_rate: insights.health.success_rate,
            request_change_rate: None,
            success_rate_change: None,
            points: health_points,
        },
        performance: OverviewPerformanceView {
            latency_p50_ms: insights
                .performance
                .latency_percentiles
                .p50_ms
                .map(|value| value.as_f64()),
            latency_p95_ms: insights
                .performance
                .latency_percentiles
                .p95_ms
                .map(|value| value.as_f64()),
            latency_p99_ms: insights
                .performance
                .latency_percentiles
                .p99_ms
                .map(|value| value.as_f64()),
            first_token_p50_ms: insights
                .performance
                .first_token_latency_percentiles
                .p50_ms
                .map(|value| value.as_f64()),
            first_token_p95_ms: insights
                .performance
                .first_token_latency_percentiles
                .p95_ms
                .map(|value| value.as_f64()),
            first_token_p99_ms: insights
                .performance
                .first_token_latency_percentiles
                .p99_ms
                .map(|value| value.as_f64()),
            latency_coverage: insights.performance.latency_coverage,
            first_token_coverage: insights.performance.first_token_coverage,
            points: performance_points,
        },
        cost: OverviewCostView {
            estimated_cost: insights
                .cost
                .estimated_cost
                .as_ref()
                .map(ToString::to_string),
            standard_cost: insights
                .cost
                .standard_cost
                .as_ref()
                .map(ToString::to_string),
            cost_per_request: insights
                .cost
                .cost_per_request
                .as_ref()
                .map(ToString::to_string),
            tokens_per_request: insights.cost.tokens_per_request,
            cached_token_rate: insights.cost.cached_token_rate,
            cache_hit_request_rate: insights.cost.cache_hit_request_rate,
            input_tokens: insights.cost.input_tokens,
            output_tokens: insights.cost.output_tokens,
            cached_tokens: insights.cost.cached_tokens,
            total_tokens: insights.cost.total_tokens,
            points: cost_points,
            costs: cost_views(&insights.cost.costs),
            coverage: cost_coverage_view(&insights.cost.coverage),
        },
        attempts: attempt_metrics_view(&insights.attempts),
        providers: insights
            .providers
            .into_iter()
            .map(|provider| ProviderOverviewView {
                provider: provider.provider_kind,
                request_count: provider.request_count,
                attempt_count: provider.attempt_count,
                failure_count: provider.failure_count,
                total_tokens: provider.total_tokens,
            })
            .collect(),
    }
}

fn granularity_name(granularity: domain::Granularity) -> &'static str {
    match granularity {
        domain::Granularity::FifteenMinutes => "15m",
        domain::Granularity::Hour => "1h",
        domain::Granularity::Day => "1d",
    }
}

fn diagnostics_view(
    result: domain::DiagnosticsResult,
    dimension: DiagnosticDimension,
) -> DiagnosticsView {
    DiagnosticsView {
        dimension: dimension.display_name().to_owned(),
        items: result
            .items
            .into_iter()
            .map(|item| DiagnosticItemView {
                name: if item.name == "__none__" {
                    "未知".to_owned()
                } else {
                    item.name
                },
                request_count: item.request_count,
                success_count: item.success_count,
                error_count: item.error_count,
                error_rate: item.error_rate,
                request_share: item.request_share,
                average_latency_ms: item.average_latency_ms,
                estimated_cost: item.estimated_cost.as_ref().map(ToString::to_string),
                attempt_count: item.attempt_count,
                total_tokens: item.total_tokens,
            })
            .collect(),
    }
}

fn map_wire_error(error: WireValidationError) -> AdminError {
    let message = match error.field() {
        "timeRange" => "Invalid time range",
        "statusCode" => "Status code must be between 100 and 599",
        "attemptIndex" => "Attempt index is out of range",
        "kind" => "Invalid dashboard trend kind",
        "dimension" => "Invalid diagnostics dimension",
        "outcome" => "Invalid Observability query",
        "id" => "Usage record ID is required",
        "page" | "pageSize" => "Invalid Observability query",
        "cursor" => "Invalid observability cursor",
        _ => "Invalid observability query",
    };
    AdminError::bad_request(message)
}

fn map_service_error(error: gateway_admin::model::AdminError) -> AdminError {
    map_admin_service_error(error, "Observability repository unavailable")
}
