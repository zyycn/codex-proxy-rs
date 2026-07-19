use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use url::Url;

use crate::{SecretValue, VerificationFailure};

/// OAuth flow whose initial access token is being verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationFlow {
    /// Authorization Code + PKCE with a mandatory nonce-bound ID token.
    AuthorizationCode,
    /// 已有 token 的导入；必须通过官方 user-info 验证当前 access token。
    CredentialImport,
    /// 导入的 AT 已过期并经官方 RT exchange 更新；必须验证官方 user-info。
    CredentialImportRefreshed,
}

/// Trusted verification mechanism reported by the injected verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationMethod {
    /// Full JWT/JWKS validation including issuer, audience, expiry, and nonce.
    IdToken,
    /// Authoritative official user-info lookup using the access token.
    UserInfo,
}

/// Evidence produced by a trusted verifier. Identity is redacted from debug
/// output and is not derived by this crate through unverified JWT decoding.
#[derive(Clone)]
pub struct VerificationEvidence {
    method: VerificationMethod,
    subject: SecretValue,
}

impl VerificationEvidence {
    /// Creates evidence after full ID-token verification.
    #[must_use]
    pub fn id_token(subject: String) -> Self {
        Self {
            method: VerificationMethod::IdToken,
            subject: SecretValue::new(subject),
        }
    }

    /// Creates evidence after an authoritative official user-info lookup.
    #[must_use]
    pub fn user_info(subject: String) -> Self {
        Self {
            method: VerificationMethod::UserInfo,
            subject: SecretValue::new(subject),
        }
    }

    /// Returns the verification method.
    #[must_use]
    pub const fn method(&self) -> VerificationMethod {
        self.method
    }

    /// Exposes the verified subject at the credential construction boundary.
    #[must_use]
    pub fn subject(&self) -> &str {
        self.subject.expose()
    }
}

impl fmt::Debug for VerificationEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerificationEvidence")
            .field("method", &self.method)
            .field("subject", &"[REDACTED]")
            .finish()
    }
}

/// Immutable trust context supplied to a token verifier.
#[derive(Debug, Clone, Copy)]
pub struct TokenVerificationContext<'a> {
    flow: VerificationFlow,
    issuer: &'a Url,
    client_id: &'a str,
    jwks_uri: &'a Url,
    userinfo_endpoint: &'a Url,
    signing_algorithms: &'a [String],
    expected_nonce: Option<&'a SecretValue>,
}

impl<'a> TokenVerificationContext<'a> {
    pub fn new(
        flow: VerificationFlow,
        issuer: &'a Url,
        client_id: &'a str,
        jwks_uri: &'a Url,
        userinfo_endpoint: &'a Url,
        signing_algorithms: &'a [String],
        expected_nonce: Option<&'a SecretValue>,
    ) -> Self {
        Self {
            flow,
            issuer,
            client_id,
            jwks_uri,
            userinfo_endpoint,
            signing_algorithms,
            expected_nonce,
        }
    }

    /// Returns the flow being verified.
    #[must_use]
    pub const fn flow(&self) -> VerificationFlow {
        self.flow
    }

    /// Returns the exact expected issuer.
    #[must_use]
    pub fn issuer(&self) -> &Url {
        self.issuer
    }

    /// Returns the exact expected audience/client identifier.
    #[must_use]
    pub const fn client_id(&self) -> &str {
        self.client_id
    }

    /// Returns the same-origin JWKS URL from validated discovery.
    #[must_use]
    pub fn jwks_uri(&self) -> &Url {
        self.jwks_uri
    }

    /// Returns the same-origin authoritative user-info URL from discovery.
    #[must_use]
    pub fn userinfo_endpoint(&self) -> &Url {
        self.userinfo_endpoint
    }

    /// Returns discovery-advertised algorithms. Implementations must intersect
    /// them with a local cryptographic allowlist and reject `none`.
    #[must_use]
    pub const fn signing_algorithms(&self) -> &[String] {
        self.signing_algorithms
    }

    /// Returns the expected nonce for authorization-code flows.
    #[must_use]
    pub const fn expected_nonce(&self) -> Option<&SecretValue> {
        self.expected_nonce
    }
}

/// Borrowed token material available only to the trusted verification port.
pub struct TokenCandidate<'a> {
    access_token: &'a SecretValue,
    id_token: Option<&'a SecretValue>,
    expires_in: Option<Duration>,
}

impl<'a> TokenCandidate<'a> {
    pub const fn new(
        access_token: &'a SecretValue,
        id_token: Option<&'a SecretValue>,
        expires_in: Option<Duration>,
    ) -> Self {
        Self {
            access_token,
            id_token,
            expires_in,
        }
    }

    /// Returns the access token for authoritative user-info verification.
    #[must_use]
    pub const fn access_token(&self) -> &SecretValue {
        self.access_token
    }

    /// Returns the ID token for full JWT/JWKS validation, when provided.
    #[must_use]
    pub const fn id_token(&self) -> Option<&SecretValue> {
        self.id_token
    }

    /// Returns the server-provided lifetime.
    #[must_use]
    pub const fn expires_in(&self) -> Option<Duration> {
        self.expires_in
    }
}

impl fmt::Debug for TokenCandidate<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TokenCandidate")
            .field("access_token", &"[REDACTED]")
            .field("id_token", &self.id_token.map(|_| "[REDACTED]"))
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// Future returned by a token verification port.
pub type VerificationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<VerificationEvidence, VerificationFailure>> + Send + 'a>>;

/// Trust boundary for full JWT/JWKS or authoritative user-info validation.
pub trait TokenVerifier: Send + Sync {
    /// Verifies an initial token set. Implementations must never base64-decode
    /// JWT claims without signature and claim validation.
    fn verify<'a>(
        &'a self,
        context: TokenVerificationContext<'a>,
        candidate: TokenCandidate<'a>,
    ) -> VerificationFuture<'a>;
}

/// Default verifier that makes incomplete wiring fail closed.
#[derive(Debug, Default)]
pub struct FailClosedTokenVerifier;

impl TokenVerifier for FailClosedTokenVerifier {
    fn verify<'a>(
        &'a self,
        _context: TokenVerificationContext<'a>,
        _candidate: TokenCandidate<'a>,
    ) -> VerificationFuture<'a> {
        Box::pin(async { Err(VerificationFailure::Unavailable) })
    }
}
