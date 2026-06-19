//! 认证相关的上游端口。

use async_trait::async_trait;

use crate::auth::oauth::{DeviceCode, OAuthError, RefreshFailure, TokenPair};

/// 刷新令牌的上游端口。
#[async_trait]
pub trait TokenRefresher: Send + Sync + 'static {
    /// 使用给定刷新令牌换取新的 token 对。
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure>;
}

/// OAuth 交互上游端口。
#[async_trait]
pub trait OAuthClient: Send + Sync + 'static {
    /// 使用 PKCE 授权码交换 token 对。
    async fn exchange_code(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenPair, OAuthError>;

    /// 请求设备码登录信息。
    async fn request_device_code(&self) -> Result<DeviceCode, OAuthError>;

    /// 轮询设备码换取 token 对。
    async fn poll_device_token(&self, device_code: &str) -> Result<TokenPair, OAuthError>;
}
