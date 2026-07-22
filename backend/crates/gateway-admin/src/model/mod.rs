//! 管理控制面使用的 Command、Result 与稳定值对象。

use std::num::{NonZeroU16, NonZeroU64};

pub mod accounts;
pub mod auth;
pub mod client_keys;
pub mod observability;
pub mod provider_credentials;
pub mod settings;
pub mod system;

/// 管理用例对外返回的稳定错误分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminErrorKind {
    Invalid,
    Unauthorized,
    NotFound,
    Conflict,
    RateLimited,
    BadGateway,
    Unavailable,
    Internal,
}

/// 不携带基础设施细节的管理用例错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct AdminError {
    kind: AdminErrorKind,
    message: String,
}

impl AdminError {
    #[must_use]
    pub fn new(kind: AdminErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> AdminErrorKind {
        self.kind
    }

    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
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
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(AdminErrorKind::Unavailable, message)
    }

    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(AdminErrorKind::Internal, message)
    }
}

/// PostgreSQL 中所有正整数 revision 的管理层表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(NonZeroU64);

impl Revision {
    /// 创建正整数 revision。
    ///
    /// # Errors
    ///
    /// `value` 为零时返回 [`AdminModelError::ZeroRevision`]。
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

/// 管理列表统一使用的受限页大小。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageSize(NonZeroU16);

impl PageSize {
    pub const MAX: u16 = 200;

    /// 创建 1 至 200 的页大小。
    ///
    /// # Errors
    ///
    /// `value` 不在有效范围时返回 [`AdminModelError::InvalidPageSize`]。
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

/// 可审计管理写操作的发起者。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationActor {
    AdminSession { admin_user_id: String },
    AdminApiKey,
    System,
}

/// 管理写操作必须携带的审计上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationContext {
    pub actor: MutationActor,
    pub request_id: String,
}

/// 领域值对象构造失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdminModelError {
    #[error("revision must be greater than zero")]
    ZeroRevision,
    #[error("page size {0} is outside 1..=200")]
    InvalidPageSize(u16),
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
