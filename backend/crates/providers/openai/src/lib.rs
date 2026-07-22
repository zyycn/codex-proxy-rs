//! OpenAI Provider 专属能力。

mod admin;
pub mod config;
mod provider;

use std::path::Path;
use std::sync::Arc;

use gateway_admin::ports::provider::ProviderAdmin;
use gateway_core::engine::credential::ProviderAccountStore;
use gateway_core::engine::provider::Provider;
use gateway_core::provider_ports::ProviderStorePorts;
use gateway_core::routing::ProviderKind;
use gateway_core::task::WorkerContribution;

use crate::admin::{OpenAiAdminProvider, OpenAiAdminServices, OpenAiOAuthPendingStore};
use crate::credential::token_client::{AuthorizationCodeExchanger, TokenRefresher};
use crate::credential::{
    CodexAccountIdentityService, CodexAccountIdentityVerifier, CodexAuthenticatedAccountSource,
    CodexCookiePolicy, CodexCredentialAdmin, CodexCredentialAdminService,
    CodexCredentialCatalogService, CodexCredentialQuotaService, CodexCredentialRefreshService,
    CodexCredentialRepository, CodexCredentialSelector, CodexJwtIdentityVerifier, CodexOAuthAdmin,
    CodexOAuthAdminService, CodexSignedIdentityVerifier, ReqwestCodexAuthenticatedAccountSource,
    ReqwestOpenAiJwksSource,
};
use crate::transport::profile::{
    CodexCliReleaseService, CodexDesktopReleaseService, OfficialCodexCliReleaseTransport,
    OfficialCodexDesktopReleaseTransport,
};
use crate::transport::{CodexWebSocketPool, build_reqwest_client};

pub use config::{CodexWireProfileConfig, OpenAiConfig, OpenAiConfigError};
pub use provider::{
    CodexProvider, CodexProviderConfigError, CodexProviderTransport, OFFICIAL_CODEX_BASE_PATH,
    OFFICIAL_CODEX_BASE_URL,
};

pub mod credential;
pub mod transport;

pub use transport::{
    CodexCanonicalDecoder, CodexCanonicalError, CodexRequestEncodeError, encode_generate_request,
    openai_billing_breakdown,
};

/// OpenAI 初始化后交给组装根的最小能力集。
pub struct ProviderBundle {
    core_provider: Arc<dyn Provider>,
    admin_provider: Arc<dyn ProviderAdmin>,
    worker_contributions: Vec<WorkerContribution>,
}

/// 构造 OpenAI 数据面、Provider-owned 后台任务与 Redis OAuth pending owner。
pub async fn initialize(
    mut config: OpenAiConfig,
    ports: ProviderStorePorts,
) -> Result<ProviderBundle, OpenAiInitializeError> {
    config
        .resolve_and_validate(Path::new("."))
        .map_err(OpenAiInitializeError::Config)?;
    let provider_kind =
        ProviderKind::new("openai").map_err(|_| OpenAiInitializeError::InvalidProviderKind)?;
    let accounts: Arc<dyn ProviderAccountStore> = ports.accounts();
    let leases = ports.leases();
    let runtime_policy = ports.runtime_policy();
    let profile = config.wire_profile_state();
    let http = build_reqwest_client().map_err(|_| OpenAiInitializeError::Transport)?;
    let desktop_release = Arc::new(CodexDesktopReleaseService::new(
        profile.clone(),
        Arc::new(
            OfficialCodexDesktopReleaseTransport::new()
                .map_err(|_| OpenAiInitializeError::DesktopRelease)?,
        ),
    ));
    let desktop_release_status = desktop_release.status();
    let cli_release = Arc::new(CodexCliReleaseService::new(
        profile.clone(),
        Arc::new(
            OfficialCodexCliReleaseTransport::new()
                .map_err(|_| OpenAiInitializeError::DesktopRelease)?,
        ),
    ));
    let repository = CodexCredentialRepository::new(Arc::clone(&accounts));
    let catalog = Arc::new(CodexCredentialCatalogService::new(
        repository.clone(),
        profile.clone(),
        http.clone(),
    ));
    let quota = Arc::new(CodexCredentialQuotaService::new(
        repository.clone(),
        profile.clone(),
        http.clone(),
    ));
    let selector = Arc::new(CodexCredentialSelector::new(
        provider_kind.clone(),
        repository.clone(),
        Arc::clone(&leases),
        Arc::clone(&catalog),
        Arc::clone(&quota),
        CodexCookiePolicy::official().map_err(|_| OpenAiInitializeError::CookiePolicy)?,
    ));
    let websocket_pool = Arc::new(CodexWebSocketPool::default());
    let core_provider: Arc<dyn Provider> = Arc::new(
        CodexProvider::new(
            selector,
            Arc::clone(&catalog),
            Arc::clone(&quota),
            http,
            profile.clone(),
            Arc::clone(&websocket_pool),
        )
        .map_err(OpenAiInitializeError::Provider)?,
    );

    let token_client = Arc::new(
        credential::token_client::official_openai_token_client()
            .map_err(|_| OpenAiInitializeError::TokenClient)?,
    );
    let jwks = ReqwestOpenAiJwksSource::new().map_err(|_| OpenAiInitializeError::Identity)?;
    let signed: Arc<dyn CodexSignedIdentityVerifier> =
        Arc::new(CodexJwtIdentityVerifier::new(Box::new(jwks)));
    let account_source: Arc<dyn CodexAuthenticatedAccountSource> = Arc::new(
        ReqwestCodexAuthenticatedAccountSource::new(profile.clone())
            .map_err(|_| OpenAiInitializeError::Identity)?,
    );
    let identity: Arc<dyn CodexAccountIdentityVerifier> =
        Arc::new(CodexAccountIdentityService::new(signed, account_source));
    let refresher: Arc<dyn TokenRefresher> = token_client.clone();
    let exchanger: Arc<dyn AuthorizationCodeExchanger> = token_client;
    let credential_admin = Arc::new(CodexCredentialAdminService::new(
        repository.clone(),
        Arc::clone(&refresher),
        Arc::clone(&identity),
        Arc::clone(&leases),
        Arc::clone(&runtime_policy),
    ));
    let refresh = Arc::new(CodexCredentialRefreshService::new(
        repository,
        refresher,
        Arc::clone(&identity),
        Arc::clone(&leases),
        Arc::clone(&runtime_policy),
    ));
    let pending = Arc::new(OpenAiOAuthPendingStore::new(
        ports.oauth_pending(),
        provider_kind.clone(),
    ));
    let oauth_admin: Arc<dyn CodexOAuthAdmin> = Arc::new(CodexOAuthAdminService::new(
        pending,
        exchanger,
        Arc::clone(&identity),
        Arc::clone(&accounts),
        Arc::clone(&runtime_policy),
        CodexCredentialAdmin,
    ));
    let admin_provider: Arc<dyn ProviderAdmin> = Arc::new(OpenAiAdminProvider::new(
        provider_kind,
        profile,
        accounts,
        OpenAiAdminServices {
            credentials: credential_admin,
            verifier: identity,
            oauth: oauth_admin,
            quota: Arc::clone(&quota),
            catalog: Arc::clone(&catalog),
            runtime_policy,
        },
        websocket_pool,
        desktop_release_status,
    ));
    let worker_contributions =
        provider::worker_contributions(refresh, quota, catalog, cli_release, desktop_release)
            .map_err(|_| OpenAiInitializeError::Worker)?;

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

/// OpenAI 初始化失败的脱敏分类。
#[derive(Debug, thiserror::Error)]
pub enum OpenAiInitializeError {
    #[error(transparent)]
    Config(OpenAiConfigError),
    #[error("OpenAI runtime policy is unavailable")]
    RuntimePolicy,
    #[error("OpenAI Provider kind is invalid")]
    InvalidProviderKind,
    #[error("OpenAI transport could not initialize")]
    Transport,
    #[error(transparent)]
    Provider(CodexProviderConfigError),
    #[error("OpenAI cookie policy could not initialize")]
    CookiePolicy,
    #[error("OpenAI token client could not initialize")]
    TokenClient,
    #[error("OpenAI identity verifier could not initialize")]
    Identity,
    #[error("OpenAI credential administration could not initialize")]
    CredentialAdmin,
    #[error("OpenAI credential refresh could not initialize")]
    Refresh,
    #[error("OpenAI Desktop release service could not initialize")]
    DesktopRelease,
    #[error("OpenAI worker plan is invalid")]
    Worker,
}
