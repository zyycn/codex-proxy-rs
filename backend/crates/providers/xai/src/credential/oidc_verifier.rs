//! xAI 官方 OIDC token 的生产校验边界。

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation, decode, decode_header, get_current_timestamp,
};
use reqwest::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::Deserialize;
use tokio::sync::Mutex;
use url::Url;

use crate::transport::network::{BoundedBody, GrokEndpointPolicy, build_client, collect_bounded};
use crate::{
    OFFICIAL_CLIENT_ID, OFFICIAL_ISSUER, TokenCandidate, TokenVerificationContext, TokenVerifier,
    VerificationEvidence, VerificationFailure, VerificationFlow, VerificationFuture,
};

const ES256_JWA: &str = "ES256";
const JWT_TYPE: &str = "JWT";
const JWKS_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_JWKS_BODY_BYTES: usize = 256 * 1024;
const MAX_USERINFO_BODY_BYTES: usize = 64 * 1024;
const MAX_JWKS_KEYS: usize = 16;
const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1024;
const MAX_SUBJECT_BYTES: usize = 512;
const MAX_KEY_ID_BYTES: usize = 128;
const MAX_AUDIENCES: usize = 16;

/// 使用官方 JWKS 与 user-info endpoint 的严格生产 verifier。
pub struct ReqwestOidcTokenVerifier {
    client: Client,
    endpoint_policy: GrokEndpointPolicy,
    jwks_cache: Mutex<JwksCache>,
    cache_ttl: Duration,
}

impl ReqwestOidcTokenVerifier {
    /// 构建只允许访问 `auth.x.ai` 官方验证端点的 verifier。
    ///
    /// # Errors
    ///
    /// TLS client 初始化失败时返回 transport build error。
    pub fn new() -> Result<Self, crate::transport::GrokReqwestTransportBuildError> {
        let endpoint_policy = GrokEndpointPolicy::official_oauth();
        Self::with_endpoint_policy(endpoint_policy)
    }

    /// 构建使用显式 endpoint policy 的 verifier。
    ///
    /// # Errors
    ///
    /// HTTP client 初始化失败时返回 transport build error。
    pub fn with_endpoint_policy(
        endpoint_policy: GrokEndpointPolicy,
    ) -> Result<Self, crate::transport::GrokReqwestTransportBuildError> {
        Self::with_endpoint_policy_and_cache_ttl(endpoint_policy, JWKS_CACHE_TTL)
    }

    /// 构建使用显式 endpoint policy 与 JWKS cache TTL 的 verifier。
    ///
    /// # Errors
    ///
    /// HTTP client 初始化失败时返回 transport build error。
    pub fn with_endpoint_policy_and_cache_ttl(
        endpoint_policy: GrokEndpointPolicy,
        cache_ttl: Duration,
    ) -> Result<Self, crate::transport::GrokReqwestTransportBuildError> {
        let client = build_client(&endpoint_policy, Some(REQUEST_TIMEOUT))?;
        Ok(Self {
            client,
            endpoint_policy,
            jwks_cache: Mutex::new(JwksCache::default()),
            cache_ttl,
        })
    }

    async fn verify_inner(
        &self,
        context: TokenVerificationContext<'_>,
        candidate: TokenCandidate<'_>,
    ) -> Result<VerificationEvidence, VerificationFailure> {
        self.validate_context(&context)?;
        match context.flow() {
            VerificationFlow::AuthorizationCode => self.verify_id_token(&context, candidate).await,
            VerificationFlow::CredentialImport | VerificationFlow::CredentialImportRefreshed => {
                self.verify_userinfo(&context, candidate).await
            }
        }
    }

    fn validate_context(
        &self,
        context: &TokenVerificationContext<'_>,
    ) -> Result<(), VerificationFailure> {
        if !is_official_issuer(context.issuer())
            || context.client_id() != OFFICIAL_CLIENT_ID
            || !context
                .signing_algorithms()
                .iter()
                .any(|algorithm| algorithm == ES256_JWA)
            || !self.endpoint_policy.validate_jwks(context.jwks_uri())
            || !self
                .endpoint_policy
                .validate_userinfo(context.userinfo_endpoint())
        {
            return Err(VerificationFailure::Rejected);
        }

        match context.flow() {
            VerificationFlow::AuthorizationCode => {
                let nonce = context
                    .expected_nonce()
                    .ok_or(VerificationFailure::Rejected)?;
                if nonce.expose().is_empty() || nonce.expose().len() > MAX_TOKEN_BYTES {
                    return Err(VerificationFailure::Rejected);
                }
            }
            VerificationFlow::CredentialImportRefreshed if context.expected_nonce().is_some() => {
                return Err(VerificationFailure::Rejected);
            }
            VerificationFlow::CredentialImportRefreshed => {}
            VerificationFlow::CredentialImport if context.expected_nonce().is_some() => {
                return Err(VerificationFailure::Rejected);
            }
            VerificationFlow::CredentialImport => {}
        }
        Ok(())
    }

    async fn verify_id_token(
        &self,
        context: &TokenVerificationContext<'_>,
        candidate: TokenCandidate<'_>,
    ) -> Result<VerificationEvidence, VerificationFailure> {
        let subject = self
            .verify_signed_id_token(context, &candidate, true)
            .await?;
        Ok(VerificationEvidence::id_token(subject))
    }

    async fn verify_signed_id_token(
        &self,
        context: &TokenVerificationContext<'_>,
        candidate: &TokenCandidate<'_>,
        require_nonce: bool,
    ) -> Result<String, VerificationFailure> {
        let token = candidate
            .id_token()
            .ok_or(VerificationFailure::MissingIdToken)?
            .expose();
        if token.len() > MAX_TOKEN_BYTES || token.matches('.').count() != 2 {
            return Err(VerificationFailure::Rejected);
        }

        let header = decode_header(token).map_err(|_| VerificationFailure::Rejected)?;
        if !valid_id_token_header(&header) {
            return Err(VerificationFailure::Rejected);
        }
        let kid = header.kid.as_deref().ok_or(VerificationFailure::Rejected)?;
        let key = self.decoding_key(context.jwks_uri(), kid).await?;

        let mut validation = Validation::new(Algorithm::ES256);
        validation.leeway = 0;
        validation.reject_tokens_expiring_in_less_than = 0;
        validation.validate_exp = true;
        validation.validate_nbf = true;
        validation.validate_aud = true;
        validation.set_issuer(&[OFFICIAL_ISSUER]);
        validation.set_audience(&[OFFICIAL_CLIENT_ID]);
        validation.set_required_spec_claims(&["sub", "iss", "aud", "exp"]);

        let claims = decode::<IdTokenClaims>(token, &key, &validation)
            .map_err(|_| VerificationFailure::Rejected)?
            .claims;
        let nonce_matches = match (
            require_nonce,
            context.expected_nonce(),
            claims.nonce.as_deref(),
        ) {
            (true, Some(expected), Some(actual)) => {
                constant_time_eq(actual.as_bytes(), expected.expose().as_bytes())
            }
            (false, None, _) => true,
            _ => false,
        };
        if claims.iss != OFFICIAL_ISSUER
            || !valid_audience(&claims.aud, claims.azp.as_deref())
            || !nonce_matches
            || claims.exp <= get_current_timestamp()
            || !valid_subject(&claims.sub)
        {
            return Err(VerificationFailure::Rejected);
        }

        Ok(claims.sub)
    }

    async fn verify_userinfo(
        &self,
        context: &TokenVerificationContext<'_>,
        candidate: TokenCandidate<'_>,
    ) -> Result<VerificationEvidence, VerificationFailure> {
        let subject = self.fetch_userinfo_subject(context, &candidate).await?;
        Ok(VerificationEvidence::user_info(subject))
    }

    async fn fetch_userinfo_subject(
        &self,
        context: &TokenVerificationContext<'_>,
        candidate: &TokenCandidate<'_>,
    ) -> Result<String, VerificationFailure> {
        let access_token = candidate.access_token().expose();
        if !valid_bearer_token(access_token) {
            return Err(VerificationFailure::Rejected);
        }
        let response = self
            .client
            .get(context.userinfo_endpoint().clone())
            .header(ACCEPT, "application/json")
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| VerificationFailure::Unavailable)?;
        classify_verification_status(response.status().as_u16())?;
        if !is_json_content_type(&response, false) {
            return Err(VerificationFailure::Rejected);
        }
        let body = match collect_bounded(response, MAX_USERINFO_BODY_BYTES)
            .await
            .map_err(|_| VerificationFailure::Unavailable)?
        {
            BoundedBody::Body(body) => body,
            BoundedBody::TooLarge => return Err(VerificationFailure::Rejected),
        };
        let user: UserInfoWire =
            serde_json::from_slice(&body).map_err(|_| VerificationFailure::Rejected)?;
        if !valid_subject(&user.sub) {
            return Err(VerificationFailure::Rejected);
        }
        Ok(user.sub)
    }

    async fn decoding_key(
        &self,
        jwks_uri: &Url,
        kid: &str,
    ) -> Result<DecodingKey, VerificationFailure> {
        if !valid_key_id(kid) {
            return Err(VerificationFailure::Rejected);
        }

        // 持锁完成获取，保证冷启动和轮换时每个进程只有一个在途 JWKS 请求。
        let mut cache = self.jwks_cache.lock().await;
        let cache_is_fresh = cache.uri.as_ref() == Some(jwks_uri)
            && cache
                .fetched_at
                .is_some_and(|fetched_at| fetched_at.elapsed() < self.cache_ttl);
        if cache_is_fresh {
            if let Some(key) = cache.keys.get(kid) {
                return key.decoding_key();
            }

            // fresh cache 未命中 kid 时只强制刷新一次，以支持官方密钥轮换。
            let keys = self.fetch_jwks(jwks_uri).await?;
            cache.replace(jwks_uri.clone(), keys);
            return cache
                .keys
                .get(kid)
                .ok_or(VerificationFailure::Rejected)?
                .decoding_key();
        }

        // 过期缓存不会在网络或协议失败时作为 stale fallback 使用。
        let keys = self.fetch_jwks(jwks_uri).await?;
        cache.replace(jwks_uri.clone(), keys);
        cache
            .keys
            .get(kid)
            .ok_or(VerificationFailure::Rejected)?
            .decoding_key()
    }

    async fn fetch_jwks(
        &self,
        jwks_uri: &Url,
    ) -> Result<HashMap<String, CachedJwk>, VerificationFailure> {
        if !self.endpoint_policy.validate_jwks(jwks_uri) {
            return Err(VerificationFailure::Rejected);
        }
        let response = self
            .client
            .get(jwks_uri.clone())
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| VerificationFailure::Unavailable)?;
        classify_verification_status(response.status().as_u16())?;
        if !is_json_content_type(&response, true) {
            return Err(VerificationFailure::Rejected);
        }
        let body = match collect_bounded(response, MAX_JWKS_BODY_BYTES)
            .await
            .map_err(|_| VerificationFailure::Unavailable)?
        {
            BoundedBody::Body(body) => body,
            BoundedBody::TooLarge => return Err(VerificationFailure::Rejected),
        };
        parse_jwks(&body)
    }
}

impl TokenVerifier for ReqwestOidcTokenVerifier {
    fn verify<'a>(
        &'a self,
        context: TokenVerificationContext<'a>,
        candidate: TokenCandidate<'a>,
    ) -> VerificationFuture<'a> {
        Box::pin(async move { self.verify_inner(context, candidate).await })
    }
}

impl fmt::Debug for ReqwestOidcTokenVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReqwestOidcTokenVerifier")
            .field("client", &"reqwest::Client")
            .field("endpoint_policy", &self.endpoint_policy)
            .field("cache_ttl", &self.cache_ttl)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct JwksCache {
    uri: Option<Url>,
    fetched_at: Option<Instant>,
    keys: HashMap<String, CachedJwk>,
}

impl JwksCache {
    fn replace(&mut self, uri: Url, keys: HashMap<String, CachedJwk>) {
        self.uri = Some(uri);
        self.fetched_at = Some(Instant::now());
        self.keys = keys;
    }
}

struct CachedJwk {
    x: String,
    y: String,
}

impl CachedJwk {
    fn decoding_key(&self) -> Result<DecodingKey, VerificationFailure> {
        DecodingKey::from_ec_components(&self.x, &self.y).map_err(|_| VerificationFailure::Rejected)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JwksWire {
    keys: Vec<JwkWire>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JwkWire {
    kty: String,
    #[serde(rename = "use")]
    usage: String,
    crv: String,
    kid: String,
    x: String,
    y: String,
    alg: String,
}

#[derive(Deserialize)]
struct IdTokenClaims {
    iss: String,
    aud: AudienceClaim,
    exp: u64,
    sub: String,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    azp: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AudienceClaim {
    One(String),
    Many(Vec<String>),
}

impl AudienceClaim {
    fn values(&self) -> &[String] {
        match self {
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values,
        }
    }
}

#[derive(Deserialize)]
struct UserInfoWire {
    sub: String,
}

fn parse_jwks(body: &[u8]) -> Result<HashMap<String, CachedJwk>, VerificationFailure> {
    let wire: JwksWire = serde_json::from_slice(body).map_err(|_| VerificationFailure::Rejected)?;
    if wire.keys.is_empty() || wire.keys.len() > MAX_JWKS_KEYS {
        return Err(VerificationFailure::Rejected);
    }

    let mut keys = HashMap::with_capacity(wire.keys.len());
    for key in wire.keys {
        if key.kty != "EC"
            || key.usage != "sig"
            || key.crv != "P-256"
            || key.alg != ES256_JWA
            || !valid_key_id(&key.kid)
            || !valid_coordinate(&key.x)
            || !valid_coordinate(&key.y)
            || keys
                .insert(key.kid, CachedJwk { x: key.x, y: key.y })
                .is_some()
        {
            return Err(VerificationFailure::Rejected);
        }
    }
    Ok(keys)
}

fn valid_id_token_header(header: &jsonwebtoken::Header) -> bool {
    header.alg == Algorithm::ES256
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

fn valid_audience(audience: &AudienceClaim, azp: Option<&str>) -> bool {
    let values = audience.values();
    if values.is_empty()
        || values.len() > MAX_AUDIENCES
        || values.iter().any(|value| {
            value.is_empty()
                || value.len() > MAX_SUBJECT_BYTES
                || value.bytes().any(|byte| byte.is_ascii_control())
        })
        || values.iter().collect::<HashSet<_>>().len() != values.len()
        || !values.iter().any(|value| value == OFFICIAL_CLIENT_ID)
    {
        return false;
    }

    match (values.len(), azp) {
        (1, None) => true,
        (_, Some(authorized_party)) => authorized_party == OFFICIAL_CLIENT_ID,
        _ => false,
    }
}

fn valid_coordinate(value: &str) -> bool {
    value.len() == 43
        && !value.contains('=')
        && URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|decoded| decoded.len() == 32)
}

fn valid_key_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_KEY_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_subject(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SUBJECT_BYTES
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_bearer_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ACCESS_TOKEN_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'=')
        })
}

fn is_official_issuer(issuer: &Url) -> bool {
    issuer.scheme() == "https"
        && issuer.host_str() == Some("auth.x.ai")
        && issuer.port_or_known_default() == Some(443)
        && issuer.path() == "/"
        && issuer.username().is_empty()
        && issuer.password().is_none()
        && issuer.query().is_none()
        && issuer.fragment().is_none()
}

fn classify_verification_status(status: u16) -> Result<(), VerificationFailure> {
    match status {
        200..=299 => Ok(()),
        429 | 500..=599 => Err(VerificationFailure::Unavailable),
        _ => Err(VerificationFailure::Rejected),
    }
}

fn is_json_content_type(response: &reqwest::Response, allow_jwk_set: bool) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("application/json")
                || (allow_jwk_set && value.eq_ignore_ascii_case("application/jwk-set+json"))
        })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}
