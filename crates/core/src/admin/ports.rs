//! 管理员领域端口。

use async_trait::async_trait;
use thiserror::Error;

/// 客户端 API Key 存储错误。
#[derive(Debug, Error)]
pub enum ClientKeyStoreError {
    /// 底层存储失败。
    #[error("client key store operation failed: {message}")]
    OperationFailed {
        /// 错误说明。
        message: String,
    },
}

/// 客户端 API Key 存储结果类型。
pub type ClientKeyStoreResult<T> = Result<T, ClientKeyStoreError>;

/// 提供客户端 API Key 验证能力的端口。
#[async_trait]
pub trait ClientKeyStore: Send + Sync + 'static {
    /// 验证明文客户端 API Key，并在成功时记录使用时间。
    async fn verify_and_touch(&self, plaintext: &str) -> ClientKeyStoreResult<bool>;
}
