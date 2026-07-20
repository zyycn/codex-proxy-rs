//! Codex 官方 OAuth access token 的签名与身份校验。

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation, decode, decode_header, get_current_timestamp,
};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Client, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::types::{CodexAccountProfile, CodexOAuthSecret};
use crate::credential::token_client::OFFICIAL_CODEX_OAUTH_CLIENT_ID;
use crate::transport::profile::CodexWireProfileState;
use crate::transport::{
    CodexBackendClient, CodexClientError, CodexRequestContext, build_reqwest_client,
};

pub const OFFICIAL_OPENAI_ISSUER: &str = "https://auth.openai.com";
pub const OFFICIAL_OPENAI_API_AUDIENCE: &str = "https://api.openai.com/v1";
pub const OFFICIAL_OPENAI_JWKS_URI: &str = "https://auth.openai.com/.well-known/jwks.json";

const JWT_TYPE: &str = "JWT";
const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1024;
const MAX_REFRESH_TOKEN_BYTES: usize = 64 * 1024;
const MAX_JWKS_BODY_BYTES: usize = 256 * 1024;
const MAX_JWKS_KEYS: usize = 16;
const MAX_KEY_ID_BYTES: usize = 128;
const MAX_IDENTITY_BYTES: usize = 2_048;
const MAX_EMAIL_BYTES: usize = 512;
const MAX_PLAN_BYTES: usize = 128;
const MAX_TOKEN_LIFETIME_SECONDS: u64 = 366 * 24 * 60 * 60;
const JWKS_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const OFFICIAL_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";

/// OAuth identity verification 的稳定失败分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CodexIdentityVerificationError {
    #[error("Codex OAuth token identity was rejected")]
    Rejected,
    #[error("Codex OAuth identity verification is unavailable")]
    Unavailable,
}

/// 获取官方 OpenAI JWKS 的窄端口。
#[async_trait]
pub trait CodexJwksSource: Send + Sync {
    async fn fetch(&self) -> Result<Vec<u8>, CodexIdentityVerificationError>;
}

/// 已验签但尚未由账号接口补全的 OAuth 身份。
#[derive(Clone)]
pub struct CodexSignedIdentity {
    oauth_subject: String,
    poid: Option<String>,
    claimed_account_id: Option<String>,
    claimed_user_id: Option<String>,
    email: Option<String>,
    plan_type: Option<String>,
    access_token_expires_at: DateTime<Utc>,
}

impl fmt::Debug for CodexSignedIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexSignedIdentity")
            .field("oauth_subject", &"<redacted>")
            .field("poid", &self.poid.as_ref().map(|_| "<redacted>"))
            .field(
                "claimed_account_id",
                &self.claimed_account_id.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "claimed_user_id",
                &self.claimed_user_id.as_ref().map(|_| "<redacted>"),
            )
            .field("email", &self.email.as_ref().map(|_| "<redacted>"))
            .field("plan_type", &self.plan_type)
            .field("access_token_expires_at", &self.access_token_expires_at)
            .finish()
    }
}

impl CodexSignedIdentity {
    #[must_use]
    pub fn oauth_subject(&self) -> &str {
        &self.oauth_subject
    }

    #[must_use]
    pub fn poid(&self) -> Option<&str> {
        self.poid.as_deref()
    }

    #[must_use]
    pub fn claimed_account_id(&self) -> Option<&str> {
        self.claimed_account_id.as_deref()
    }

    #[must_use]
    pub fn claimed_user_id(&self) -> Option<&str> {
        self.claimed_user_id.as_deref()
    }

    #[must_use]
    pub const fn access_token_expires_at(&self) -> DateTime<Utc> {
        self.access_token_expires_at
    }
}

/// 导入文档或现有 credential 对认证结果施加的一致性约束。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodexIdentityExpectation {
    oauth_subject: Option<String>,
    poid: PoidExpectation,
    chatgpt_account_id: Option<String>,
    chatgpt_user_id: Option<String>,
    installation_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum PoidExpectation {
    #[default]
    Unspecified,
    Exact(Option<String>),
}

impl CodexIdentityExpectation {
    pub fn imported(
        chatgpt_account_id: Option<String>,
        chatgpt_user_id: Option<String>,
    ) -> Result<Self, CodexIdentityVerificationError> {
        if chatgpt_account_id
            .as_deref()
            .is_some_and(|value| !valid_identity(value))
            || chatgpt_user_id
                .as_deref()
                .is_some_and(|value| !valid_identity(value))
        {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        Ok(Self {
            chatgpt_account_id,
            chatgpt_user_id,
            ..Self::default()
        })
    }

    pub fn current(
        oauth_subject: String,
        poid: Option<String>,
        chatgpt_account_id: String,
        chatgpt_user_id: String,
        installation_id: String,
    ) -> Result<Self, CodexIdentityVerificationError> {
        if !valid_identity(&oauth_subject)
            || poid.as_deref().is_some_and(|value| !valid_identity(value))
            || !valid_identity(&chatgpt_account_id)
            || !valid_identity(&chatgpt_user_id)
            || !valid_installation_id(&installation_id)
        {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        Ok(Self {
            oauth_subject: Some(oauth_subject),
            poid: PoidExpectation::Exact(poid),
            chatgpt_account_id: Some(chatgpt_account_id),
            chatgpt_user_id: Some(chatgpt_user_id),
            installation_id: Some(installation_id),
        })
    }

    #[must_use]
    pub fn chatgpt_account_id(&self) -> Option<&str> {
        self.chatgpt_account_id.as_deref()
    }

    #[must_use]
    pub fn chatgpt_user_id(&self) -> Option<&str> {
        self.chatgpt_user_id.as_deref()
    }

    #[must_use]
    pub fn installation_id(&self) -> Option<&str> {
        self.installation_id.as_deref()
    }
}

/// `/wham/usage` 认证后返回的账号身份事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexAuthenticatedAccount {
    pub chatgpt_account_id: String,
    pub chatgpt_user_id: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

/// 完整认证成功，或仅签名已确认但账号接口暂时不可用。
#[derive(Clone, Debug)]
pub enum CodexIdentityVerification {
    Complete(CodexAccountProfile),
    SignedOnly(CodexSignedIdentity),
}

impl CodexIdentityVerification {
    pub fn into_complete(self) -> Result<CodexAccountProfile, CodexIdentityVerificationError> {
        match self {
            Self::Complete(profile) => Ok(profile),
            Self::SignedOnly(_) => Err(CodexIdentityVerificationError::Unavailable),
        }
    }
}

/// JWT/OIDC 签名验证边界；结果不能直接持久化为 Ready 账号。
#[async_trait]
pub trait CodexSignedIdentityVerifier: Send + Sync {
    async fn verify_access(
        &self,
        secret: &CodexOAuthSecret,
    ) -> Result<CodexSignedIdentity, CodexIdentityVerificationError>;

    async fn verify_authorization(
        &self,
        secret: &CodexOAuthSecret,
        id_token: &SecretString,
        expected_nonce: &SecretString,
    ) -> Result<CodexSignedIdentity, CodexIdentityVerificationError>;
}

/// 用 access token 调用认证账号接口的窄端口。
#[async_trait]
pub trait CodexAuthenticatedAccountSource: Send + Sync {
    async fn fetch(
        &self,
        secret: &CodexOAuthSecret,
        expectation: &CodexIdentityExpectation,
    ) -> Result<CodexAuthenticatedAccount, CodexIdentityVerificationError>;
}

/// OAuth、导入和刷新共享的完整身份认证边界。
#[async_trait]
pub trait CodexAccountIdentityVerifier: Send + Sync {
    async fn verify(
        &self,
        secret: &CodexOAuthSecret,
        expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError>;

    async fn verify_authorization(
        &self,
        secret: &CodexOAuthSecret,
        id_token: &SecretString,
        expected_nonce: &SecretString,
        expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError>;
}

/// 固定访问官方 OpenAI JWKS endpoint 的生产 source。
#[derive(Clone)]
pub struct ReqwestOpenAiJwksSource {
    client: Client,
}

impl ReqwestOpenAiJwksSource {
    /// 构建禁止 redirect、带固定超时的官方 JWKS source。
    ///
    /// # Errors
    ///
    /// TLS/HTTP client 无法初始化时返回 unavailable。
    pub fn new() -> Result<Self, CodexIdentityVerificationError> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| CodexIdentityVerificationError::Unavailable)?;
        Ok(Self { client })
    }
}

impl fmt::Debug for ReqwestOpenAiJwksSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReqwestOpenAiJwksSource")
            .field("endpoint", &OFFICIAL_OPENAI_JWKS_URI)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl CodexJwksSource for ReqwestOpenAiJwksSource {
    async fn fetch(&self) -> Result<Vec<u8>, CodexIdentityVerificationError> {
        let mut response = self
            .client
            .get(OFFICIAL_OPENAI_JWKS_URI)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| CodexIdentityVerificationError::Unavailable)?;
        match response.status().as_u16() {
            200..=299 => {}
            429 | 500..=599 => return Err(CodexIdentityVerificationError::Unavailable),
            _ => return Err(CodexIdentityVerificationError::Rejected),
        }
        if !is_jwks_content_type(&response) {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| CodexIdentityVerificationError::Unavailable)?
        {
            let next_length = body
                .len()
                .checked_add(chunk.len())
                .filter(|length| *length <= MAX_JWKS_BODY_BYTES)
                .ok_or(CodexIdentityVerificationError::Rejected)?;
            body.reserve(next_length.saturating_sub(body.len()));
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

/// 使用严格 RS256/JWKS 校验的 Codex token identity verifier。
pub struct CodexJwtIdentityVerifier {
    jwks_source: Box<dyn CodexJwksSource>,
    cache: Mutex<JwksCache>,
    cache_ttl: Duration,
}

impl CodexJwtIdentityVerifier {
    #[must_use]
    pub fn new(jwks_source: Box<dyn CodexJwksSource>) -> Self {
        Self {
            jwks_source,
            cache: Mutex::new(JwksCache::default()),
            cache_ttl: JWKS_CACHE_TTL,
        }
    }

    async fn decoding_key(
        &self,
        key_id: &str,
    ) -> Result<DecodingKey, CodexIdentityVerificationError> {
        if !valid_key_id(key_id) {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        let mut cache = self.cache.lock().await;
        if cache
            .fetched_at
            .is_some_and(|fetched_at| fetched_at.elapsed() < self.cache_ttl)
            && let Some(key) = cache.keys.get(key_id)
        {
            return key.decoding_key();
        }
        let keys = parse_jwks(&self.jwks_source.fetch().await?)?;
        cache.replace(keys);
        cache
            .keys
            .get(key_id)
            .ok_or(CodexIdentityVerificationError::Rejected)?
            .decoding_key()
    }

    async fn decode_access_token(
        &self,
        secret: &CodexOAuthSecret,
    ) -> Result<AccessTokenClaims, CodexIdentityVerificationError> {
        validate_oauth_shape(secret)?;
        let access_token = secret.access_token.expose_secret();
        let header =
            decode_header(access_token).map_err(|_| CodexIdentityVerificationError::Rejected)?;
        if !valid_signed_token_header(&header) {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        let key_id = header
            .kid
            .as_deref()
            .ok_or(CodexIdentityVerificationError::Rejected)?;
        let decoding_key = self.decoding_key(key_id).await?;
        let mut validation = official_validation(OFFICIAL_OPENAI_API_AUDIENCE);
        validation.set_required_spec_claims(&["sub", "iss", "aud", "exp", "iat"]);
        Ok(
            decode::<AccessTokenClaims>(access_token, &decoding_key, &validation)
                .map_err(|_| CodexIdentityVerificationError::Rejected)?
                .claims,
        )
    }

    async fn decode_id_token(
        &self,
        id_token: &SecretString,
    ) -> Result<IdTokenClaims, CodexIdentityVerificationError> {
        let id_token = id_token.expose_secret();
        if id_token.len() > MAX_ACCESS_TOKEN_BYTES
            || !valid_visible_ascii(id_token)
            || id_token.matches('.').count() != 2
        {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        let header =
            decode_header(id_token).map_err(|_| CodexIdentityVerificationError::Rejected)?;
        if !valid_signed_token_header(&header) {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        let key_id = header
            .kid
            .as_deref()
            .ok_or(CodexIdentityVerificationError::Rejected)?;
        let decoding_key = self.decoding_key(key_id).await?;
        let mut validation = official_validation(OFFICIAL_CODEX_OAUTH_CLIENT_ID);
        validation.set_required_spec_claims(&["sub", "iss", "aud", "exp", "iat", "nonce"]);
        Ok(
            decode::<IdTokenClaims>(id_token, &decoding_key, &validation)
                .map_err(|_| CodexIdentityVerificationError::Rejected)?
                .claims,
        )
    }
}

impl fmt::Debug for CodexJwtIdentityVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexJwtIdentityVerifier")
            .field("jwks_source", &"CodexJwksSource")
            .field("cache_ttl", &self.cache_ttl)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl CodexSignedIdentityVerifier for CodexJwtIdentityVerifier {
    async fn verify_access(
        &self,
        secret: &CodexOAuthSecret,
    ) -> Result<CodexSignedIdentity, CodexIdentityVerificationError> {
        self.decode_access_token(secret)
            .await?
            .into_signed_identity()
    }

    async fn verify_authorization(
        &self,
        secret: &CodexOAuthSecret,
        id_token: &SecretString,
        expected_nonce: &SecretString,
    ) -> Result<CodexSignedIdentity, CodexIdentityVerificationError> {
        let access = self.decode_access_token(secret).await?;
        let id = self.decode_id_token(id_token).await?;
        if access.sub != id.sub || !id.valid_for_nonce(expected_nonce.expose_secret()) {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        access.into_signed_identity()
    }
}

/// 官方 `/wham/usage` 账号身份 source。
pub struct ReqwestCodexAuthenticatedAccountSource {
    client: CodexBackendClient,
}

impl ReqwestCodexAuthenticatedAccountSource {
    pub fn new(profile: CodexWireProfileState) -> Result<Self, CodexIdentityVerificationError> {
        let http =
            build_reqwest_client().map_err(|_| CodexIdentityVerificationError::Unavailable)?;
        Ok(Self {
            client: CodexBackendClient::new(http, OFFICIAL_CODEX_BASE_URL, profile),
        })
    }
}

impl fmt::Debug for ReqwestCodexAuthenticatedAccountSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReqwestCodexAuthenticatedAccountSource")
            .field("endpoint", &OFFICIAL_CODEX_BASE_URL)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl CodexAuthenticatedAccountSource for ReqwestCodexAuthenticatedAccountSource {
    async fn fetch(
        &self,
        secret: &CodexOAuthSecret,
        expectation: &CodexIdentityExpectation,
    ) -> Result<CodexAuthenticatedAccount, CodexIdentityVerificationError> {
        let request_id = format!("identity_{}", Uuid::now_v7().simple());
        let value = self
            .client
            .fetch_usage(CodexRequestContext {
                access_token: secret.access_token.expose_secret(),
                account_id: expectation.chatgpt_account_id(),
                request_id: &request_id,
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: None,
                installation_id: expectation.installation_id(),
                session_id: None,
                thread_id: None,
                client_request_id: None,
                turn_id: None,
            })
            .await
            .map_err(map_account_source_error)?;
        authenticated_account(&value)
    }
}

/// 签名验证与认证账号接口的唯一组合服务。
pub struct CodexAccountIdentityService {
    signed: Arc<dyn CodexSignedIdentityVerifier>,
    accounts: Arc<dyn CodexAuthenticatedAccountSource>,
}

impl CodexAccountIdentityService {
    #[must_use]
    pub fn new(
        signed: Arc<dyn CodexSignedIdentityVerifier>,
        accounts: Arc<dyn CodexAuthenticatedAccountSource>,
    ) -> Self {
        Self { signed, accounts }
    }

    async fn complete(
        &self,
        secret: &CodexOAuthSecret,
        signed: CodexSignedIdentity,
        expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        match self.accounts.fetch(secret, expectation).await {
            Ok(account) => complete_profile(signed, account, expectation)
                .map(CodexIdentityVerification::Complete),
            Err(CodexIdentityVerificationError::Unavailable) => {
                Ok(CodexIdentityVerification::SignedOnly(signed))
            }
            Err(error) => Err(error),
        }
    }
}

impl fmt::Debug for CodexAccountIdentityService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CodexAccountIdentityService([VERIFIERS])")
    }
}

#[async_trait]
impl CodexAccountIdentityVerifier for CodexAccountIdentityService {
    async fn verify(
        &self,
        secret: &CodexOAuthSecret,
        expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        let signed = self.signed.verify_access(secret).await?;
        self.complete(secret, signed, expectation).await
    }

    async fn verify_authorization(
        &self,
        secret: &CodexOAuthSecret,
        id_token: &SecretString,
        expected_nonce: &SecretString,
        expectation: &CodexIdentityExpectation,
    ) -> Result<CodexIdentityVerification, CodexIdentityVerificationError> {
        let signed = self
            .signed
            .verify_authorization(secret, id_token, expected_nonce)
            .await?;
        self.complete(secret, signed, expectation).await
    }
}

#[derive(Default)]
struct JwksCache {
    fetched_at: Option<Instant>,
    keys: HashMap<String, CachedRsaKey>,
}

impl JwksCache {
    fn replace(&mut self, keys: HashMap<String, CachedRsaKey>) {
        self.fetched_at = Some(Instant::now());
        self.keys = keys;
    }
}

struct CachedRsaKey {
    modulus: String,
    exponent: String,
}

impl CachedRsaKey {
    fn decoding_key(&self) -> Result<DecodingKey, CodexIdentityVerificationError> {
        DecodingKey::from_rsa_components(&self.modulus, &self.exponent)
            .map_err(|_| CodexIdentityVerificationError::Rejected)
    }
}

#[derive(Deserialize)]
struct JwksWire {
    keys: Vec<RsaJwkWire>,
}

#[derive(Deserialize)]
struct RsaJwkWire {
    kty: String,
    #[serde(rename = "use")]
    usage: String,
    alg: String,
    kid: String,
    n: String,
    e: String,
}

#[derive(Deserialize)]
struct AccessTokenClaims {
    iss: String,
    aud: AudienceClaim,
    exp: u64,
    iat: u64,
    #[serde(default)]
    nbf: Option<u64>,
    sub: String,
    #[serde(rename = "https://api.openai.com/auth")]
    auth: OpenAiAuthClaims,
    #[serde(rename = "https://api.openai.com/profile")]
    profile: Option<OpenAiProfileClaims>,
}

#[derive(Deserialize)]
struct IdTokenClaims {
    iss: String,
    aud: AudienceClaim,
    exp: u64,
    iat: u64,
    #[serde(default)]
    nbf: Option<u64>,
    sub: String,
    nonce: String,
}

impl IdTokenClaims {
    fn valid_for_nonce(&self, expected_nonce: &str) -> bool {
        let now = get_current_timestamp();
        self.iss == OFFICIAL_OPENAI_ISSUER
            && self.aud.is_exact(OFFICIAL_CODEX_OAUTH_CLIENT_ID)
            && self.exp > now
            && self.iat <= now
            && self.iat < self.exp
            && self.exp.saturating_sub(self.iat) <= MAX_TOKEN_LIFETIME_SECONDS
            && self
                .nbf
                .is_none_or(|not_before| not_before <= now && not_before < self.exp)
            && valid_identity(&self.sub)
            && valid_visible_ascii(&self.nonce)
            && constant_time_equal(self.nonce.as_bytes(), expected_nonce.as_bytes())
    }
}

impl AccessTokenClaims {
    fn into_signed_identity(self) -> Result<CodexSignedIdentity, CodexIdentityVerificationError> {
        let now = get_current_timestamp();
        if self.iss != OFFICIAL_OPENAI_ISSUER
            || !self.aud.is_exact(OFFICIAL_OPENAI_API_AUDIENCE)
            || self.exp <= now
            || self.iat > now
            || self.iat >= self.exp
            || self.exp.saturating_sub(self.iat) > MAX_TOKEN_LIFETIME_SECONDS
            || self
                .nbf
                .is_some_and(|not_before| not_before > now || not_before >= self.exp)
            || !valid_identity(&self.sub)
            || self
                .auth
                .chatgpt_account_id
                .as_deref()
                .is_some_and(|value| !valid_identity(value))
            || self
                .auth
                .poid
                .as_deref()
                .is_some_and(|value| !valid_identity(value))
        {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        if self
            .auth
            .chatgpt_user_id
            .as_deref()
            .is_some_and(|value| !valid_identity(value))
            || self.auth.user_id.as_ref().is_some_and(|user_id| {
                !valid_identity(user_id)
                    || self
                        .auth
                        .chatgpt_user_id
                        .as_ref()
                        .is_some_and(|chatgpt_user_id| chatgpt_user_id != user_id)
            })
            || self
                .auth
                .chatgpt_plan_type
                .as_deref()
                .is_some_and(|value| !valid_text(value, MAX_PLAN_BYTES))
            || self
                .profile
                .as_ref()
                .and_then(|profile| profile.email.as_deref())
                .is_some_and(|value| !valid_text(value, MAX_EMAIL_BYTES))
        {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        let expires_at = DateTime::<Utc>::from_timestamp(
            i64::try_from(self.exp).map_err(|_| CodexIdentityVerificationError::Rejected)?,
            0,
        )
        .ok_or(CodexIdentityVerificationError::Rejected)?;
        Ok(CodexSignedIdentity {
            email: self.profile.and_then(|profile| profile.email),
            oauth_subject: self.sub,
            poid: self.auth.poid,
            claimed_account_id: self.auth.chatgpt_account_id,
            claimed_user_id: self.auth.chatgpt_user_id.or(self.auth.user_id),
            plan_type: self.auth.chatgpt_plan_type,
            access_token_expires_at: expires_at,
        })
    }
}

#[derive(Deserialize)]
struct OpenAiAuthClaims {
    #[serde(default)]
    chatgpt_account_id: Option<String>,
    #[serde(default)]
    poid: Option<String>,
    #[serde(default)]
    chatgpt_user_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    chatgpt_plan_type: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiProfileClaims {
    #[serde(default)]
    email: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AudienceClaim {
    One(String),
    Many(Vec<String>),
}

impl AudienceClaim {
    fn is_exact(&self, expected: &str) -> bool {
        match self {
            Self::One(value) => value == expected,
            Self::Many(values) => values.as_slice() == [expected],
        }
    }
}

fn complete_profile(
    signed: CodexSignedIdentity,
    account: CodexAuthenticatedAccount,
    expectation: &CodexIdentityExpectation,
) -> Result<CodexAccountProfile, CodexIdentityVerificationError> {
    if expectation
        .oauth_subject
        .as_deref()
        .is_some_and(|expected| expected != signed.oauth_subject)
        || matches!(&expectation.poid, PoidExpectation::Exact(expected) if expected != &signed.poid)
        || expectation
            .chatgpt_account_id
            .as_deref()
            .is_some_and(|expected| expected != account.chatgpt_account_id)
        || expectation
            .chatgpt_user_id
            .as_deref()
            .is_some_and(|expected| expected != account.chatgpt_user_id)
        || signed
            .claimed_account_id
            .as_deref()
            .is_some_and(|claimed| claimed != account.chatgpt_account_id)
        || signed
            .claimed_user_id
            .as_deref()
            .is_some_and(|claimed| claimed != account.chatgpt_user_id)
    {
        return Err(CodexIdentityVerificationError::Rejected);
    }
    Ok(CodexAccountProfile {
        email: account.email.or(signed.email),
        oauth_subject: signed.oauth_subject,
        poid: signed.poid,
        chatgpt_account_id: account.chatgpt_account_id,
        chatgpt_user_id: account.chatgpt_user_id,
        plan_type: account.plan_type.or(signed.plan_type),
        access_token_expires_at: Some(signed.access_token_expires_at),
    })
}

fn authenticated_account(
    value: &Value,
) -> Result<CodexAuthenticatedAccount, CodexIdentityVerificationError> {
    let chatgpt_account_id = required_account_field(value, "account_id")?;
    let chatgpt_user_id = required_account_field(value, "user_id")?;
    let email = optional_account_field(value, "email", MAX_EMAIL_BYTES)?;
    let plan_type = optional_account_field(value, "plan_type", MAX_PLAN_BYTES)?;
    Ok(CodexAuthenticatedAccount {
        chatgpt_account_id,
        chatgpt_user_id,
        email,
        plan_type,
    })
}

fn required_account_field(
    value: &Value,
    key: &str,
) -> Result<String, CodexIdentityVerificationError> {
    let value = value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| valid_identity(value))
        .ok_or(CodexIdentityVerificationError::Rejected)?;
    Ok(value.to_owned())
}

fn optional_account_field(
    value: &Value,
    key: &str,
    max_bytes: usize,
) -> Result<Option<String>, CodexIdentityVerificationError> {
    let Some(value) = value.get(key).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .map(str::trim)
        .filter(|value| valid_text(value, max_bytes))
        .ok_or(CodexIdentityVerificationError::Rejected)?;
    Ok(Some(value.to_owned()))
}

fn map_account_source_error(error: CodexClientError) -> CodexIdentityVerificationError {
    match error {
        CodexClientError::Upstream {
            status: StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN,
            ..
        } => CodexIdentityVerificationError::Rejected,
        _ => CodexIdentityVerificationError::Unavailable,
    }
}

fn parse_jwks(
    body: &[u8],
) -> Result<HashMap<String, CachedRsaKey>, CodexIdentityVerificationError> {
    let wire: JwksWire =
        serde_json::from_slice(body).map_err(|_| CodexIdentityVerificationError::Rejected)?;
    if wire.keys.is_empty() || wire.keys.len() > MAX_JWKS_KEYS {
        return Err(CodexIdentityVerificationError::Rejected);
    }
    let mut keys = HashMap::with_capacity(wire.keys.len());
    for key in wire.keys {
        if key.kty != "RSA"
            || key.usage != "sig"
            || key.alg != "RS256"
            || !valid_key_id(&key.kid)
            || !valid_rsa_component(&key.n, 256, 512)
            || !valid_rsa_component(&key.e, 3, 8)
            || keys
                .insert(
                    key.kid,
                    CachedRsaKey {
                        modulus: key.n,
                        exponent: key.e,
                    },
                )
                .is_some()
        {
            return Err(CodexIdentityVerificationError::Rejected);
        }
    }
    Ok(keys)
}

fn validate_oauth_shape(secret: &CodexOAuthSecret) -> Result<(), CodexIdentityVerificationError> {
    let access_token = secrecy::ExposeSecret::expose_secret(&secret.access_token);
    if access_token.len() > MAX_ACCESS_TOKEN_BYTES
        || !valid_visible_ascii(access_token)
        || access_token.matches('.').count() != 2
    {
        return Err(CodexIdentityVerificationError::Rejected);
    }
    if secret.refresh_token.as_ref().is_some_and(|token| {
        let token = secrecy::ExposeSecret::expose_secret(token);
        token.len() > MAX_REFRESH_TOKEN_BYTES
            || !valid_visible_ascii(token)
            || token == access_token
    }) {
        return Err(CodexIdentityVerificationError::Rejected);
    }
    Ok(())
}

fn official_validation(audience: &str) -> Validation {
    let mut validation = Validation::new(Algorithm::RS256);
    validation.leeway = 0;
    validation.reject_tokens_expiring_in_less_than = 0;
    validation.validate_exp = true;
    validation.validate_nbf = true;
    validation.validate_aud = true;
    validation.set_issuer(&[OFFICIAL_OPENAI_ISSUER]);
    validation.set_audience(&[audience]);
    validation
}

fn valid_signed_token_header(header: &jsonwebtoken::Header) -> bool {
    header.alg == Algorithm::RS256
        && header.typ.as_deref().is_none_or(|value| value == JWT_TYPE)
        && header.kid.as_deref().is_some_and(valid_key_id)
        && header.cty.is_none()
        && header.jku.is_none()
        && header.jwk.is_none()
        && header.x5u.is_none()
        && header.x5c.is_none()
        && header.x5t.is_none()
        && header.x5t_s256.is_none()
        && header.crit.is_none()
        && header.enc.is_none()
        && header.zip.is_none()
        && header.url.is_none()
        && header.nonce.is_none()
        && header.extras.is_empty()
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    use subtle::ConstantTimeEq as _;

    left.len() == right.len() && bool::from(left.ct_eq(right))
}

fn valid_key_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_KEY_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_rsa_component(value: &str, min_bytes: usize, max_bytes: usize) -> bool {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .is_ok_and(|decoded| (min_bytes..=max_bytes).contains(&decoded.len()))
}

fn valid_identity(value: &str) -> bool {
    valid_text(value, MAX_IDENTITY_BYTES)
}

fn valid_installation_id(value: &str) -> bool {
    Uuid::parse_str(value)
        .ok()
        .is_some_and(|uuid| uuid.get_version_num() == 4)
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_visible_ascii(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn is_jwks_content_type(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("application/json")
                || value.eq_ignore_ascii_case("application/jwk-set+json")
        })
}
