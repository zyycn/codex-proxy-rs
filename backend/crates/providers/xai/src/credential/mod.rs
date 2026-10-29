//! Grok Build OAuth credential 与运行时 selector

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
mod quota;
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
    GrokCatalogCache, GrokCatalogCacheError, GrokCatalogScope, GrokCredentialCatalogCache,
    GrokCredentialCatalogError, GrokCredentialCatalogSeed, GrokCredentialCatalogService,
    GrokPlanCatalog,
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
pub use quota::{
    GROK_FREE_ROLLING_WINDOW_SECONDS, GrokBillingPresentation, GrokCredentialQuotaService,
    GrokQuotaError, GrokQuotaPeriodKind, GrokQuotaSnapshot,
};
pub use refresh::{
    DueGrokCredential, GrokCredentialRecovery, GrokCredentialRecoveryOutcome,
    GrokCredentialRefreshError, GrokCredentialRefreshOutcome, GrokCredentialRefreshService,
    GrokCredentialRefresher, GrokOAuthRefreshClient, GrokRefreshFailure, GrokRefreshTokens,
};
pub use repository::{
    GrokAccountExport, GrokCredentialAdmin, GrokCredentialLifecycle, GrokCredentialRepository,
    GrokCredentialRepositoryError, VerifiedGrokAccount,
};
pub use secret::SecretValue;
pub use selector::GrokAccountSessionSelector;
pub use token::{
    OAuthPrincipal, RefreshTokenGrant, RefreshedTokenSet, VerifiedTokenSet, parse_oauth_error,
    parse_refresh_success,
};
pub use types::{
    CreateGrokCredential, GrokAccountProfile, GrokCredentialRecord, GrokOAuthSecret,
    PreparedGrokCredentialRotation, PreparedGrokCredentialRotationGuard, RotateGrokCredential,
    RotateManagedGrokCredential, UpdateGrokCredentialState, XAI_AUTHENTICATION_KIND_OAUTH,
};
pub use verification::{
    FailClosedTokenVerifier, TokenCandidate, TokenVerificationContext, TokenVerifier,
    VerificationEvidence, VerificationFlow, VerificationFuture, VerificationMethod,
};

/// 直连资格由 Provider 拥有，执行快照与只读导出使用同一判断
pub(crate) fn is_direct_account_kind(authentication_kind: &str) -> bool {
    authentication_kind == XAI_AUTHENTICATION_KIND_OAUTH
}
