//! Official Grok Build OAuth Provider boundary.

mod admin;
pub mod config;
pub mod credential;
mod provider;
pub mod transport;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gateway_admin::ports::provider::ProviderAdmin;
use gateway_core::engine::credential::ProviderAccountStore;
use gateway_core::engine::provider::Provider;
use gateway_core::provider_ports::ProviderStorePorts;
use gateway_core::routing::ProviderKind;
use gateway_core::task::WorkerContribution;

use crate::admin::{XaiAdminProvider, XaiAdminServices};

pub use config::{XaiConfig, XaiConfigError};

pub use credential::{
    AllowedRedirectUri, AuthorizationCallback, AuthorizationCodeGrant, CallbackRejection,
    ConfigError, CreateGrokCredential, DiscoveryDocument, DueGrokCredential,
    FailClosedTokenVerifier, FailureClass, FormField, FormValue, GROK_FREE_ROLLING_WINDOW_SECONDS,
    GrokAccountCatalog, GrokAccountExport, GrokAccountProfile, GrokAccountSessionSelector,
    GrokBillingPresentation, GrokCatalogCache, GrokCatalogCacheError, GrokCredentialAdmin,
    GrokCredentialAvailability, GrokCredentialCatalogCache, GrokCredentialCatalogError,
    GrokCredentialCatalogSeed, GrokCredentialCatalogService, GrokCredentialCatalogSnapshot,
    GrokCredentialLifecycle, GrokCredentialQuotaService, GrokCredentialRecord,
    GrokCredentialRefreshError, GrokCredentialRefreshOutcome, GrokCredentialRefreshService,
    GrokCredentialRefresher, GrokCredentialRepository, GrokCredentialRepositoryError,
    GrokOAuthClient, GrokOAuthConfig, GrokOAuthImportCandidate, GrokOAuthImportDocument,
    GrokOAuthImportEntry, GrokOAuthImportError, GrokOAuthImportMetadata, GrokOAuthImportTokens,
    GrokOAuthRefreshClient, GrokOAuthSecret, GrokQuotaError, GrokQuotaPeriodKind,
    GrokQuotaSnapshot, GrokRefreshFailure, GrokRefreshTokens, HttpHeader, HttpMethod, OAuthError,
    OAuthErrorCode, OAuthHttpRequest, OAuthHttpResponse, OAuthHttpTransport, OAuthOperation,
    OAuthPrincipal, OFFICIAL_CLIENT_ID, OFFICIAL_ISSUER, OFFICIAL_REDIRECT_URI, OFFICIAL_SCOPES,
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
    GrokProviderTransport, GrokRequestEncodeError, GrokReqwestTransportBuildError,
    GrokResponsesRequest, GrokSessionBinding, GrokSessionDataError, GrokSessionLeaseGuard,
    GrokSessionSelection, GrokSessionSelector, GrokSessionSelectorError, GrokSessionSelectorFuture,
    MAX_GROK_BILLING_BYTES, MAX_GROK_MODEL_CATALOG_BYTES, OfficialGrokEndpointPolicy,
    ReqwestGrokInferenceTransport, ReqwestGrokModelCatalogTransport, ReqwestOAuthTransport,
    SelectedGrokSession, XAI_PROVIDER_NAME, build_grok_headers, grok_billing_breakdown,
    parse_grok_billing, parse_grok_model_catalog,
};

/// xAI 初始化后交给组装根的最小能力集。
pub struct ProviderBundle {
    core_provider: Arc<dyn Provider>,
    admin_provider: Arc<dyn ProviderAdmin>,
    worker_contributions: Vec<WorkerContribution>,
}

/// 构造 xAI 数据面、管理面准备器与 Provider-owned 后台任务。
pub async fn initialize(
    mut config: XaiConfig,
    ports: ProviderStorePorts,
) -> Result<ProviderBundle, XaiInitializeError> {
    config
        .resolve_and_validate(Path::new("."))
        .map_err(XaiInitializeError::Config)?;
    let oauth_config = config.oauth_config().map_err(XaiInitializeError::Config)?;
    let provider_kind =
        ProviderKind::new(XAI_PROVIDER_NAME).map_err(|_| XaiInitializeError::ProviderKind)?;
    let accounts: Arc<dyn ProviderAccountStore> = ports.accounts();
    let leases = ports.leases();
    let runtime_policy = ports.runtime_policy();
    let instances = ports.instances();
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
    ));
    let quota = Arc::new(GrokCredentialQuotaService::new(
        repository.clone(),
        billing_transport,
    ));
    let selector: Arc<dyn GrokSessionSelector> = Arc::new(GrokAccountSessionSelector::new(
        repository.clone(),
        catalog_cache,
        Arc::clone(&quota),
        Arc::clone(&leases),
    ));
    let inference: Arc<dyn GrokInferenceTransport> = Arc::new(
        ReqwestGrokInferenceTransport::new(Arc::clone(&endpoint_policy))
            .map_err(|_| XaiInitializeError::Transport)?,
    );
    let core_provider: Arc<dyn Provider> = Arc::new(GrokBuildProvider::new(
        selector,
        inference,
        Arc::clone(&catalog),
    ));

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
        Arc::clone(&runtime_policy),
    ));
    let admin_provider: Arc<dyn ProviderAdmin> = Arc::new(XaiAdminProvider::new(
        provider_kind.clone(),
        Arc::clone(&accounts),
        XaiAdminServices {
            repository,
            oauth_config,
            oauth,
            pending: ports.oauth_pending(),
            refresh: Arc::clone(&refresh),
            quota: Arc::clone(&quota),
            catalog: Arc::clone(&catalog),
            runtime_policy,
        },
    ));
    let worker_contributions =
        provider::worker_contributions(refresh, quota, catalog, accounts, instances, provider_kind)
            .map_err(|_| XaiInitializeError::Worker)?;

    Ok(ProviderBundle {
        core_provider,
        admin_provider,
        worker_contributions,
    })
}

impl ProviderBundle {
    #[must_use]
    pub fn core_provider(&self) -> Arc<dyn Provider> {
        Arc::clone(&self.core_provider)
    }

    #[must_use]
    pub fn admin_provider(&self) -> Arc<dyn ProviderAdmin> {
        Arc::clone(&self.admin_provider)
    }

    /// 一次性移交 Host 任务计划，防止同一 owner 被重复注册。
    pub fn take_worker_contributions(&mut self) -> Vec<WorkerContribution> {
        std::mem::take(&mut self.worker_contributions)
    }
}

/// xAI 初始化失败的脱敏分类。
#[derive(Debug, thiserror::Error)]
pub enum XaiInitializeError {
    #[error(transparent)]
    Config(XaiConfigError),
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
