//! 用量基础操作的错误分类，不保存失败或重试状态

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageErrorKind {
    InvalidInput,
    NotFound,
    Conflict,
    CapacityExceeded,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageDataError {
    pub kind: UsageErrorKind,
    pub message: String,
}
impl UsageDataError {
    #[must_use]
    pub fn new(kind: UsageErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(UsageErrorKind::InvalidInput, message)
    }
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(UsageErrorKind::Unavailable, message)
    }
}
impl std::fmt::Display for UsageDataError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for UsageDataError {}
pub type UsageDataResult<T> = Result<T, UsageDataError>;
