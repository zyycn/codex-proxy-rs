use thiserror::Error;

use crate::TransportFailureKind;

/// Static OAuth operation labels used for safe telemetry and classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthOperation {
    /// OpenID Provider discovery.
    Discovery,
    /// Authorization-code token exchange.
    AuthorizationCodeToken,
    /// Refresh-token exchange.
    RefreshToken,
    /// 已有 OAuth token 的受控导入验证。
    CredentialImport,
}

/// Stable OAuth error codes that are safe to expose to control-plane logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthErrorCode {
    /// The human rejected authorization.
    AccessDenied,
    /// A code or refresh token is invalid or already consumed.
    InvalidGrant,
    /// The configured public client is rejected.
    InvalidClient,
    /// The requested official scope set is rejected.
    InvalidScope,
    /// The authorization server is temporarily unavailable.
    TemporarilyUnavailable,
    /// An unrecognized code whose raw server text is intentionally discarded.
    Other,
}

/// High-level action a coordinator may take after a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Safe transient failure; a later, separately coordinated attempt may run.
    Transient,
    /// Send state is ambiguous and the one-time artifact must not be replayed.
    Ambiguous,
    /// Credential material is permanently rejected at its current revision.
    CredentialPermanent,
    /// Provider/client configuration is permanently rejected.
    ConfigurationPermanent,
    /// The human denied or must restart the interactive flow.
    UserActionRequired,
    /// Protocol or trust-boundary validation failed closed.
    Security,
    /// The official deployment does not offer the requested flow.
    Unsupported,
}

/// Configuration failures detected before any network request.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// Client version is empty, non-ASCII, or contains unsafe characters.
    #[error("invalid Grok client version")]
    InvalidClientVersion,
    /// Redirect URI is malformed, insecure, or contains forbidden components.
    #[error("invalid OAuth redirect URI")]
    InvalidRedirectUri,
    /// Redirect URI was not explicitly registered in the local allowlist.
    #[error("OAuth redirect URI is not allowlisted")]
    RedirectUriNotAllowlisted,
    /// Issuer differs from the official Grok Build issuer.
    #[error("OIDC issuer is not the official Grok Build issuer")]
    UntrustedIssuer,
    /// A discovered endpoint leaves the official issuer origin.
    #[error("OIDC discovery returned an untrusted endpoint")]
    UntrustedEndpoint,
    /// Optional team principal metadata is empty or contains unsafe text.
    #[error("invalid OAuth principal metadata")]
    InvalidPrincipal,
}

/// Safe, field-oriented protocol validation errors.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolViolation {
    /// JSON could not be decoded into the expected wire schema.
    #[error("OAuth response is not valid JSON")]
    InvalidJson,
    /// A required wire field is absent.
    #[error("OAuth response is missing field `{0}`")]
    MissingField(&'static str),
    /// A wire field violates its format or size contract.
    #[error("OAuth response contains invalid field `{0}`")]
    InvalidField(&'static str),
    /// A response exceeded the bounded parser limit.
    #[error("OAuth response exceeds the maximum body size")]
    ResponseTooLarge,
    /// The discovery document permits the insecure `none` signing algorithm.
    #[error("OIDC discovery advertises an insecure signing algorithm")]
    InsecureSigningAlgorithm,
}

/// Callback failures that never retain the authorization code or state value.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum CallbackRejection {
    /// Query contains repeated security-sensitive keys.
    #[error("OAuth callback contains a duplicate parameter")]
    DuplicateParameter,
    /// Provider callback omitted the one-time state.
    #[error("OAuth callback is missing state")]
    MissingState,
    /// Provider callback omitted the authorization code.
    #[error("OAuth callback is missing code")]
    MissingCode,
    /// One-time state does not match the pending flow.
    #[error("OAuth callback state mismatch")]
    StateMismatch,
    /// Authorization server returned `access_denied`.
    #[error("OAuth authorization was denied")]
    AccessDenied,
    /// Authorization server returned another callback error.
    #[error("OAuth authorization callback was rejected")]
    ProviderRejected,
}

/// Reasons an unverified token set cannot cross the credential boundary.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum VerificationFailure {
    /// No JWT/JWKS or authoritative user-info verifier was wired.
    #[error("token verification is unavailable; refusing unverified credentials")]
    Unavailable,
    /// Authorization-code response omitted its required ID token.
    #[error("authorization-code response is missing an ID token")]
    MissingIdToken,
    /// The verifier rejected signature, issuer, audience, nonce, expiry, or identity.
    #[error("token verification failed")]
    Rejected,
    /// Authorization-code flow was not verified through an ID token.
    #[error("authorization-code flow requires verified ID-token evidence")]
    WrongEvidence,
}

/// Secret-free OAuth protocol error.
#[derive(Debug, Error)]
pub enum OAuthError {
    /// Local trust or redirect configuration is invalid.
    #[error(transparent)]
    Configuration(#[from] ConfigError),
    /// Replaceable transport failed without retaining its raw error message.
    #[error("OAuth transport failed during {operation:?}: {kind:?}")]
    Transport {
        /// Operation being executed.
        operation: OAuthOperation,
        /// Safe send-state classification.
        kind: TransportFailureKind,
    },
    /// Non-success HTTP response without a recognized OAuth code.
    #[error("OAuth endpoint returned HTTP {status} during {operation:?}")]
    HttpStatus {
        /// Operation being executed.
        operation: OAuthOperation,
        /// HTTP status code.
        status: u16,
    },
    /// Recognized OAuth error response.
    #[error("OAuth endpoint returned {code:?} during {operation:?}")]
    Server {
        /// Operation being executed.
        operation: OAuthOperation,
        /// HTTP status code.
        status: u16,
        /// Stable OAuth error code.
        code: OAuthErrorCode,
    },
    /// Callback was rejected before token exchange.
    #[error(transparent)]
    Callback(#[from] CallbackRejection),
    /// Wire response violated the strict parser contract.
    #[error("{operation:?} protocol violation: {violation}")]
    Protocol {
        /// Operation being parsed.
        operation: OAuthOperation,
        /// Safe field-oriented reason.
        violation: ProtocolViolation,
    },
    /// Token set failed the mandatory verification boundary.
    #[error(transparent)]
    Verification(#[from] VerificationFailure),
    /// Cryptographically secure entropy was unavailable before a flow started.
    #[error("secure OAuth entropy is unavailable")]
    EntropyUnavailable,
}

impl OAuthError {
    /// Returns a low-cardinality failure class for orchestration decisions.
    #[must_use]
    pub fn class(&self) -> FailureClass {
        match self {
            Self::Transport {
                kind: TransportFailureKind::Ambiguous | TransportFailureKind::Timeout,
                ..
            } => FailureClass::Ambiguous,
            Self::Transport { .. } => FailureClass::Transient,
            Self::EntropyUnavailable => FailureClass::Transient,
            Self::HttpStatus { status: 429, .. }
            | Self::HttpStatus {
                status: 500..=599, ..
            }
            | Self::Server {
                code: OAuthErrorCode::TemporarilyUnavailable,
                ..
            } => FailureClass::Transient,
            Self::Server {
                code: OAuthErrorCode::InvalidGrant,
                ..
            } => FailureClass::CredentialPermanent,
            Self::Server {
                code: OAuthErrorCode::InvalidClient | OAuthErrorCode::InvalidScope,
                ..
            } => FailureClass::ConfigurationPermanent,
            Self::Server {
                code: OAuthErrorCode::AccessDenied,
                ..
            }
            | Self::Callback(CallbackRejection::AccessDenied) => FailureClass::UserActionRequired,
            Self::Configuration(_)
            | Self::Callback(_)
            | Self::Protocol { .. }
            | Self::Verification(_)
            | Self::Server { .. }
            | Self::HttpStatus { .. } => FailureClass::Security,
        }
    }

    pub(crate) fn transport(operation: OAuthOperation, failure: crate::TransportFailure) -> Self {
        Self::Transport {
            operation,
            kind: failure.kind(),
        }
    }

    pub(crate) fn protocol(operation: OAuthOperation, violation: ProtocolViolation) -> Self {
        Self::Protocol {
            operation,
            violation,
        }
    }
}
