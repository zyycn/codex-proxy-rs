//! Official Grok Build OAuth Provider boundary.

pub mod credential;
mod provider;
pub mod transport;

pub use credential::{
    AllowedRedirectUri, AuthorizationCallback, AuthorizationCodeGrant, CallbackRejection,
    ConfigError, CreateGrokCredential, DiscoveryDocument, DueGrokCredential,
    FailClosedTokenVerifier, FailureClass, FormField, FormValue, GROK_FREE_ROLLING_WINDOW_SECONDS,
    GrokAccountCatalog, GrokAccountExport, GrokAccountProfile, GrokAccountSchedulingState,
    GrokAccountSessionSelector, GrokBillingPresentation, GrokCatalogCacheError,
    GrokCredentialAdmin, GrokCredentialAvailability, GrokCredentialCatalogCache,
    GrokCredentialCatalogError, GrokCredentialCatalogSeed, GrokCredentialCatalogService,
    GrokCredentialCatalogSnapshot, GrokCredentialLeaseAcquisition, GrokCredentialLeaseCoordinator,
    GrokCredentialLeaseCoordinatorError, GrokCredentialLeaseGuard, GrokCredentialLeaseRequest,
    GrokCredentialLifecycle, GrokCredentialQuotaService, GrokCredentialRecord,
    GrokCredentialRefreshError, GrokCredentialRefreshOutcome, GrokCredentialRefreshService,
    GrokCredentialRefresher, GrokCredentialRepository, GrokCredentialRepositoryError,
    GrokOAuthClient, GrokOAuthConfig, GrokOAuthImportCandidate, GrokOAuthImportDocument,
    GrokOAuthImportEntry, GrokOAuthImportError, GrokOAuthImportMetadata, GrokOAuthImportTokens,
    GrokOAuthRefreshClient, GrokOAuthSecret, GrokQuotaError, GrokQuotaPeriodKind,
    GrokQuotaSnapshot, GrokRefreshFailure, GrokRefreshLeaseAcquisition,
    GrokRefreshLeaseCoordinator, GrokRefreshLeaseError, GrokRefreshLeaseGuard,
    GrokRefreshLeaseRequest, GrokRefreshTokens, HttpHeader, HttpMethod, OAuthError, OAuthErrorCode,
    OAuthHttpRequest, OAuthHttpResponse, OAuthHttpTransport, OAuthOperation, OAuthPrincipal,
    OFFICIAL_CLIENT_ID, OFFICIAL_ISSUER, OFFICIAL_REDIRECT_URI, OFFICIAL_SCOPES,
    PendingAuthorization, Pkce, PreparedGrokCredentialRotation,
    PreparedGrokCredentialRotationGuard, ProtocolViolation, RedirectUriAllowlist,
    RefreshTokenGrant, RefreshedTokenSet, ReqwestOidcTokenVerifier, RotateGrokCredential,
    RotateManagedGrokCredential, SecretValue, TokenCandidate, TokenVerificationContext,
    TokenVerifier, TransportFailure, TransportFailureKind, TransportFuture,
    UpdateGrokCredentialState, VerificationEvidence, VerificationFailure, VerificationFlow,
    VerificationFuture, VerificationMethod, VerifiedGrokAccount, VerifiedTokenSet,
    parse_oauth_error, parse_refresh_success,
};
pub use provider::GrokBuildProvider;
pub use transport::{
    GROK_BILLING_URL, GROK_CLI_BASE_URL, GROK_MODEL_CATALOG_URL, GrokBillingClient,
    GrokBillingError, GrokBillingRequest, GrokBillingSnapshot, GrokBillingTransport,
    GrokBillingTransportError, GrokBillingTransportErrorKind, GrokBillingTransportFuture,
    GrokBillingTransportResponse, GrokCanonicalDecoder, GrokCatalogApiBackend,
    GrokCatalogCapabilities, GrokCatalogCapabilityEvidence, GrokCatalogLimits, GrokCatalogMetadata,
    GrokCatalogModel, GrokCredentialFailure, GrokCredentialFeedbackFuture, GrokDnsResolutionError,
    GrokDnsResolutionPlan, GrokDnsResolutionPolicy, GrokEndpointPolicy, GrokHeader,
    GrokHeaderValue, GrokInferenceChunkStream, GrokInferenceRequest, GrokInferenceResponse,
    GrokInferenceTransport, GrokInferenceTransportError, GrokInferenceTransportErrorKind,
    GrokInferenceTransportFuture, GrokModelCatalogClient, GrokModelCatalogError,
    GrokModelCatalogRequest, GrokModelCatalogSession, GrokModelCatalogSessionError,
    GrokModelCatalogSnapshot, GrokModelCatalogTransport, GrokModelCatalogTransportError,
    GrokModelCatalogTransportErrorKind, GrokModelCatalogTransportFuture,
    GrokModelCatalogTransportResponse, GrokProviderConfigError, GrokProviderInstanceConfig,
    GrokProviderTransport, GrokRequestEncodeError, GrokResponsesRequest, GrokSessionBinding,
    GrokSessionDataError, GrokSessionLeaseGuard, GrokSessionSelection, GrokSessionSelector,
    GrokSessionSelectorError, GrokSessionSelectorFuture, MAX_GROK_BILLING_BYTES,
    MAX_GROK_MODEL_CATALOG_BYTES, ReqwestGrokInferenceTransport, ReqwestGrokModelCatalogTransport,
    ReqwestOAuthTransport, SelectedGrokSession, XAI_PROVIDER_NAME, build_grok_headers,
    grok_billing_breakdown, parse_grok_billing, parse_grok_model_catalog,
};
