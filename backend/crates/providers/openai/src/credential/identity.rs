//! Codex 官方 OAuth access token 的签名与身份校验。

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation, decode, decode_header, get_current_timestamp,
};
use reqwest::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use tokio::sync::Mutex;

use super::types::{CodexAccountProfile, CodexOAuthSecret};
use crate::credential::token_client::OFFICIAL_CODEX_OAUTH_CLIENT_ID;

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
const REFRESH_MARGIN: TimeDelta = TimeDelta::minutes(5);

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

/// 验证 Codex OAuth token 并返回由签名 claims 派生的账号事实。
#[async_trait]
pub trait CodexTokenIdentityVerifier: Send + Sync {
    async fn verify(
        &self,
        secret: &CodexOAuthSecret,
    ) -> Result<CodexAccountProfile, CodexIdentityVerificationError>;
}

/// Authorization Code flow 的 OIDC token-set 校验端口。
///
/// 除 access token 的官方身份边界外，还必须验证 ID token 的签名、client audience、
/// subject 与 server-side nonce；调用方不能用浏览器输入替代这些事实。
#[async_trait]
pub trait CodexAuthorizationTokenVerifier: Send + Sync {
    async fn verify_authorization(
        &self,
        secret: &CodexOAuthSecret,
        id_token: &SecretString,
        expected_nonce: &SecretString,
    ) -> Result<CodexAccountProfile, CodexIdentityVerificationError>;
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
impl CodexTokenIdentityVerifier for CodexJwtIdentityVerifier {
    async fn verify(
        &self,
        secret: &CodexOAuthSecret,
    ) -> Result<CodexAccountProfile, CodexIdentityVerificationError> {
        self.decode_access_token(secret)
            .await?
            .into_profile(secret.refresh_token.is_some())
    }
}

#[async_trait]
impl CodexAuthorizationTokenVerifier for CodexJwtIdentityVerifier {
    async fn verify_authorization(
        &self,
        secret: &CodexOAuthSecret,
        id_token: &SecretString,
        expected_nonce: &SecretString,
    ) -> Result<CodexAccountProfile, CodexIdentityVerificationError> {
        let access = self.decode_access_token(secret).await?;
        let id = self.decode_id_token(id_token).await?;
        if access.sub != id.sub || !id.valid_for_nonce(expected_nonce.expose_secret()) {
            return Err(CodexIdentityVerificationError::Rejected);
        }
        access.into_profile(secret.refresh_token.is_some())
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
    fn into_profile(
        self,
        refreshable: bool,
    ) -> Result<CodexAccountProfile, CodexIdentityVerificationError> {
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
            || !valid_identity(&self.auth.chatgpt_account_id)
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
        let next_refresh_at = refreshable.then(|| {
            let planned = expires_at - REFRESH_MARGIN;
            planned.max(Utc::now())
        });
        Ok(CodexAccountProfile {
            email: self.profile.and_then(|profile| profile.email),
            chatgpt_account_id: self.auth.chatgpt_account_id,
            chatgpt_user_id: self.auth.chatgpt_user_id.or(self.auth.user_id),
            plan_type: self.auth.chatgpt_plan_type,
            access_token_expires_at: Some(expires_at),
            next_refresh_at,
        })
    }
}

#[derive(Deserialize)]
struct OpenAiAuthClaims {
    chatgpt_account_id: String,
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
