//! OAuth 领域逻辑与上游端口。

use std::{collections::BTreeMap, sync::Arc};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::Semaphore;

use crate::accounts::model::{Account, AccountStatus};
pub use crate::accounts::token_refresh::TokenRefresher;

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

/// 刷新任务的调度策略。
#[derive(Debug, Clone, Copy)]
pub struct RefreshPolicy {
    /// 提前多久开始刷新访问令牌。
    pub refresh_margin_seconds: u64,
    /// 允许并发执行的刷新任务数。
    pub refresh_concurrency: u32,
}

/// 触发刷新动作的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshTrigger {
    /// 在访问令牌即将过期前触发刷新。
    BeforeExpiry,
    /// 在上游返回未授权后立即刷新。
    Unauthorized,
}

/// 上游刷新失败后的领域结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RefreshFailure {
    /// 刷新令牌无效或已过期。
    #[error("refresh token is invalid or expired")]
    InvalidGrant,
    /// 账号配额耗尽。
    #[error("account quota is exhausted")]
    QuotaExhausted,
    /// 账号被上游封禁。
    #[error("account is banned")]
    Banned,
    /// 账号被显式禁用。
    #[error("account is disabled")]
    Disabled,
    /// 刷新请求在到达服务端前失败，可安全复用当前 refresh token 重试。
    #[error("refresh transport failed before server processing")]
    RetryableTransport,
    /// 刷新请求在传输层失败，refresh token 可能已经被服务端消费。
    #[error("refresh transport failed after possible server processing")]
    Transport,
}

/// 调度器自身的执行错误。
#[derive(Debug, Error)]
pub enum RefreshError {
    /// 并发限制信号量已关闭。
    #[error("refresh task semaphore closed")]
    ConcurrencyClosed,
    /// 刷新请求在到达服务端前失败，可安全复用当前 refresh token 重试。
    #[error("refresh transport failed before server processing")]
    RetryableTransport,
    /// 刷新请求在传输层失败，refresh token 可能已经被服务端消费。
    #[error("refresh transport failed after possible server processing")]
    Transport,
}

/// 负责执行单账号刷新策略的调度器。
#[derive(Clone)]
pub struct RefreshScheduler<C> {
    policy: RefreshPolicy,
    client: Arc<C>,
    semaphore: Arc<Semaphore>,
}

impl<C> RefreshScheduler<C>
where
    C: TokenRefresher,
{
    /// 使用策略和上游刷新端口构造调度器。
    pub fn new(policy: RefreshPolicy, client: C) -> Self {
        let concurrency = policy.refresh_concurrency.max(1) as usize;
        Self {
            policy,
            client: Arc::new(client),
            semaphore: Arc::new(Semaphore::new(concurrency)),
        }
    }

    /// 在给定时间点按触发原因刷新账号。
    ///
    /// 当账号不需要刷新时返回原账号快照；当刷新失败但属于可映射的领域错误时，
    /// 返回更新过状态的账号。
    ///
    /// # Errors
    ///
    /// 当并发控制已关闭或刷新传输失败时返回 [`RefreshError`]。
    pub async fn refresh_account_at(
        &self,
        account: &Account,
        trigger: RefreshTrigger,
        now: DateTime<Utc>,
    ) -> Result<Account, RefreshError> {
        if !self.should_refresh(account, trigger, now) {
            return Ok(account.clone());
        }

        let Some(refresh_token) = account.refresh_token.as_deref() else {
            let mut expired = account.clone();
            expired.status = AccountStatus::Expired;
            return Ok(expired);
        };

        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| RefreshError::ConcurrencyClosed)?;

        match self.client.refresh(refresh_token).await {
            Ok(token_pair) => Ok(apply_token_pair(account, token_pair)),
            Err(RefreshFailure::RetryableTransport) => Err(RefreshError::RetryableTransport),
            Err(RefreshFailure::Transport) => Err(RefreshError::Transport),
            Err(error) => Ok(apply_refresh_failure(account, error)),
        }
    }

    /// 判断账号在给定触发原因下是否需要刷新。
    pub fn should_refresh_account_at(
        &self,
        account: &Account,
        trigger: RefreshTrigger,
        now: DateTime<Utc>,
    ) -> bool {
        self.should_refresh(account, trigger, now)
    }

    fn should_refresh(
        &self,
        account: &Account,
        trigger: RefreshTrigger,
        now: DateTime<Utc>,
    ) -> bool {
        if account.status != AccountStatus::Active {
            return false;
        }

        match trigger {
            RefreshTrigger::Unauthorized => true,
            RefreshTrigger::BeforeExpiry => account
                .access_token_expires_at
                .is_some_and(|expires_at| expires_at <= now + self.refresh_margin()),
        }
    }

    fn refresh_margin(&self) -> Duration {
        let seconds = self.policy.refresh_margin_seconds.min(86_400 * 7) as i64;
        Duration::seconds(seconds)
    }
}

/// 将新的 token 对应用到账号快照上。
pub fn apply_token_pair(account: &Account, token_pair: TokenPair) -> Account {
    let mut refreshed = account.clone();
    refreshed.access_token = token_pair.access_token;

    // 刷新响应不返回 refresh_token 时，继续保留旧值，避免永久失去刷新能力。
    if let Some(refresh_token) = token_pair.refresh_token {
        refreshed.refresh_token = Some(refresh_token);
    }

    refreshed.status = AccountStatus::Active;
    refreshed
}

/// 将刷新失败映射为账号状态变更。
pub fn apply_refresh_failure(account: &Account, failure: RefreshFailure) -> Account {
    let mut updated = account.clone();
    updated.status = match failure {
        RefreshFailure::InvalidGrant => AccountStatus::Disabled,
        RefreshFailure::QuotaExhausted => AccountStatus::QuotaExhausted,
        RefreshFailure::Banned => AccountStatus::Banned,
        RefreshFailure::Disabled => AccountStatus::Disabled,
        RefreshFailure::RetryableTransport => AccountStatus::Active,
        RefreshFailure::Transport => AccountStatus::Active,
    };
    updated
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
