//! OAuth 领域逻辑与上游端口。

use std::{collections::BTreeMap, sync::Arc};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// 一次刷新返回的 token 对。
#[derive(Debug, Clone)]
pub struct TokenPair {
    /// 新的访问令牌。
    pub access_token: String,
    /// 可选的新刷新令牌。
    pub refresh_token: Option<String>,
}

/// OAuth 客户端静态配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthConfig {
    /// OAuth 客户端 ID。
    pub client_id: String,
    /// 浏览器授权入口。
    pub auth_endpoint: String,
    /// 设备码申请入口。
    pub device_code_endpoint: String,
    /// Token 交换入口。
    pub token_endpoint: String,
}

/// 设备码登录响应。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DeviceCode {
    /// 设备码。
    pub device_code: String,
    /// 用户输入码。
    pub user_code: String,
    /// 激活入口。
    pub verification_uri: String,
    /// 带用户码的激活入口。
    pub verification_uri_complete: String,
    /// 过期秒数。
    pub expires_in: u64,
    /// 建议轮询间隔秒数。
    pub interval: u64,
}

/// OAuth 调用错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OAuthError {
    /// 设备授权尚未完成。
    #[error("authorization pending")]
    AuthorizationPending,
    /// 上游要求放慢轮询速度。
    #[error("slow down")]
    SlowDown,
    /// 上游显式拒绝请求。
    #[error("OAuth request rejected: {0}")]
    Rejected(String),
    /// HTTP 或解析层失败。
    #[error("OAuth transport failed")]
    Transport,
}

impl OAuthError {
    /// 返回可用于设备码轮询响应的标准错误码。
    pub fn pending_code(&self) -> Option<&'static str> {
        match self {
            Self::AuthorizationPending => Some("authorization_pending"),
            Self::SlowDown => Some("slow_down"),
            Self::Rejected(_) | Self::Transport => None,
        }
    }
}

/// 一次 PKCE 登录开始后返回给前端的数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkceLogin {
    /// 引导用户跳转的授权地址。
    pub auth_url: String,
    /// 回调校验状态值。
    pub state: String,
}

/// 存储在服务端的 PKCE 会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkceSession {
    /// PKCE code verifier。
    pub code_verifier: String,
    /// OAuth 回调地址。
    pub redirect_uri: String,
    /// 登录完成后跳回的 host。
    pub return_host: String,
}

#[derive(Debug, Clone)]
struct PendingPkceSession {
    session: PkceSession,
    created_at: DateTime<Utc>,
    exchanging: bool,
}

/// 管理 PKCE 授权回调状态的内存会话仓。
#[derive(Debug)]
pub struct PkceSessionStore {
    pending: BTreeMap<String, PendingPkceSession>,
    completed: BTreeMap<String, DateTime<Utc>>,
    ttl: Duration,
}

impl Default for PkceSessionStore {
    fn default() -> Self {
        Self {
            pending: BTreeMap::new(),
            completed: BTreeMap::new(),
            ttl: Duration::minutes(5),
        }
    }
}

impl PkceSessionStore {
    /// 开始一次新的 PKCE 登录流程。
    pub fn start_login(&mut self, return_host: &str, config: &OAuthConfig) -> PkceLogin {
        self.cleanup(Utc::now());
        let (code_verifier, code_challenge) = generate_pkce_pair();
        let state = random_state();
        let redirect_uri = "http://localhost:1455/auth/callback".to_string();
        let auth_url = build_auth_url(config, &redirect_uri, &state, &code_challenge);

        self.pending.insert(
            state.clone(),
            PendingPkceSession {
                session: PkceSession {
                    code_verifier,
                    redirect_uri,
                    return_host: return_host.to_string(),
                },
                created_at: Utc::now(),
                exchanging: false,
            },
        );

        PkceLogin { auth_url, state }
    }

    /// 尝试独占获取一条待完成的 PKCE 会话。
    pub fn try_acquire(&mut self, state: &str) -> Option<PkceSession> {
        self.cleanup(Utc::now());
        let pending = self.pending.get_mut(state)?;
        if pending.exchanging {
            return None;
        }
        pending.exchanging = true;
        Some(pending.session.clone())
    }

    /// 释放一次失败的会话占用。
    pub fn release(&mut self, state: &str) {
        if let Some(pending) = self.pending.get_mut(state) {
            pending.exchanging = false;
        }
    }

    /// 将给定 state 标记为已完成。
    pub fn complete(&mut self, state: &str) {
        self.pending.remove(state);
        self.completed.insert(state.to_string(), Utc::now());
    }

    /// 判断 state 是否已经完成或正在处理中。
    pub fn is_completed_or_exchanging(&mut self, state: &str) -> bool {
        self.cleanup(Utc::now());
        self.completed.contains_key(state)
            || self
                .pending
                .get(state)
                .is_some_and(|pending| pending.exchanging)
    }

    fn cleanup(&mut self, now: DateTime<Utc>) {
        self.pending
            .retain(|_, pending| now - pending.created_at <= self.ttl);
        self.completed
            .retain(|_, completed_at| now - *completed_at <= self.ttl);
    }
}

/// 管理端 OAuth 服务。
#[derive(Clone)]
pub struct AdminOAuthService {
    config: OAuthConfig,
    client: Arc<dyn OAuthClient>,
    sessions: Arc<tokio::sync::Mutex<PkceSessionStore>>,
}

impl AdminOAuthService {
    /// 构造服务。
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

    /// 申请设备码。
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

    /// 交换 OAuth callback code。
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
    /// 仍在等待用户授权。
    Pending {
        /// OAuth 标准 pending code。
        code: &'static str,
    },
    /// 设备码已授权。
    Authorized(TokenPair),
}

/// OAuth callback 交换结果。
#[derive(Debug, Clone)]
pub struct AdminOAuthCallback {
    /// 交换得到的 token。
    pub tokens: TokenPair,
    /// 登录完成后返回的 host。
    pub return_host: String,
}

/// 管理端 OAuth 错误。
#[derive(Debug, Error)]
pub enum AdminOAuthError {
    /// OAuth callback 无法解析。
    #[error("invalid OAuth callback")]
    InvalidCallback,
    /// OAuth state 无效或已被占用。
    #[error("invalid OAuth state")]
    InvalidState,
    /// 上游 OAuth 调用失败。
    #[error("{0}")]
    OAuth(OAuthError),
}

fn generate_pkce_pair() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let code_verifier = URL_SAFE_NO_PAD.encode(bytes);
    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = URL_SAFE_NO_PAD.encode(digest);
    (code_verifier, code_challenge)
}

fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn build_auth_url(
    config: &OAuthConfig,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", config.client_id.as_str()),
        ("redirect_uri", redirect_uri),
        ("scope", "openid profile email offline_access"),
        ("code_challenge", code_challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
        ("originator", "codex_cli_rs"),
    ];
    let query = params
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{}?{query}", config.auth_endpoint)
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push_str("%20"),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// OAuth 交互上游端口。
#[async_trait::async_trait]
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
