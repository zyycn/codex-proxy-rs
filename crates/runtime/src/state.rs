//! 运行时状态。

use std::{path::PathBuf, sync::Arc};

use chrono::{DateTime, Utc};
use codex_proxy_core::{
    auth::ports::{OAuthClient, TokenRefresher},
    gateway::fingerprint::Fingerprint,
};
use codex_proxy_platform::{
    config::AppConfig, crypto::SecretBox, identity::ApiKeyHasher, storage::SqlitePool,
};

use crate::{
    config::RuntimeConfig,
    repositories::sqlite_repositories,
    services::{RuntimeAccountPoolError, RuntimeSessionAffinityError, Services},
};

/// 应用状态。
#[derive(Clone)]
pub struct AppState {
    /// 运行时配置。
    pub config: RuntimeConfig,
    /// 运行时服务。
    pub services: Services,
}

struct AppStateServiceOptions {
    fingerprint: Fingerprint,
    installation_id: Option<String>,
    local_config_path: PathBuf,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
    oauth_client: Option<Arc<dyn OAuthClient>>,
}

impl AppState {
    /// 从连接池和密钥箱构造状态。
    pub fn with_pool_and_secret_box(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
    ) -> Self {
        Self::with_pool_secret_api_key_hasher_and_fingerprint(
            config,
            pool,
            secret_box,
            ApiKeyHasher::new([0u8; 32]),
            Fingerprint::default_codex_desktop(),
        )
    }

    /// 从连接池、密钥箱和 API key hasher 构造状态。
    pub fn with_pool_secret_and_api_key_hasher(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        hasher: ApiKeyHasher,
    ) -> Self {
        Self::with_pool_secret_api_key_hasher_and_fingerprint(
            config,
            pool,
            secret_box,
            hasher,
            Fingerprint::default_codex_desktop(),
        )
    }

    /// 从连接池、密钥箱、API key hasher 和 installation id 构造状态。
    pub fn with_pool_secret_api_key_hasher_and_installation_id(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        hasher: ApiKeyHasher,
        installation_id: String,
    ) -> Self {
        Self::with_pool_secret_api_key_hasher_fingerprint_and_installation_id(
            config,
            pool,
            secret_box,
            hasher,
            Fingerprint::default_codex_desktop(),
            Some(installation_id),
        )
    }

    /// 从连接池、密钥箱、API key hasher 和指纹构造状态。
    pub fn with_pool_secret_api_key_hasher_and_fingerprint(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        hasher: ApiKeyHasher,
        fingerprint: Fingerprint,
    ) -> Self {
        Self::with_pool_secret_api_key_hasher_fingerprint_and_installation_id(
            config,
            pool,
            secret_box,
            hasher,
            fingerprint,
            None,
        )
    }

    /// 从连接池、密钥箱、API key hasher、指纹和 installation id 构造状态。
    pub fn with_pool_secret_api_key_hasher_fingerprint_and_installation_id(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        hasher: ApiKeyHasher,
        fingerprint: Fingerprint,
        installation_id: Option<String>,
    ) -> Self {
        Self::with_pool_secret_api_key_hasher_fingerprint_installation_id_and_local_config_path(
            config,
            pool,
            secret_box,
            hasher,
            fingerprint,
            installation_id,
            "local.yaml",
        )
    }

    /// 从连接池、密钥箱、API key hasher 和本地配置路径构造状态。
    pub fn with_pool_secret_api_key_hasher_and_local_config_path(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        hasher: ApiKeyHasher,
        local_config_path: impl Into<PathBuf>,
    ) -> Self {
        Self::with_pool_secret_api_key_hasher_fingerprint_installation_id_and_local_config_path(
            config,
            pool,
            secret_box,
            hasher,
            Fingerprint::default_codex_desktop(),
            None,
            local_config_path,
        )
    }

    /// 从连接池、密钥箱、API key hasher、指纹、installation id 和本地配置路径构造状态。
    pub fn with_pool_secret_api_key_hasher_fingerprint_installation_id_and_local_config_path(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        hasher: ApiKeyHasher,
        fingerprint: Fingerprint,
        installation_id: Option<String>,
        local_config_path: impl Into<PathBuf>,
    ) -> Self {
        Self::with_pool_secret_api_key_hasher_and_options(
            config,
            pool,
            secret_box,
            hasher,
            AppStateServiceOptions {
                fingerprint,
                installation_id,
                local_config_path: local_config_path.into(),
                token_refresher: None,
                oauth_client: None,
            },
        )
    }

    /// 从连接池、密钥箱、API key hasher 和 token refresher 构造状态。
    pub fn with_pool_secret_api_key_hasher_and_token_refresher<C>(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        hasher: ApiKeyHasher,
        token_refresher: C,
    ) -> Self
    where
        C: TokenRefresher,
    {
        Self::with_pool_secret_api_key_hasher_and_options(
            config,
            pool,
            secret_box,
            hasher,
            AppStateServiceOptions {
                fingerprint: Fingerprint::default_codex_desktop(),
                installation_id: None,
                local_config_path: PathBuf::from("local.yaml"),
                token_refresher: Some(Arc::new(token_refresher)),
                oauth_client: None,
            },
        )
    }

    /// 从连接池、密钥箱、API key hasher 和 OAuth client 构造状态。
    pub fn with_pool_secret_api_key_hasher_and_oauth_client<C>(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        hasher: ApiKeyHasher,
        oauth_client: C,
    ) -> Self
    where
        C: OAuthClient + TokenRefresher + Clone,
    {
        let token_refresher: Arc<dyn TokenRefresher> = Arc::new(oauth_client.clone());
        let oauth_client: Arc<dyn OAuthClient> = Arc::new(oauth_client);
        Self::with_pool_secret_api_key_hasher_and_options(
            config,
            pool,
            secret_box,
            hasher,
            AppStateServiceOptions {
                fingerprint: Fingerprint::default_codex_desktop(),
                installation_id: None,
                local_config_path: PathBuf::from("local.yaml"),
                token_refresher: Some(token_refresher),
                oauth_client: Some(oauth_client),
            },
        )
    }

    fn with_pool_secret_api_key_hasher_and_options(
        config: AppConfig,
        pool: SqlitePool,
        secret_box: SecretBox,
        hasher: ApiKeyHasher,
        options: AppStateServiceOptions,
    ) -> Self {
        let AppStateServiceOptions {
            fingerprint,
            installation_id,
            local_config_path,
            token_refresher,
            oauth_client,
        } = options;
        let runtime_config: RuntimeConfig = config.clone().into();
        let repositories = sqlite_repositories(pool, secret_box, hasher);
        let services = match (token_refresher, oauth_client) {
            (Some(token_refresher), Some(oauth_client)) => {
                Services::with_installation_id_local_config_path_and_oauth_clients(
                    &config,
                    repositories,
                    fingerprint,
                    installation_id,
                    local_config_path,
                    token_refresher,
                    oauth_client,
                )
            }
            (Some(token_refresher), None) => {
                Services::with_installation_id_local_config_path_and_token_refresher(
                    &config,
                    repositories,
                    fingerprint,
                    installation_id,
                    local_config_path,
                    token_refresher,
                )
            }
            (None, Some(oauth_client)) => {
                let token_refresher: Arc<dyn TokenRefresher> = Arc::new(
                    codex_proxy_adapters::oauth::openai::default_openai_oauth_client(
                        crate::services::oauth_config(&config),
                    ),
                );
                Services::with_installation_id_local_config_path_and_oauth_clients(
                    &config,
                    repositories,
                    fingerprint,
                    installation_id,
                    local_config_path,
                    token_refresher,
                    oauth_client,
                )
            }
            (None, None) => Services::with_installation_id_and_local_config_path(
                &config,
                repositories,
                fingerprint,
                installation_id,
                local_config_path,
            ),
        };
        Self {
            config: runtime_config,
            services,
        }
    }

    /// 从持久化存储恢复未过期的会话亲和性映射。
    pub async fn restore_session_affinity_from_repository(
        &self,
        now: DateTime<Utc>,
    ) -> Result<usize, RuntimeSessionAffinityError> {
        self.services
            .session_affinity
            .restore_from_repository(now)
            .await
    }

    /// 使用当前时间从持久化存储恢复未过期的会话亲和性映射。
    pub async fn restore_session_affinity_from_repository_now(
        &self,
    ) -> Result<usize, RuntimeSessionAffinityError> {
        self.restore_session_affinity_from_repository(Utc::now())
            .await
    }

    /// 从持久化存储恢复运行时账号池。
    pub async fn restore_account_pool_from_repository(
        &self,
    ) -> Result<usize, RuntimeAccountPoolError> {
        self.services.account_pool.restore_from_repository().await
    }
}
