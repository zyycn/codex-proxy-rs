//! 插件侧传输和会话错误及稳定业务错误投影

use crate::{CallBudgetError, ErrorCode, FrameError, PluginFault};

/// 插件侧传输和会话错误
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("plugin session configuration is invalid")]
    Configuration,
    #[error(transparent)]
    CallBudget(#[from] CallBudgetError),
    #[error("plugin handshake is invalid")]
    Handshake,
    #[error("plugin protocol is invalid")]
    Protocol,
    #[error("plugin transport is closed")]
    Closed,
    #[error("plugin operation exceeded its deadline")]
    Timeout,
    #[error("plugin call was cancelled")]
    Cancelled,
    #[error("plugin session capacity is exhausted")]
    Capacity,
    #[error("plugin handler stopped unexpectedly")]
    HandlerStopped,
    #[error("host callback returned an error: {0:?}")]
    Remote(PluginFault),
    #[error(transparent)]
    Frame(#[from] FrameError),
}

impl SessionError {
    /// 将本地会话失败收敛为可返回宿主的稳定插件错误
    #[must_use]
    pub fn into_plugin_fault(self) -> PluginFault {
        match self {
            Self::Remote(fault) => fault,
            Self::Timeout => PluginFault::new(ErrorCode::Timeout, "host callback timed out"),
            Self::Cancelled => PluginFault::new(ErrorCode::Cancelled, "parent call was cancelled"),
            Self::Capacity => PluginFault::new(ErrorCode::Capacity, "plugin capacity is exhausted"),
            Self::Configuration
            | Self::CallBudget(_)
            | Self::Protocol
            | Self::Handshake
            | Self::Frame(_) => {
                PluginFault::new(ErrorCode::InvalidInput, "plugin session input is invalid")
            }
            Self::Closed | Self::HandlerStopped => {
                PluginFault::new(ErrorCode::Fault, "plugin session is unavailable")
            }
        }
    }
}
