//! 查询 wire 类型、校验与领域命令映射。

use super::*;

/// 观测列表默认页大小。
pub const DEFAULT_PAGE_SIZE: u16 = 50;
/// 观测列表允许的最大页大小。
pub const MAX_PAGE_SIZE: u16 = 100;

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
    pub current_page: Option<u32>,
    pub page_size: Option<u16>,
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
    /// 校验 Element Plus 风格的页码分页字段。
    pub fn validate_pagination(&self) -> Result<(u32, u16), WireValidationError> {
        let current_page = self.current_page.unwrap_or(1);
        if current_page == 0 {
            return Err(WireValidationError::new("currentPage"));
        }
        let page_size = self.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
        if page_size == 0 || page_size > MAX_PAGE_SIZE {
            return Err(WireValidationError::new("pageSize"));
        }
        Ok((current_page, page_size))
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
    pub current_page: Option<u32>,
    pub page_size: Option<u16>,
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
    /// 校验 Element Plus 风格的页码分页字段。
    pub fn validate_pagination(&self) -> Result<(u32, u16), WireValidationError> {
        let current_page = self.current_page.unwrap_or(1);
        if current_page == 0 {
            return Err(WireValidationError::new("currentPage"));
        }
        let page_size = self.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
        if page_size == 0 || page_size > MAX_PAGE_SIZE {
            return Err(WireValidationError::new("pageSize"));
        }
        Ok((current_page, page_size))
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

pub(crate) fn require_text(value: &str, field: &'static str) -> Result<(), WireValidationError> {
    if value.trim().is_empty() {
        Err(WireValidationError::new(field))
    } else {
        Ok(())
    }
}

pub(crate) fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub(crate) fn domain_trend_kind(kind: TrendKind) -> domain::TrendKind {
    match kind {
        TrendKind::Usage => domain::TrendKind::Usage,
        TrendKind::Latency => domain::TrendKind::Latency,
        TrendKind::Errors => domain::TrendKind::Errors,
    }
}

pub(crate) fn domain_diagnostic_dimension(
    dimension: DiagnosticDimension,
) -> domain::DiagnosticDimension {
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

pub(crate) fn request_outcome(
    value: Option<String>,
) -> Result<Option<domain::RequestOutcome>, WireValidationError> {
    non_empty(value)
        .map(|value| {
            domain::RequestOutcome::new(value).map_err(|_| WireValidationError::new("outcome"))
        })
        .transpose()
}

pub(crate) fn usage_range(
    start: Option<&str>,
    end: Option<&str>,
) -> Result<domain::TimeRange, WireValidationError> {
    let end = parse_datetime(end)?.unwrap_or_else(Utc::now);
    let start = parse_datetime(start)?.unwrap_or(end - Duration::days(7));
    domain::TimeRange::new(start, end).map_err(|_| WireValidationError::new("timeRange"))
}

pub(crate) fn dashboard_today_range(
    start: Option<&str>,
    end: Option<&str>,
) -> Result<domain::TimeRange, WireValidationError> {
    let end = parse_datetime(end)?.unwrap_or_else(Utc::now);
    let start = parse_datetime(start)?.unwrap_or_else(|| domain::china_day_start(end));
    domain::TimeRange::new(start, end).map_err(|_| WireValidationError::new("timeRange"))
}

pub(crate) fn usage_filter(query: &UsageQuery) -> Result<domain::UsageFilter, WireValidationError> {
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

pub(crate) fn usage_command(query: &UsageQuery) -> Result<domain::UsageQuery, WireValidationError> {
    let (current_page, page_size) = query.validate_pagination()?;
    let page_size_value =
        DomainPageSize::new(page_size).map_err(|_| WireValidationError::new("pageSize"))?;
    Ok(domain::UsageQuery {
        range: usage_range(query.start_time.as_deref(), query.end_time.as_deref())?,
        filter: usage_filter(query)?,
        current_page,
        page_size: page_size_value,
    })
}

pub(crate) fn ops_command(query: &OpsQuery) -> Result<domain::OpsErrorQuery, WireValidationError> {
    let (current_page, page_size) = query.validate_pagination()?;
    let page_size_value =
        DomainPageSize::new(page_size).map_err(|_| WireValidationError::new("pageSize"))?;
    let status_code = parse_status(
        query
            .upstream_status_code
            .or(query.client_status_code)
            .or(query.status_code),
    )?;
    Ok(domain::OpsErrorQuery {
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
        current_page,
        page_size: page_size_value,
    })
}

pub(crate) fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
