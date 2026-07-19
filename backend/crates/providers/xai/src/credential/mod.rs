//! Grok Build OAuth credential 与运行时 selector。

mod authorization_code;
mod catalog;
mod client;
mod config;
pub(crate) mod discovery;
mod error;
mod http;
mod import;
mod oidc_verifier;
mod pkce;
mod refresh;
mod repository;
mod secret;
mod selector;
mod token;
mod types;
mod verification;

pub use authorization_code::{AuthorizationCallback, AuthorizationCodeGrant, PendingAuthorization};
pub use client::GrokOAuthClient;
pub use config::{
    AllowedRedirectUri, GrokOAuthConfig, OFFICIAL_CLIENT_ID, OFFICIAL_ISSUER,
    OFFICIAL_REDIRECT_URI, OFFICIAL_SCOPES, RedirectUriAllowlist,
};

pub use catalog::{
    GROK_FREE_ROLLING_WINDOW_SECONDS, GrokAccountCatalog, GrokBillingPresentation,
    GrokCatalogCacheError, GrokCredentialCatalogCache, GrokCredentialCatalogError,
    GrokCredentialCatalogSeed, GrokCredentialCatalogService, GrokCredentialCatalogSnapshot,
    GrokCredentialQuotaService, GrokQuotaError, GrokQuotaPeriodKind, GrokQuotaSnapshot,
};
pub use discovery::DiscoveryDocument;
pub use error::{
    CallbackRejection, ConfigError, FailureClass, OAuthError, OAuthErrorCode, OAuthOperation,
    ProtocolViolation, VerificationFailure,
};
pub use http::{
    FormField, FormValue, HttpHeader, HttpMethod, OAuthHttpRequest, OAuthHttpResponse,
    OAuthHttpTransport, TransportFailure, TransportFailureKind, TransportFuture,
};
pub use import::{
    GrokOAuthImportCandidate, GrokOAuthImportDocument, GrokOAuthImportEntry, GrokOAuthImportError,
    GrokOAuthImportMetadata, GrokOAuthImportTokens,
};
pub use oidc_verifier::ReqwestOidcTokenVerifier;
pub use pkce::Pkce;
pub use refresh::{
    DueGrokCredential, GrokCredentialRefreshError, GrokCredentialRefreshOutcome,
    GrokCredentialRefreshService, GrokCredentialRefresher, GrokOAuthRefreshClient,
    GrokRefreshFailure, GrokRefreshLeaseAcquisition, GrokRefreshLeaseCoordinator,
    GrokRefreshLeaseError, GrokRefreshLeaseGuard, GrokRefreshLeaseRequest, GrokRefreshTokens,
};
pub use repository::{
    GrokAccountExport, GrokCredentialAdmin, GrokCredentialLifecycle, GrokCredentialRepository,
    GrokCredentialRepositoryError, VerifiedGrokAccount,
};
pub use secret::SecretValue;
pub use selector::{
    GrokAccountSchedulingState, GrokAccountSessionSelector, GrokCredentialLeaseAcquisition,
    GrokCredentialLeaseCoordinator, GrokCredentialLeaseCoordinatorError, GrokCredentialLeaseGuard,
    GrokCredentialLeaseRequest,
};
pub use token::{
    OAuthPrincipal, RefreshTokenGrant, RefreshedTokenSet, VerifiedTokenSet, parse_oauth_error,
    parse_refresh_success,
};
pub use types::{
    CreateGrokCredential, GrokAccountProfile, GrokCredentialAvailability, GrokCredentialRecord,
    GrokOAuthSecret, PreparedGrokCredentialRotation, PreparedGrokCredentialRotationGuard,
    RotateGrokCredential, RotateManagedGrokCredential, UpdateGrokCredentialState,
};
pub use verification::{
    FailClosedTokenVerifier, TokenCandidate, TokenVerificationContext, TokenVerifier,
    VerificationEvidence, VerificationFlow, VerificationFuture, VerificationMethod,
};
