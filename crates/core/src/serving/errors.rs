//! 服务编排错误。

use thiserror::Error;

/// 服务编排错误。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ServingError {
    /// 没有可用账号。
    #[error("no available account")]
    NoAvailableAccount,
    /// 上游请求失败。
    #[error("upstream request failed: {message}")]
    Upstream {
        /// 错误说明。
        message: String,
    },
}
