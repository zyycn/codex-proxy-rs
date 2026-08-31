//! OpenAI Provider 专属能力。

mod admin;
pub mod config;
mod provider;
mod session_transport;

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
    CodexCookiePolicy, CodexCredentialAdmin, CodexCredentialAdminService,
    CodexCredentialCatalogService, CodexCredentialProfileService, CodexCredentialQuotaService,
    CodexCredentialRefreshService, CodexCredentialRepository, CodexCredentialSelector,
    CodexOAuthAdmin, CodexOAuthAdminService,
};
use crate::transport::profile::{
    CodexArtifactProfileCache, CodexDesktopReleaseService, OfficialCodexDesktopReleaseTransport,
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
    CodexCanonicalDecoder, CodexCanonicalError, CodexRequestEncodeError, OpenAiBillingUsage,
    encode_generate_request, openai_billing_breakdown,
};

/// OpenAI 初始化后交给组装根的最小能力集。
pub struct ProviderBundle {
    core_provider: Arc<dyn Provider>,
    admin_provider: Arc<dyn ProviderAdmin>,
    worker_contributions: Vec<WorkerContribution>,
}

/// 构造 OpenAI 数据面、Provider-owned 后台任务与 Redis OAuth pending owner。
pub async fn initialize(
    config: OpenAiConfig,
    ports: ProviderStorePorts,
) -> Result<ProviderBundle, OpenAiInitializeError> {
    let provider_kind =
        ProviderKind::new("openai").map_err(|_| OpenAiInitializeError::InvalidProviderKind)?;
    let accounts: Arc<dyn ProviderAccountStore> = ports.accounts();
    let leases = ports.leases();
    let session_affinity = ports.session_affinity();
    let session_exclusions = ports.session_exclusions();
    let account_feedback = ports.account_feedback();
    let runtime_policy = ports.runtime_policy();
    let credential_state = ports.credential_state();
    let profile = config.wire_profile_state();
    let artifact_cache =
        CodexArtifactProfileCache::new(provider_kind.clone(), ports.artifact_profiles());
    let configured_build = profile.snapshot().desktop_build.parse::<u64>().ok();
    let verified_profile = match artifact_cache.load().await {
        Ok(cached) => cached.filter(|cached| {
            cached
                .desktop_build
                .parse::<u64>()
                .ok()
                .zip(configured_build)
                .is_some_and(|(cached, configured)| cached >= configured)
        }),
        Err(error) => {
            tracing::warn!(error = %error, "OpenAI artifact profile cache could not be loaded");
            None
        }
    };
    let session_identity = config
        .session_identity()
        .map_err(|_| OpenAiInitializeError::SessionIdentity)?;
    let http = build_reqwest_client().map_err(|_| OpenAiInitializeError::Transport)?;
    let desktop_release = Arc::new(CodexDesktopReleaseService::new(
        profile.clone(),
        Arc::new(
            OfficialCodexDesktopReleaseTransport::new()
                .map_err(|_| OpenAiInitializeError::DesktopRelease)?,
        ),
        artifact_cache,
        verified_profile,
    ));
    let desktop_release_status = desktop_release.status();
    let repository = CodexCredentialRepository::new(Arc::clone(&accounts));
    let websocket_pool = Arc::new(CodexWebSocketPool::with_config(
        config.websocket_pool_config(),
    ));
    let catalog = Arc::new(CodexCredentialCatalogService::new(
        repository.clone(),
        profile.clone(),
        http.clone(),
        config.base_url().to_owned(),
        ports.catalog_cache(),
    ));
    let quota = Arc::new(CodexCredentialQuotaService::new(
        repository.clone(),
        profile.clone(),
        http.clone(),
        config.base_url().to_owned(),
        ports.cooldowns(),
    ));
    let profile_statistics = Arc::new(CodexCredentialProfileService::new(
        repository.clone(),
        profile.clone(),
        http.clone(),
        config.base_url().to_owned(),
    ));
    let selector = Arc::new(CodexCredentialSelector::new(
        provider_kind.clone(),
        repository.clone(),
        Arc::clone(&leases),
        session_affinity,
        session_exclusions,
        Arc::clone(&catalog),
        Arc::clone(&quota),
        Arc::clone(&account_feedback),
        CodexCookiePolicy::official().map_err(|_| OpenAiInitializeError::CookiePolicy)?,
    ));
    let core_provider: Arc<dyn Provider> = Arc::new(
        CodexProvider::new(
            selector,
            Arc::clone(&catalog),
            Arc::clone(&quota),
            account_feedback,
            http,
            profile.clone(),
            config.base_url().to_owned(),
            Arc::clone(&websocket_pool),
            config.stream_max_retries(),
        )
        .map_err(OpenAiInitializeError::Provider)?
        .with_session_identity(session_identity),
    );
    let token_client = Arc::new(
        credential::token_client::openai_token_client(config.token_client_config())
            .map_err(|_| OpenAiInitializeError::TokenClient)?,
    );
    let refresher: Arc<dyn TokenRefresher> = token_client.clone();
    let exchanger: Arc<dyn AuthorizationCodeExchanger> = token_client;
    let credential_admin = Arc::new(CodexCredentialAdminService::new(
        Arc::clone(&refresher),
        Arc::clone(&leases),
        Arc::clone(&runtime_policy),
    ));
    let refresh = Arc::new(CodexCredentialRefreshService::new(
        repository,
        refresher,
        Arc::clone(&leases),
        credential_state,
        Arc::clone(&runtime_policy),
    ));
    let pending = Arc::new(OpenAiOAuthPendingStore::new(
        ports.oauth_pending(),
        provider_kind.clone(),
    ));
    let oauth_admin: Arc<dyn CodexOAuthAdmin> = Arc::new(
        CodexOAuthAdminService::new(
            pending,
            exchanger,
            Arc::clone(&accounts),
            CodexCredentialAdmin,
            profile.clone(),
        )
        .with_oauth_client_id(config.oauth_client_id()),
    );
    let admin_provider: Arc<dyn ProviderAdmin> = Arc::new(OpenAiAdminProvider::new(
        provider_kind,
        profile,
        accounts,
        OpenAiAdminServices {
            credentials: credential_admin,
            oauth: oauth_admin,
            profile_statistics,
            quota: Arc::clone(&quota),
            catalog: Arc::clone(&catalog),
        },
        websocket_pool,
        desktop_release_status,
    ));
    let worker_contributions = provider::worker_contributions(
        refresh,
        quota,
        catalog,
        config.quota_refresh_policy(),
        config.oauth_refresh_enabled(),
        desktop_release,
    )
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
    #[error("OpenAI local session identity is unavailable")]
    SessionIdentity,
    #[error("OpenAI transport could not initialize")]
    Transport,
    #[error(transparent)]
    Provider(CodexProviderConfigError),
    #[error("OpenAI cookie policy could not initialize")]
    CookiePolicy,
    #[error("OpenAI token client could not initialize")]
    TokenClient,
    #[error("OpenAI credential administration could not initialize")]
    CredentialAdmin,
    #[error("OpenAI credential refresh could not initialize")]
    Refresh,
    #[error("OpenAI Desktop release service could not initialize")]
    DesktopRelease,
    #[error("OpenAI worker plan is invalid")]
    Worker,
}
