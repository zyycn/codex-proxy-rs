use std::fmt;
use std::time::Duration;

use serde::Deserialize;

use crate::credential::discovery::MAX_OAUTH_RESPONSE_BYTES;
use crate::{
    ConfigError, OAuthError, OAuthErrorCode, OAuthHttpResponse, OAuthOperation, ProtocolViolation,
    SecretValue, VerificationEvidence,
};

const MAX_TOKEN_BYTES: usize = 64 * 1024;
const MAX_TOKEN_LIFETIME_SECONDS: u64 = 366 * 24 * 60 * 60;

/// Optional team principal fields sent during refresh.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthPrincipal {
    principal_type: String,
    principal_id: String,
}

impl fmt::Debug for OAuthPrincipal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthPrincipal")
            .field("principal_type", &self.principal_type)
            .field("principal_id", &"[REDACTED]")
            .finish()
    }
}

impl OAuthPrincipal {
    /// Creates validated principal metadata.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::InvalidPrincipal`] for empty, oversized, or
    /// control-character-bearing values.
    pub fn new(
        principal_type: impl Into<String>,
        principal_id: impl Into<String>,
    ) -> Result<Self, ConfigError> {
        let principal_type = principal_type.into();
        let principal_id = principal_id.into();
        if !valid_principal(&principal_type) || !valid_principal(&principal_id) {
            return Err(ConfigError::InvalidPrincipal);
        }

        Ok(Self {
            principal_type,
            principal_id,
        })
    }

    /// Returns the official principal type value.
    #[must_use]
    pub fn principal_type(&self) -> &str {
        &self.principal_type
    }

    /// Returns the official principal identifier.
    #[must_use]
    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }
}

/// Input for one refresh-token exchange.
#[derive(Clone)]
pub struct RefreshTokenGrant {
    refresh_token: SecretValue,
}

impl RefreshTokenGrant {
    /// Creates a refresh grant for an existing verified credential.
    #[must_use]
    pub fn new(refresh_token: SecretValue) -> Self {
        Self { refresh_token }
    }

    pub(crate) fn refresh_token(&self) -> &SecretValue {
        &self.refresh_token
    }
}

impl fmt::Debug for RefreshTokenGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RefreshTokenGrant")
            .field("refresh_token", &"[REDACTED]")
            .finish()
    }
}

/// Initial token set that crossed the mandatory verification boundary.
#[derive(Clone)]
pub struct VerifiedTokenSet {
    access_token: SecretValue,
    refresh_token: Option<SecretValue>,
    id_token: Option<SecretValue>,
    scope: String,
    expires_in: Option<Duration>,
    evidence: VerificationEvidence,
}

impl VerifiedTokenSet {
    pub(crate) fn new(
        tokens: UnverifiedTokenSet,
        evidence: VerificationEvidence,
        scope: String,
    ) -> Self {
        Self {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            id_token: tokens.id_token,
            scope,
            expires_in: tokens.expires_in,
            evidence,
        }
    }

    /// Returns the verified access token.
    #[must_use]
    pub fn access_token(&self) -> &SecretValue {
        &self.access_token
    }

    /// Returns the optional refresh token.
    #[must_use]
    pub fn refresh_token(&self) -> Option<&SecretValue> {
        self.refresh_token.as_ref()
    }

    /// 返回验证边界保留的签名 ID token；某些官方流程可能不返回。
    #[must_use]
    pub fn id_token(&self) -> Option<&SecretValue> {
        self.id_token.as_ref()
    }

    /// 返回该 token set 实际绑定的 OAuth scope 字符串。
    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Returns the server-provided lifetime.
    #[must_use]
    pub const fn expires_in(&self) -> Option<Duration> {
        self.expires_in
    }

    /// Returns trusted identity verification evidence.
    #[must_use]
    pub const fn evidence(&self) -> &VerificationEvidence {
        &self.evidence
    }
}

impl fmt::Debug for VerifiedTokenSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedTokenSet")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("id_token", &self.id_token.as_ref().map(|_| "[REDACTED]"))
            .field("scope", &"[REDACTED]")
            .field("expires_in", &self.expires_in)
            .field("evidence", &self.evidence)
            .finish()
    }
}

/// Result of refreshing an already verified credential. An omitted refresh
/// token means the caller must retain the existing one under revision CAS.
#[derive(Clone)]
pub struct RefreshedTokenSet {
    access_token: SecretValue,
    rotated_refresh_token: Option<SecretValue>,
    expires_in: Option<Duration>,
}

impl RefreshedTokenSet {
    /// Returns the replacement access token.
    #[must_use]
    pub fn access_token(&self) -> &SecretValue {
        &self.access_token
    }

    /// Returns a rotated refresh token when the server supplied one.
    #[must_use]
    pub fn rotated_refresh_token(&self) -> Option<&SecretValue> {
        self.rotated_refresh_token.as_ref()
    }

    /// Returns the replacement access-token lifetime.
    #[must_use]
    pub const fn expires_in(&self) -> Option<Duration> {
        self.expires_in
    }
}

impl fmt::Debug for RefreshedTokenSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RefreshedTokenSet")
            .field("access_token", &"[REDACTED]")
            .field(
                "rotated_refresh_token",
                &self.rotated_refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

pub(crate) struct UnverifiedTokenSet {
    pub(crate) access_token: SecretValue,
    pub(crate) refresh_token: Option<SecretValue>,
    pub(crate) id_token: Option<SecretValue>,
    pub(crate) expires_in: Option<Duration>,
}

#[derive(Deserialize)]
struct TokenSuccessWire {
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    token_type: Option<String>,
}

#[derive(Deserialize)]
struct OAuthErrorWire<'a> {
    #[serde(borrow)]
    error: Option<&'a str>,
}

pub(crate) fn parse_token_success(
    response: &OAuthHttpResponse,
    operation: OAuthOperation,
) -> Result<UnverifiedTokenSet, OAuthError> {
    validate_body_size(response, operation)?;
    let wire: TokenSuccessWire = serde_json::from_slice(response.body())
        .map_err(|_| OAuthError::protocol(operation, ProtocolViolation::InvalidJson))?;

    let access_token = required_secret(wire.access_token, "access_token", operation)?;
    let refresh_token = optional_secret(wire.refresh_token, "refresh_token", operation)?;
    let id_token = optional_secret(wire.id_token, "id_token", operation)?;
    if wire
        .token_type
        .as_deref()
        .is_some_and(|token_type| !token_type.eq_ignore_ascii_case("bearer"))
    {
        return Err(OAuthError::protocol(
            operation,
            ProtocolViolation::InvalidField("token_type"),
        ));
    }
    let expires_in = wire
        .expires_in
        .map(|seconds| {
            if seconds == 0 || seconds > MAX_TOKEN_LIFETIME_SECONDS {
                return Err(OAuthError::protocol(
                    operation,
                    ProtocolViolation::InvalidField("expires_in"),
                ));
            }
            Ok(Duration::from_secs(seconds))
        })
        .transpose()?;

    Ok(UnverifiedTokenSet {
        access_token,
        refresh_token,
        id_token,
        expires_in,
    })
}

pub fn parse_refresh_success(
    response: &OAuthHttpResponse,
) -> Result<RefreshedTokenSet, OAuthError> {
    let tokens = parse_token_success(response, OAuthOperation::RefreshToken)?;
    Ok(RefreshedTokenSet {
        access_token: tokens.access_token,
        rotated_refresh_token: tokens.refresh_token,
        expires_in: tokens.expires_in,
    })
}

pub fn parse_oauth_error(response: &OAuthHttpResponse, operation: OAuthOperation) -> OAuthError {
    if response.body().len() > MAX_OAUTH_RESPONSE_BYTES {
        return OAuthError::protocol(operation, ProtocolViolation::ResponseTooLarge);
    }

    let code = serde_json::from_slice::<OAuthErrorWire<'_>>(response.body())
        .ok()
        .and_then(|wire| wire.error)
        .map_or(OAuthErrorCode::Other, |value| match value {
            "access_denied" => OAuthErrorCode::AccessDenied,
            "invalid_grant" => OAuthErrorCode::InvalidGrant,
            "invalid_client" => OAuthErrorCode::InvalidClient,
            "invalid_scope" => OAuthErrorCode::InvalidScope,
            "temporarily_unavailable" => OAuthErrorCode::TemporarilyUnavailable,
            _ => OAuthErrorCode::Other,
        });

    if code == OAuthErrorCode::Other {
        OAuthError::HttpStatus {
            operation,
            status: response.status(),
        }
    } else {
        OAuthError::Server {
            operation,
            status: response.status(),
            code,
        }
    }
}

fn required_secret(
    value: Option<String>,
    field: &'static str,
    operation: OAuthOperation,
) -> Result<SecretValue, OAuthError> {
    let value = value
        .ok_or_else(|| OAuthError::protocol(operation, ProtocolViolation::MissingField(field)))?;
    validate_secret(value, field, operation)
}

fn optional_secret(
    value: Option<String>,
    field: &'static str,
    operation: OAuthOperation,
) -> Result<Option<SecretValue>, OAuthError> {
    value
        .map(|value| validate_secret(value, field, operation))
        .transpose()
}

fn validate_secret(
    value: String,
    field: &'static str,
    operation: OAuthOperation,
) -> Result<SecretValue, OAuthError> {
    if value.is_empty() || value.len() > MAX_TOKEN_BYTES || value.chars().any(char::is_control) {
        return Err(OAuthError::protocol(
            operation,
            ProtocolViolation::InvalidField(field),
        ));
    }
    Ok(SecretValue::new(value))
}

fn validate_body_size(
    response: &OAuthHttpResponse,
    operation: OAuthOperation,
) -> Result<(), OAuthError> {
    if response.body().len() > MAX_OAUTH_RESPONSE_BYTES {
        return Err(OAuthError::protocol(
            operation,
            ProtocolViolation::ResponseTooLarge,
        ));
    }
    Ok(())
}

fn valid_principal(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
