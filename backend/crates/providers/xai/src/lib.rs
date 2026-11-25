//! 官方 Grok Build OAuth Provider 边界

mod admin;
mod billing;
mod credential;
mod oauth_http;
pub mod profile;
mod provider;
mod reasoning_replay;
mod secret;
pub mod transport;

use std::sync::Arc;
use std::time::Duration;

use gateway_admin::ports::provider::ProviderBundle;
use gateway_core::account::ProviderAccountStore;
use gateway_core::provider_ports::ProviderStorePorts;
use gateway_core::routing::ProviderKind;

use crate::admin::{XaiAdminProvider, XaiAdminServices};
use crate::profile::OfficialGrokCliReleaseTransport;

pub use profile::{XaiWireProfile, XaiWireProfileState};

pub use credential::{
    AllowedRedirectUri, AuthorizationCallback, AuthorizationCodeGrant, CallbackRejection,
    ConfigError, CreateGrokCredential, DiscoveryDocument, DueGrokCredential,
    FailClosedTokenVerifier, FailureClass, GROK_FREE_ROLLING_WINDOW_SECONDS, GrokAccountExport,
    GrokAccountProfile, GrokAccountSessionSelector, GrokBillingPresentation, GrokCatalogCache,
    GrokCatalogCacheError, GrokCatalogScope, GrokCredentialAdmin, GrokCredentialCatalogCache,
    GrokCredentialCatalogError, GrokCredentialCatalogSeed, GrokCredentialCatalogService,
    GrokCredentialLifecycle, GrokCredentialQuotaService, GrokCredentialRecord,
    GrokCredentialRecovery, GrokCredentialRecoveryOutcome, GrokCredentialRefreshError,
    GrokCredentialRefreshOutcome, GrokCredentialRefreshService, GrokCredentialRefresher,
    GrokCredentialRepository, GrokCredentialRepositoryError, GrokOAuthClient, GrokOAuthConfig,
    GrokOAuthImportCandidate, GrokOAuthImportDocument, GrokOAuthImportEntry, GrokOAuthImportError,
    GrokOAuthImportMetadata, GrokOAuthImportTokens, GrokOAuthRefreshClient, GrokOAuthSecret,
    GrokPlanCatalog, GrokQuotaError, GrokQuotaPeriodKind, GrokQuotaSnapshot, GrokRefreshFailure,
    GrokRefreshTokens, OAuthError, OAuthErrorCode, OAuthOperation, OAuthPrincipal,
    OFFICIAL_CLIENT_ID, OFFICIAL_ISSUER, OFFICIAL_REDIRECT_URI, OFFICIAL_SCOPES,
    PendingAuthorization, Pkce, PreparedGrokCredentialRotation,
    PreparedGrokCredentialRotationGuard, ProtocolViolation, RedirectUriAllowlist,
    RefreshTokenGrant, RefreshedTokenSet, ReqwestOidcTokenVerifier, RotateGrokCredential,
    RotateManagedGrokCredential, TokenCandidate, TokenVerificationContext, TokenVerifier,
    UpdateGrokCredentialState, VerificationEvidence, VerificationFailure, VerificationFlow,
    VerificationFuture, VerificationMethod, VerifiedGrokAccount, VerifiedTokenSet,
    parse_oauth_error, parse_refresh_success,
};
pub use oauth_http::{
    FormField, FormValue, HttpHeader, HttpMethod, OAuthHttpRequest, OAuthHttpResponse,
    OAuthHttpTransport, TransportFailure, TransportFailureKind, TransportFuture,
};
pub use profile::{
    GROK_CLI_RELEASE_URL, GrokCliReleaseError, GrokCliReleaseService, GrokCliReleaseSnapshot,
    GrokCliReleaseStatus, GrokCliReleaseTransport,
};
pub use provider::GrokBuildProvider;
pub use secret::SecretValue;
pub use transport::{
    GROK_BILLING_URL, GROK_CLI_BASE_URL, GROK_MODEL_CATALOG_URL, GROK_SUBSCRIPTION_URL,
    GrokBillingClient, GrokBillingError, GrokBillingRequest, GrokBillingSnapshot,
    GrokBillingTransport, GrokBillingTransportError, GrokBillingTransportErrorKind,
    GrokBillingTransportFuture, GrokBillingTransportResponse, GrokCanonicalDecoder,
    GrokCatalogApiBackend, GrokCatalogCapabilities, GrokCatalogCapabilityEvidence,
    GrokCatalogLimits, GrokCatalogMetadata, GrokCatalogModel, GrokCatalogReasoningEffort,
    GrokClientIdentity, GrokCompactionDecodeError, GrokCompactionRequest,
    GrokCompactionSummaryDecoder, GrokCredentialFailure, GrokCredentialFeedbackFuture,
    GrokDnsResolutionError, GrokDnsResolutionPlan, GrokDnsResolutionPolicy, GrokEndpointPolicy,
    GrokHeader, GrokHeaderValue, GrokInferenceChunkStream, GrokInferenceClientCacheStatus,
    GrokInferenceDnsObservation, GrokInferenceDnsSource, GrokInferenceRequest,
    GrokInferenceResponse, GrokInferenceTransport, GrokInferenceTransportError,
    GrokInferenceTransportErrorKind, GrokInferenceTransportFuture, GrokInferenceTransportMetrics,
    GrokModelCatalogClient, GrokModelCatalogError, GrokModelCatalogRequest,
    GrokModelCatalogSession, GrokModelCatalogSessionError, GrokModelCatalogSnapshot,
    GrokModelCatalogTransport, GrokModelCatalogTransportError, GrokModelCatalogTransportErrorKind,
    GrokModelCatalogTransportFuture, GrokModelCatalogTransportResponse, GrokProviderConfigError,
    GrokProviderTransport, GrokRequestEncodeError, GrokReqwestTransportBuildError,
    GrokResponsesRequest, GrokSessionAffinityKey, GrokSessionBinding, GrokSessionDataError,
    GrokSessionLeaseGuard, GrokSessionSelection, GrokSessionSelector, GrokSessionSelectorError,
    GrokSessionSelectorFuture, MAX_GROK_BILLING_BYTES, MAX_GROK_MODEL_CATALOG_BYTES,
    OfficialGrokEndpointPolicy, ReqwestGrokInferenceTransport, ReqwestGrokModelCatalogTransport,
    ReqwestOAuthTransport, SelectedGrokSession, XAI_PROVIDER_NAME, build_grok_headers,
    parse_grok_billing, parse_grok_model_catalog,
};

/// 构造 xAI 数据面、管理面准备器与 Provider-owned 后台任务
pub async fn initialize(ports: ProviderStorePorts) -> Result<ProviderBundle, XaiInitializeError> {
    let profile = XaiWireProfileState::new(XaiWireProfile::default());
    let oauth_config = GrokOAuthConfig::official().map_err(XaiInitializeError::Config)?;
    let cli_release = Arc::new(GrokCliReleaseService::new(
        profile.clone(),
        Arc::new(
            OfficialGrokCliReleaseTransport::new().map_err(|_| XaiInitializeError::Transport)?,
        ),
    ));
    let cli_release_status = cli_release.status();
    let provider_kind =
        ProviderKind::new(XAI_PROVIDER_NAME).map_err(|_| XaiInitializeError::ProviderKind)?;
    let accounts: Arc<dyn ProviderAccountStore> = ports.accounts();
    let leases = ports.leases();
    let cooldowns = ports.cooldowns();
    let account_feedback = ports.account_feedback();
    let runtime_policy = ports.runtime_policy();
    let initial_profile = profile::selection::GrokClientProfileSelection::default()
        .document()
        .map_err(|_| XaiInitializeError::RuntimePolicy)?;
    let configured_profile = runtime_policy
        .initialize_request_profile(&provider_kind, initial_profile)
        .await
        .map_err(|_| XaiInitializeError::RuntimePolicy)?;
    profile::selection::GrokClientProfileSelection::parse(&configured_profile)
        .map_err(|_| XaiInitializeError::RuntimePolicy)?;
    let repository = GrokCredentialRepository::new(Arc::clone(&accounts));
    let endpoint_policy: Arc<dyn GrokEndpointPolicy> = Arc::new(OfficialGrokEndpointPolicy);

    let catalog_cache = Arc::new(
        GrokCatalogCache::new(ports.catalog_cache()).map_err(|_| XaiInitializeError::Catalog)?,
    );
    let catalog_cache_port: Arc<dyn GrokCredentialCatalogCache> = catalog_cache.clone();
    let upstream_catalog = Arc::new(
        ReqwestGrokModelCatalogTransport::new(Arc::clone(&endpoint_policy))
            .map_err(|_| XaiInitializeError::Transport)?,
    );
    let catalog_transport: Arc<dyn GrokModelCatalogTransport> = upstream_catalog.clone();
    let billing_transport: Arc<dyn GrokBillingTransport> = upstream_catalog;
    let catalog = Arc::new(GrokCredentialCatalogService::new(
        repository.clone(),
        catalog_transport,
        catalog_cache_port,
        profile.clone(),
    ));
    let quota = Arc::new(GrokCredentialQuotaService::new(
        repository.clone(),
        billing_transport,
        profile.clone(),
    ));
    let selector: Arc<dyn GrokSessionSelector> = Arc::new(GrokAccountSessionSelector::new(
        provider_kind.clone(),
        repository.clone(),
        catalog_cache,
        Arc::clone(&quota),
        Arc::clone(&leases),
        Arc::clone(&cooldowns),
        Arc::clone(&account_feedback),
        ports.session_affinity(),
    ));
    let inference: Arc<dyn GrokInferenceTransport> = Arc::new(
        ReqwestGrokInferenceTransport::new(Arc::clone(&endpoint_policy))
            .map_err(|_| XaiInitializeError::Transport)?,
    );
    let oauth_transport: Arc<dyn OAuthHttpTransport> = Arc::new(
        ReqwestOAuthTransport::new(Arc::clone(&endpoint_policy))
            .map_err(|_| XaiInitializeError::Transport)?,
    );
    let token_verifier: Arc<dyn TokenVerifier> = Arc::new(
        ReqwestOidcTokenVerifier::new(endpoint_policy, Duration::from_secs(60 * 60))
            .map_err(|_| XaiInitializeError::Transport)?,
    );
    let oauth = Arc::new(GrokOAuthClient::new(
        oauth_config.clone(),
        profile.clone(),
        oauth_transport,
        token_verifier,
    ));
    let refresher: Arc<dyn GrokCredentialRefresher> =
        Arc::new(GrokOAuthRefreshClient::new(Arc::clone(&oauth)));
    let refresh = Arc::new(GrokCredentialRefreshService::new(
        repository.clone(),
        refresher,
        Arc::clone(&catalog),
        Arc::clone(&leases),
        ports.credential_state(),
        Arc::clone(&runtime_policy),
    ));
    let credential_recovery: Arc<dyn GrokCredentialRecovery> = refresh.clone();
    let core = Arc::new(
        GrokBuildProvider::new(
            selector,
            inference,
            Arc::clone(&catalog),
            credential_recovery,
            account_feedback,
            profile.clone(),
        )
        .map_err(|_| XaiInitializeError::Transport)?,
    );
    let admin = Arc::new(XaiAdminProvider::new(
        provider_kind.clone(),
        profile,
        Arc::clone(&accounts),
        XaiAdminServices {
            repository,
            oauth_config,
            oauth,
            pending: ports.oauth_pending(),
            refresh: Arc::clone(&refresh),
            quota: Arc::clone(&quota),
            catalog: Arc::clone(&catalog),
            cooldowns: Arc::clone(&cooldowns),
        },
        cli_release_status,
    ));
    let workers = provider::workers::contributions(
        refresh,
        quota,
        catalog,
        accounts,
        provider_kind,
        cli_release,
    )
    .map_err(|_| XaiInitializeError::Worker)?;

    Ok(ProviderBundle {
        core,
        admin,
        workers,
    })
}

/// xAI 初始化失败的脱敏分类
#[derive(Debug, thiserror::Error)]
pub enum XaiInitializeError {
    #[error(transparent)]
    Config(ConfigError),
    #[error("xAI runtime policy is unavailable")]
    RuntimePolicy,
    #[error("xAI Provider kind is invalid")]
    ProviderKind,
    #[error("xAI transport could not initialize")]
    Transport,
    #[error("xAI model catalog could not initialize")]
    Catalog,
    #[error("xAI credential refresh could not initialize")]
    Refresh,
    #[error("xAI worker plan is invalid")]
    Worker,
}
