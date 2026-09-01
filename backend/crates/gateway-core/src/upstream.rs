//! 上游调用跨 Engine、Event、Error 与 Provider 共享的中立边界事实。

use crate::error::{IdentifierError, validate_text};

/// 上游是否可能已经收到业务 payload。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpstreamSendState {
    NotSent,
    Sent,
    Ambiguous,
}

impl UpstreamSendState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotSent => "not_sent",
            Self::Sent => "sent",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// 上游 transport 注册名称。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UpstreamTransport(String);

impl UpstreamTransport {
    /// 校验 transport 名称。
    ///
    /// # Errors
    ///
    /// 名称无效时返回错误。
    pub fn new(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_text(&value, 64, true, None)?;
        Ok(Self(value))
    }

    /// 返回 transport 名称。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
