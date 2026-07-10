//! 运行时服务集合。

use std::sync::Arc;

use crate::{
    accounts::{
        cookies::PgCookieStore,
        manage::{AccountManageService, AccountManageServiceParts},
        pool::RuntimeAccountPoolService,
        pool::{AccountPoolOptions, RotationStrategy},
        refresh::{
            RedisRefreshLeaseStore, RefreshPolicy, RuntimeRefreshPolicy, RuntimeTokenRefreshService,
        },
        store::{AccountStore as AccountStoreTrait, PgAccountStore},
    },
    auth::{
        service::SessionService,
        store::{PgAdminUserStore, RedisAdminSessionStore},
    },
    bootstrap::config::AppConfig,
    dispatch::{
        affinity::{RedisSessionAffinityStore, RuntimeSessionAffinityService},
        responses::service::{ResponseDispatchService, ResponseDispatchServiceParts},
    },
    infra::redis::RedisConnection,
    keys::{manage::KeyManageService, service::KeyVerifier, store::PgClientKeyStore},
    models::{
        config::ModelConfig,
        service::{ModelService, ModelServiceError},
        store::{ModelSnapshotStore, RedisModelSnapshotStore},
    },
    settings::{service::RuntimeSettingsService, SettingsSnapshot},
    telemetry::{
        account_usage::query::AccountUsageQueryService,
        account_usage::store::PgAccountUsageStore,
        buckets::store::PgRequestBucketStore,
        ops::query::OpsQueryService,
        ops::store::PgOpsErrorLogStore,
        usage::query::UsageQueryService,
        usage::store::{PgUsageRecordStore, DEFAULT_USAGE_RECORD_CAPTURE_BODY},
    },
    upstream::openai::{
        fingerprint::{Fingerprint, PgFingerprintStore, RuntimeFingerprint},
        token_client::{default_openai_token_client, OpenAiTokenClient, TokenClientConfig},
        transport::{
            build_reqwest_client, tls::CustomCaError, CodexBackendClient, CodexModelCatalogClient,
            CodexWebSocketPool, CodexWebSocketPoolConfig,
        },
    },
};

/// 运行时热路径内存状态初始化错误。
#[derive(Debug, thiserror::Error)]
pub enum RuntimeServiceInitializationError {
    #[error("failed to initialize model catalog runtime cache: {0}")]
    Models(#[from] ModelServiceError),
}

// ============================================================================
// BackgroundTaskStores
// ============================================================================

/// 后台任务需要的具体存储适配器集合。
#[derive(Clone)]
pub struct BackgroundTaskStores {
    pub redis: RedisConnection,
    pub accounts: PgAccountStore,
    pub admin_users: PgAdminUserStore,
    pub admin_sessions: RedisAdminSessionStore,
    pub cookies: PgCookieStore,
    pub fingerprints: PgFingerprintStore,
    pub session_affinity: RedisSessionAffinityStore,
    pub refresh_leases: RedisRefreshLeaseStore,
    pub model_snapshots: RedisModelSnapshotStore,
    pub client_keys: PgClientKeyStore,
    pub usage_records: PgUsageRecordStore,
    pub ops_errors: PgOpsErrorLogStore,
    pub account_usage: PgAccountUsageStore,
    pub request_buckets: PgRequestBucketStore,
}

// ============================================================================
// Services struct
// ============================================================================

/// 运行时服务集合。
#[derive(Clone)]
pub struct Services {
    pub database: sqlx::PgPool,
    pub redis: RedisConnection,
    pub models: Arc<ModelService>,
    pub accounts: Arc<dyn AccountStoreTrait>,
    pub client_keys: Arc<KeyVerifier>,
    pub admin_client_keys: Arc<KeyManageService>,
    pub admin_sessions: Arc<SessionService>,
    pub settings: Arc<RuntimeSettingsService>,
    pub refresh_policy: RuntimeRefreshPolicy,
    pub admin_accounts: Arc<AccountManageService>,
    pub usage_records: Arc<UsageQueryService>,
    pub ops_errors: Arc<OpsQueryService>,
    pub usage: Arc<AccountUsageQueryService>,
    pub account_pool: Arc<RuntimeAccountPoolService>,
    pub token_refresh: Arc<RuntimeTokenRefreshService<OpenAiTokenClient>>,
    pub responses: Arc<ResponseDispatchService>,
    pub session_affinity: Arc<RuntimeSessionAffinityService>,
    pub codex: Arc<CodexBackendClient>,
    pub websocket_pool: Option<Arc<CodexWebSocketPool>>,
    pub fingerprint: RuntimeFingerprint,
    pub installation_id: Option<String>,
    pub background_tasks: BackgroundTaskStores,
    account_pool_static: AccountPoolStaticSettings,
}

#[derive(Debug, Clone)]
struct AccountPoolStaticSettings {
    skip_quota_limited: bool,
    tier_priority: Vec<String>,
}

/// 使用记录运行选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageRecordOptions {
    pub enabled: bool,
    pub capture_body: bool,
}

impl UsageRecordOptions {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            enabled: config.logging.enabled,
            capture_body: DEFAULT_USAGE_RECORD_CAPTURE_BODY,
        }
    }
}

impl Services {
    pub fn new(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        fingerprint: RuntimeFingerprint,
    ) -> Self {
        Self::try_new(config, stores, fingerprint)
            .expect("failed to build runtime services with configured TLS transport")
    }

    pub fn try_new(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        fingerprint: RuntimeFingerprint,
    ) -> Result<Self, CustomCaError> {
        Self::try_with_installation_id(config, stores, fingerprint, None)
    }

    pub(crate) fn try_with_installation_id(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        fingerprint: RuntimeFingerprint,
        installation_id: Option<String>,
    ) -> Result<Self, CustomCaError> {
        Self::try_with_installation_id_and_usage_record_options(
            config,
            stores,
            fingerprint,
            installation_id,
            UsageRecordOptions::from_config(config),
        )
    }

    pub fn try_with_installation_id_and_usage_record_options(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        fingerprint: RuntimeFingerprint,
        installation_id: Option<String>,
        usage_record_options: UsageRecordOptions,
    ) -> Result<Self, CustomCaError> {
        let installation_id = installation_id.filter(|id| !id.trim().is_empty());
        let database = stores.accounts.pool().clone();
        let redis = stores.redis.clone();
        let account_store_trait = Arc::new(stores.accounts.clone()) as Arc<dyn AccountStoreTrait>;
        let websocket_pool = config.ws_pool.enabled.then(|| {
            Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
                enabled: true,
                max_age: std::time::Duration::from_millis(config.ws_pool.max_age_ms),
                max_per_account: config.ws_pool.max_per_account,
                first_token_timeout: (config.ws_pool.first_token_timeout_ms > 0).then(|| {
                    std::time::Duration::from_millis(config.ws_pool.first_token_timeout_ms)
                }),
                ..CodexWebSocketPoolConfig::default()
            }))
        });
        let codex = {
            let websocket_first_token_timeout = (config.ws_pool.first_token_timeout_ms > 0)
                .then(|| std::time::Duration::from_millis(config.ws_pool.first_token_timeout_ms));
            let client = CodexBackendClient::new(
                build_reqwest_client(config.tls.force_http11)?,
                config.api.base_url.clone(),
                fingerprint.clone(),
            )
            .with_websocket_first_token_timeout(websocket_first_token_timeout);
            if let Some(pool) = &websocket_pool {
                Arc::new(client.with_websocket_pool(pool.clone()))
            } else {
                Arc::new(client)
            }
        };
        let usage_records = Arc::new(UsageQueryService::new(
            stores.usage_records.clone(),
            usage_record_options.enabled,
            usage_record_options.capture_body,
        ));
        let ops_errors = Arc::new(OpsQueryService::new(
            stores.ops_errors.clone(),
            usage_record_options.capture_body,
        ));
        let account_pool_static = AccountPoolStaticSettings {
            skip_quota_limited: config.quota.skip_exhausted,
            tier_priority: config.auth.tier_priority.clone(),
        };
        let account_pool = Arc::new(RuntimeAccountPoolService::new(
            account_store_trait.clone(),
            account_pool_options_from_settings(
                &settings_snapshot_from_config(config),
                &account_pool_static,
            ),
            config.auth.request_interval_ms,
        ));
        let refresh_policy = RuntimeRefreshPolicy::new(RefreshPolicy {
            refresh_margin_seconds: config.auth.refresh_margin_seconds,
            refresh_concurrency: config.auth.refresh_concurrency,
        });
        let settings = Arc::new(RuntimeSettingsService::new(
            settings_snapshot_from_config(config),
            stores.accounts.pool().clone(),
        ));
        let admin_sessions = Arc::new(SessionService::new(
            stores.admin_users.clone(),
            stores.admin_sessions.clone(),
            config.admin.default_username.clone(),
            config.admin.session_ttl_minutes,
        ));
        let admin_client_keys = Arc::new(KeyManageService::new(stores.client_keys.clone()));
        let client_keys = Arc::new(KeyVerifier::new(stores.client_keys.clone()));
        let token_client = default_openai_token_client(token_client_config(config));
        let token_refresh = Arc::new(
            RuntimeTokenRefreshService::new(
                stores.accounts.clone(),
                refresh_policy.clone(),
                token_client.clone(),
            )
            .with_refresh_lease_store(stores.refresh_leases.clone()),
        );
        let upstream_client: Arc<dyn CodexModelCatalogClient> = codex.clone();
        let store: Arc<dyn ModelSnapshotStore> = Arc::new(stores.model_snapshots.clone());
        let models = Arc::new(ModelService::new(
            ModelConfig {
                model_aliases: config.model_aliases.clone(),
            },
            Some(store),
            Some(upstream_client),
        ));
        let admin_accounts = Arc::new(AccountManageService::new(AccountManageServiceParts {
            store: stores.accounts.clone(),
            cookies: stores.cookies.clone(),
            codex: codex.clone(),
            models: models.clone(),
            account_pool: account_pool.clone(),
            token_refresher: Arc::new(token_client),
            refresh_leases: stores.refresh_leases.clone(),
            session_affinity: stores.session_affinity.clone(),
            oauth: crate::accounts::manage::oauth::AccountOAuthService::new(
                reqwest::Client::new(),
                config.auth.oauth_client_id.clone(),
                config.auth.oauth_token_endpoint.clone(),
            ),
            refresh_policy: refresh_policy.clone(),
            installation_id: installation_id.clone(),
        }));
        let usage = Arc::new(AccountUsageQueryService::new(stores.account_usage.clone()));
        let session_affinity = Arc::new(RuntimeSessionAffinityService::new(
            stores.session_affinity.clone(),
        ));

        let cloudflare_recovery =
            crate::dispatch::cloudflare::CloudflareRecovery::new(stores.cookies.clone());
        let responses = Arc::new(ResponseDispatchService::new(ResponseDispatchServiceParts {
            account_pool: account_pool.clone(),
            models: models.clone(),
            codex: codex.clone(),
            session_affinity: session_affinity.clone(),
            usage_records: usage_records.clone(),
            ops_errors: ops_errors.clone(),
            installation_id: installation_id.clone(),
            cloudflare: cloudflare_recovery,
        }));

        Ok(Self {
            database,
            redis,
            models,
            accounts: account_store_trait,
            client_keys,
            admin_client_keys,
            admin_sessions,
            settings,
            refresh_policy,
            admin_accounts,
            usage_records,
            ops_errors,
            usage,
            account_pool,
            token_refresh,
            responses,
            session_affinity,
            codex,
            websocket_pool,
            fingerprint,
            installation_id,
            background_tasks: stores,
            account_pool_static,
        })
    }

    /// 初始化请求热路径依赖的内存状态。
    pub async fn initialize_hot_path_state(&self) -> Result<(), RuntimeServiceInitializationError> {
        self.models.reload_from_store().await?;
        Ok(())
    }

    /// 将已持久化的设置快照同步到各运行时消费者。
    pub async fn apply_settings(&self, settings: &SettingsSnapshot) {
        self.models.update_config(ModelConfig {
            model_aliases: settings.model_aliases.clone(),
        });
        self.account_pool
            .apply_options(
                account_pool_options_from_settings(settings, &self.account_pool_static),
                settings.request_interval_ms,
            )
            .await;
        self.refresh_policy.update(RefreshPolicy {
            refresh_margin_seconds: settings.refresh_margin_seconds,
            refresh_concurrency: settings.refresh_concurrency,
        });
    }
}

fn token_client_config(config: &AppConfig) -> TokenClientConfig {
    TokenClientConfig {
        client_id: config.auth.oauth_client_id.clone(),
        token_endpoint: config.auth.oauth_token_endpoint.clone(),
    }
}

pub fn settings_snapshot_from_config(config: &AppConfig) -> SettingsSnapshot {
    SettingsSnapshot {
        model_aliases: config.model_aliases.clone(),
        refresh_margin_seconds: config.auth.refresh_margin_seconds,
        refresh_concurrency: config.auth.refresh_concurrency,
        max_concurrent_per_account: config.auth.max_concurrent_per_account,
        request_interval_ms: config.auth.request_interval_ms,
        rotation_strategy: config.auth.rotation_strategy.clone(),
    }
}

pub fn apply_settings_to_config(config: &mut AppConfig, settings: &SettingsSnapshot) {
    config.model_aliases = settings.model_aliases.clone();
    config.auth.refresh_margin_seconds = settings.refresh_margin_seconds;
    config.auth.refresh_concurrency = settings.refresh_concurrency;
    config.auth.max_concurrent_per_account = settings.max_concurrent_per_account;
    config.auth.request_interval_ms = settings.request_interval_ms;
    config.auth.rotation_strategy = settings.rotation_strategy.clone();
}

pub fn fingerprint_from_config(
    config: &crate::bootstrap::config::FingerprintConfig,
) -> Fingerprint {
    Fingerprint {
        originator: config.originator.clone(),
        app_version: config.app_version.clone(),
        build_number: config.build_number.clone(),
        platform: config.platform.clone(),
        arch: config.arch.clone(),
        chromium_version: config.chromium_version.clone(),
        user_agent_template: config.user_agent_template.clone(),
        default_headers: config
            .default_headers
            .iter()
            .map(|header| (header.name.clone(), header.value.clone()))
            .collect(),
        header_order: config.header_order.clone(),
        updated_at: None,
    }
}

fn account_pool_options_from_settings(
    settings: &SettingsSnapshot,
    static_settings: &AccountPoolStaticSettings,
) -> AccountPoolOptions {
    AccountPoolOptions {
        max_concurrent_per_account: settings.max_concurrent_per_account,
        rotation_strategy: match settings.rotation_strategy.as_str() {
            "smart" => RotationStrategy::Smart,
            "quota_reset_priority" => RotationStrategy::QuotaResetPriority,
            "round_robin" => RotationStrategy::RoundRobin,
            "sticky" => RotationStrategy::Sticky,
            _ => RotationStrategy::Smart,
        },
        skip_quota_limited: static_settings.skip_quota_limited,
        tier_priority: static_settings.tier_priority.clone(),
        ..AccountPoolOptions::default()
    }
}

pub fn account_pool_options_from_config(config: &AppConfig) -> AccountPoolOptions {
    account_pool_options_from_settings(
        &settings_snapshot_from_config(config),
        &AccountPoolStaticSettings {
            skip_quota_limited: config.quota.skip_exhausted,
            tier_priority: config.auth.tier_priority.clone(),
        },
    )
}
