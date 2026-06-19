//! 核心领域通用错误。

use thiserror::Error;

/// 核心领域错误。
#[derive(Debug, Error)]
pub enum CoreError {
    /// 输入不符合领域约束。
    #[error("invalid domain input: {message}")]
    InvalidInput {
        /// 错误说明。
        message: String,
    },
    /// 依赖端口调用失败。
    #[error("port operation failed: {message}")]
    Port {
        /// 错误说明。
        message: String,
    },
}

/// 核心领域结果。
pub type CoreResult<T> = Result<T, CoreError>;
