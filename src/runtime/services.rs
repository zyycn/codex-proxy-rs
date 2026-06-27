//! 运行时服务集合。

use std::sync::Arc as StdArc;

use crate::{
    admin::monitoring::{
        event_store::{
            AdminLogService, SqliteEventLogStore, DEFAULT_EVENT_LOG_CAPACITY,
            DEFAULT_EVENT_LOG_CAPTURE_BODY, DEFAULT_EVENT_LOG_ENABLED,
        },
        service::AdminUsageService,
        usage_store::SqliteUsageStore,
    },
    admin::{
        accounts::service::AdminAccountService,
        auth::service::{AdminSessionService, SqliteAdminSessionStore},
        keys::service::{AdminClientKeyService, ClientKeyService, SqliteClientKeyStore},
    },
    config::{
        settings::{account_pool_options_from_config, RuntimeSettingsService},
        types::AppConfig,
    },
    proxy::dispatch::{
        chat::ChatDispatchService,
        responses::ResponseDispatchService,
        session_affinity::{RuntimeSessionAffinityService, SqliteSessionAffinityStore},
    },
    upstream::accounts::{
        cookies::SqliteCookieStore,
        pool::RuntimeAccountPoolService,
        store::{AccountStore as AccountStoreTrait, SqliteAccountStore},
        token_refresh::RefreshLeaseStore as SqliteRefreshLeaseStore,
    },
    upstream::{
        fingerprint::{Fingerprint, FingerprintRepository},
        models::{AdminModelService, ModelService, ModelSnapshotStore},
        token_client::{default_openai_token_client, TokenClientConfig},
        transport::{
            build_reqwest_client, CodexBackendClient, CodexModelCatalogClient, CustomCaError,
        },
    },
};

// ============================================================================
// BackgroundTaskStores
// ============================================================================

/// 后台任务需要的具体存储适配器集合。
#[derive(Clone)]
pub struct BackgroundTaskStores {
    pub accounts: SqliteAccountStore,
    pub admin_sessions: SqliteAdminSessionStore,
    pub cookies: SqliteCookieStore,
    pub fingerprints: FingerprintRepository,
    pub session_affinity: SqliteSessionAffinityStore,
    pub refresh_leases: SqliteRefreshLeaseStore,
    pub client_keys: SqliteClientKeyStore,
    pub event_logs: SqliteEventLogStore,
}

// ============================================================================
// Services struct
// ============================================================================

/// 运行时服务集合。
#[derive(Clone)]
pub struct Services {
    pub models: StdArc<ModelService>,
    pub admin_models: StdArc<AdminModelService>,
    pub accounts: StdArc<dyn AccountStoreTrait>,
    pub client_keys: StdArc<ClientKeyService>,
    pub admin_client_keys: StdArc<AdminClientKeyService>,
    pub admin_sessions: StdArc<AdminSessionService>,
    pub settings: StdArc<RuntimeSettingsService>,
    pub admin_accounts: StdArc<AdminAccountService>,
    pub logs: StdArc<AdminLogService>,
    pub usage: StdArc<AdminUsageService>,
    pub account_pool: StdArc<RuntimeAccountPoolService>,
    pub chat: StdArc<ChatDispatchService>,
    pub responses: StdArc<ResponseDispatchService>,
    pub session_affinity: StdArc<RuntimeSessionAffinityService>,
    pub codex: StdArc<CodexBackendClient>,
    pub fingerprint: Fingerprint,
    pub installation_id: Option<String>,
    pub background_tasks: BackgroundTaskStores,
}

impl Services {
    pub fn new(config: &AppConfig, stores: BackgroundTaskStores, fingerprint: Fingerprint) -> Self {
        Self::try_new(config, stores, fingerprint)
            .expect("failed to build runtime services with configured TLS transport")
    }

    pub fn try_new(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        fingerprint: Fingerprint,
    ) -> Result<Self, CustomCaError> {
        Self::try_with_installation_id(config, stores, fingerprint, None)
    }

    pub fn with_installation_id(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        fingerprint: Fingerprint,
        installation_id: Option<String>,
    ) -> Self {
        Self::try_with_installation_id(config, stores, fingerprint, installation_id)
            .expect("failed to build runtime services with configured TLS transport")
    }

    pub fn try_with_installation_id(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        fingerprint: Fingerprint,
        installation_id: Option<String>,
    ) -> Result<Self, CustomCaError> {
        let installation_id = installation_id.filter(|id| !id.trim().is_empty());
        let account_store_trait =
            StdArc::new(stores.accounts.clone()) as StdArc<dyn AccountStoreTrait>;
        let codex = {
            let client = CodexBackendClient::new(
                build_reqwest_client(config.tls.force_http11)?,
                config.api.base_url.clone(),
                fingerprint.clone(),
            );
            if config.ws_pool.enabled {
                let pool =
                    StdArc::new(crate::upstream::transport::CodexWebSocketPool::with_config(
                        crate::upstream::transport::CodexWebSocketPoolConfig {
                            enabled: config.ws_pool.enabled,
                            max_age: std::time::Duration::from_millis(config.ws_pool.max_age_ms),
                            max_per_account: config.ws_pool.max_per_account,
                            ..crate::upstream::transport::CodexWebSocketPoolConfig::default()
                        },
                    ));
                StdArc::new(client.with_websocket_pool(pool))
            } else {
                StdArc::new(client)
            }
        };
        let account_pool = StdArc::new(RuntimeAccountPoolService::new(
            account_store_trait.clone(),
            account_pool_options_from_config(config),
            config.auth.request_interval_ms,
        ));
        let settings = StdArc::new(RuntimeSettingsService::with_account_pool(
            config.clone(),
            stores.accounts.pool().clone(),
            account_pool.clone(),
        ));
        let admin_sessions = StdArc::new(AdminSessionService::new(
            stores.admin_sessions.clone(),
            config.admin.default_username.clone(),
            config.admin.session_ttl_minutes,
        ));
        let admin_client_keys = StdArc::new(AdminClientKeyService::new(stores.client_keys.clone()));
        let client_keys = StdArc::new(ClientKeyService::new(StdArc::new(
            stores.client_keys.clone(),
        )));
        let token_client = StdArc::new(default_openai_token_client(token_client_config(config)));
        let admin_accounts = StdArc::new(AdminAccountService::new(
            stores.accounts.clone(),
            stores.cookies.clone(),
            config.quota.warning_thresholds.clone(),
            codex.clone(),
            account_pool.clone(),
            token_client,
            crate::admin::accounts::service::oauth::AccountOAuthService::new(
                reqwest::Client::new(),
                config.auth.oauth_client_id.clone(),
                config.auth.oauth_token_endpoint.clone(),
            ),
            config.auth.refresh_margin_seconds,
            installation_id.clone(),
        ));
        let logs = StdArc::new(AdminLogService::new(
            stores.event_logs.clone(),
            DEFAULT_EVENT_LOG_ENABLED,
            DEFAULT_EVENT_LOG_CAPACITY,
            DEFAULT_EVENT_LOG_CAPTURE_BODY,
        ));
        let usage_store = SqliteUsageStore::new(stores.accounts.pool().clone());
        let usage = StdArc::new(AdminUsageService::new(usage_store));
        let session_affinity = StdArc::new(RuntimeSessionAffinityService::new(
            stores.session_affinity.clone(),
        ));

        let upstream_client: StdArc<dyn CodexModelCatalogClient> = codex.clone();
        let snapshot_store: StdArc<dyn ModelSnapshotStore> = StdArc::new(
            crate::upstream::models::SqliteModelSnapshotStore::new(stores.accounts.pool().clone()),
        );
        let models = StdArc::new(ModelService::new(
            crate::upstream::models::ModelConfig {
                model_aliases: config.model_aliases.clone(),
            },
            Some(snapshot_store.clone()),
            Some(upstream_client.clone()),
            None,
        ));
        let admin_models = StdArc::new(AdminModelService::new(
            StdArc::new(ModelService::new(
                crate::upstream::models::ModelConfig {
                    model_aliases: config.model_aliases.clone(),
                },
                Some(snapshot_store),
                Some(upstream_client),
                None,
            )),
            account_store_trait.clone(),
            installation_id.clone(),
        ));
        let cloudflare_recovery =
            crate::proxy::dispatch::cloudflare::CloudflareRecovery::new(stores.cookies.clone());
        let chat = StdArc::new(ChatDispatchService::new(
            account_pool.clone(),
            models.clone(),
            codex.clone(),
            logs.clone(),
            installation_id.clone(),
            cloudflare_recovery.clone(),
        ));
        let responses = StdArc::new(ResponseDispatchService::new(
            account_pool.clone(),
            models.clone(),
            codex.clone(),
            session_affinity.clone(),
            logs.clone(),
            installation_id.clone(),
            cloudflare_recovery,
        ));

        Ok(Self {
            models,
            admin_models,
            accounts: account_store_trait,
            client_keys,
            admin_client_keys,
            admin_sessions,
            settings,
            admin_accounts,
            logs,
            usage,
            account_pool,
            chat,
            responses,
            session_affinity,
            codex,
            fingerprint,
            installation_id,
            background_tasks: stores,
        })
    }
}

fn token_client_config(config: &AppConfig) -> TokenClientConfig {
    TokenClientConfig {
        client_id: config.auth.oauth_client_id.clone(),
        token_endpoint: config.auth.oauth_token_endpoint.clone(),
    }
}
