//! token 刷新用例拥有的上游端口与稳定值。

use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RefreshFailure {
    #[error("refresh token is invalid or expired")]
    InvalidGrant,
    #[error("account is banned")]
    Banned,
    #[error("refresh transport failed before server processing")]
    RetryableTransport,
    #[error("refresh transport failed after possible server processing")]
    Transport,
}

#[async_trait]
pub trait TokenRefresher: Send + Sync + 'static {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure>;
}
