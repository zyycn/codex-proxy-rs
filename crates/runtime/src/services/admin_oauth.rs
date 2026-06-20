use super::*;

/// 管理端 OAuth 服务。
#[derive(Clone)]
pub struct AdminOAuthService {
    config: OAuthConfig,
    client: Arc<dyn OAuthClient>,
    sessions: Arc<tokio::sync::Mutex<PkceSessionStore>>,
}

impl AdminOAuthService {
    /// 构造管理端 OAuth 服务。
    pub fn new(config: OAuthConfig, client: Arc<dyn OAuthClient>) -> Self {
        Self {
            config,
            client,
            sessions: Arc::new(tokio::sync::Mutex::new(PkceSessionStore::default())),
        }
    }

    /// 开始 PKCE 登录。
    pub async fn start_pkce_login(&self, return_host: &str) -> PkceLogin {
        self.sessions
            .lock()
            .await
            .start_login(return_host, &self.config)
    }

    /// 请求设备码登录信息。
    pub async fn request_device_code(&self) -> Result<DeviceCode, AdminOAuthError> {
        self.client
            .request_device_code()
            .await
            .map_err(AdminOAuthError::OAuth)
    }

    /// 轮询设备码 token。
    pub async fn poll_device_token(
        &self,
        device_code: &str,
    ) -> Result<AdminDevicePoll, AdminOAuthError> {
        match self.client.poll_device_token(device_code).await {
            Ok(tokens) => Ok(AdminDevicePoll::Authorized(tokens)),
            Err(error) => {
                if let Some(code) = error.pending_code() {
                    Ok(AdminDevicePoll::Pending { code })
                } else {
                    Err(AdminOAuthError::OAuth(error))
                }
            }
        }
    }

    /// 使用回调 code/state 完成 PKCE token 交换。
    pub async fn exchange_callback(
        &self,
        code: &str,
        state: &str,
    ) -> Result<AdminOAuthCallback, AdminOAuthError> {
        let session = self
            .sessions
            .lock()
            .await
            .try_acquire(state)
            .ok_or(AdminOAuthError::InvalidState)?;

        match self
            .client
            .exchange_code(code, &session.code_verifier, &session.redirect_uri)
            .await
        {
            Ok(tokens) => {
                self.sessions.lock().await.complete(state);
                Ok(AdminOAuthCallback {
                    tokens,
                    return_host: session.return_host,
                })
            }
            Err(error) => {
                self.sessions.lock().await.release(state);
                Err(AdminOAuthError::OAuth(error))
            }
        }
    }
}

/// 设备码轮询结果。
#[derive(Debug, Clone)]
pub enum AdminDevicePoll {
    /// 授权还未完成。
    Pending {
        /// OAuth 标准 pending 错误码。
        code: &'static str,
    },
    /// 已换取 token。
    Authorized(TokenPair),
}

/// PKCE 回调换取的 token 和返回 host。
#[derive(Debug, Clone)]
pub struct AdminOAuthCallback {
    /// OAuth token 对。
    pub tokens: TokenPair,
    /// 登录前的管理端 host。
    pub return_host: String,
}

/// 管理端 OAuth 错误。
#[derive(Debug, Error)]
pub enum AdminOAuthError {
    /// callback URL 或 query 缺少必需字段。
    #[error("invalid OAuth callback")]
    InvalidCallback,
    /// OAuth state 不存在、过期或正在处理。
    #[error("invalid OAuth state")]
    InvalidState,
    /// OAuth 上游错误。
    #[error("{0}")]
    OAuth(OAuthError),
}
