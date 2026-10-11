//! 管理控制面使用的 Command、Result 与稳定值对象

use std::num::{NonZeroU16, NonZeroU64};

pub mod account_groups;
pub mod accounts;
pub mod analytics;
pub mod audit;
pub mod auth;
pub mod backup;
pub mod client_distribution;
pub mod client_keys;
pub mod import_tasks;
pub mod key_usage;
pub mod observability;
pub mod plugin_client_keys;
pub mod plugin_resources;
pub mod plugins;
pub mod pricing;
pub mod provider_credentials;
pub mod proxies;
pub mod quota_forecast;
pub mod quota_forecast_sampling;
pub mod retention;
pub mod settings;
pub mod system;

/// 管理用例对外返回的稳定错误分类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminErrorKind {
    Invalid,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    RateLimited,
    BadGateway,
    UpstreamResultUnknown,
    Unavailable,
    Internal,
}

impl AdminErrorKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Forbidden => "forbidden",
            Self::Unauthorized => "unauthorized",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::RateLimited => "rate_limited",
            Self::BadGateway => "bad_gateway",
            Self::UpstreamResultUnknown => "upstream_result_unknown",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }
}

/// 管理用例的安全消息与内部原因分离，原始来源不进入响应
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct AdminError {
    kind: AdminErrorKind,
    message: String,
    source: Option<gateway_core::error::ErrorSource>,
}

impl AdminError {
    #[must_use]
    pub fn new(kind: AdminErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    #[must_use]
    pub fn with_source(mut self, source: impl Into<gateway_core::error::ErrorSource>) -> Self {
        self.source = Some(source.into());
        self
    }

    #[must_use]
    pub fn error_details(&self) -> Option<gateway_core::error::ErrorDetails> {
        gateway_core::error::ErrorDetails::capture(self.source.as_ref(), None, false)
    }

    #[must_use]
    pub const fn kind(&self) -> AdminErrorKind {
        self.kind
    }

    /// 返回已脱敏、可公开给管理员的文案
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(AdminErrorKind::Invalid, message)
    }

    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(AdminErrorKind::Invalid, message)
    }

    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(AdminErrorKind::NotFound, message)
    }

    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(AdminErrorKind::Conflict, message)
    }

    #[must_use]
    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self::new(AdminErrorKind::BadGateway, message)
    }

    #[must_use]
    pub fn upstream_result_unknown(message: impl Into<String>) -> Self {
        Self::new(AdminErrorKind::UpstreamResultUnknown, message)
    }

    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(AdminErrorKind::Unavailable, message)
    }

    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(AdminErrorKind::Internal, message)
    }
}

/// PostgreSQL 中所有正整数 revision 的管理层表示
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct Revision(NonZeroU64);

impl Revision {
    /// 创建正整数 revision
    ///
    /// # Errors
    ///
    /// `value` 为零时返回 [`AdminModelError::ZeroRevision`]
    pub fn new(value: u64) -> Result<Self, AdminModelError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(AdminModelError::ZeroRevision)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// 管理列表统一使用的受限页大小
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageSize(NonZeroU16);

impl PageSize {
    pub const MAX: u16 = 200;

    /// 创建 1 至 200 的页大小
    ///
    /// # Errors
    ///
    /// `value` 不在有效范围时返回 [`AdminModelError::InvalidPageSize`]
    pub fn new(value: u16) -> Result<Self, AdminModelError> {
        if value > Self::MAX {
            return Err(AdminModelError::InvalidPageSize(value));
        }
        NonZeroU16::new(value)
            .map(Self)
            .ok_or(AdminModelError::InvalidPageSize(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// 可审计管理写操作的发起者
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MutationActor {
    AdminSession { admin_user_id: String },
    AdminApiKey,
    System,
}

/// 管理写操作必须携带的审计上下文
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MutationContext {
    pub actor: MutationActor,
    pub request_id: String,
}

/// 领域值对象构造失败
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdminModelError {
    #[error("revision must be greater than zero")]
    ZeroRevision,
    #[error("page size {0} is outside 1..=200")]
    InvalidPageSize(u16),
    #[error("observability page size {0} is outside 1..=100")]
    InvalidObservabilityPageSize(u16),
    #[error("account group name or description is invalid")]
    InvalidAccountGroupFields,
    #[error("client key group IDs are duplicated or exceed 1000 entries")]
    InvalidClientKeyGroups,
    #[error("client key page size must be inside 1..=65535")]
    InvalidClientKeyPageSize,
    #[error("request outcome must be 1..=256 bytes without control characters")]
    InvalidRequestOutcome,
    #[error("time range must be positive and no longer than 366 days")]
    InvalidTimeRange,
    #[error("decimal amount is not a non-negative numeric(20,10) value")]
    InvalidDecimalAmount,
    #[error("latency percentile must be finite and non-negative")]
    InvalidLatencyPercentile,
}
