//! 服务组装。

use std::{
    collections::BTreeMap,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, RwLock as StdRwLock},
    task::{Context, Poll},
    time::{Duration as StdDuration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use bytes::Bytes;
use chrono::{DateTime, Duration, Utc};
use codex_proxy_adapters::{
    codex::client::{
        CodexBackendClient, CodexBackendResponse, CodexBackendSseStream,
        CodexBackendStreamingResponse, CodexBackendTransport, CodexClientError,
        CodexCompactResponse, CodexRateLimitHeaderUpdates, CodexRequestContext,
        CodexTurnStateUpdate,
    },
    codex::fingerprint::FingerprintRepository,
    oauth::openai::default_openai_oauth_client,
    sqlite::{
        accounts::{
            AccountClaimsUpdate, AccountQuotaSnapshot, AccountUsageListRecord, AccountUsageSummary,
            ImportedAccountUpdate, NewAccount, SqliteAccountStore, StoredAccount,
            StoredAccountMetadata,
        },
        admin_sessions::SqliteAdminSessionStore,
        client_keys::{CreatedClientApiKey, SqliteClientKeyStore, StoredClientApiKey},
        cookies::SqliteCookieStore,
        events::{EventLogFilter, SqliteEventLogStore},
        refresh_leases::SqliteRefreshLeaseStore,
        session_affinity::{SqliteSessionAffinityStore, SqliteSessionAffinityStoreError},
    },
};
use codex_proxy_core::{
    accounts::{
        cloudflare::{CloudflareChallengeCooldownTracker, CloudflarePathBlockTracker},
        jwt::{jwt_expiry, JwtExpiry},
        model::{Account, AccountStatus},
        pool::{
            AccountAcquireRequest, AccountCapacitySummary, AccountPool, AccountPoolOptions,
            AccountWindowUsageDelta, AcquiredAccount, RotationStrategy,
        },
        ports::{AccountStore, AccountStoreError},
    },
    admin::{
        auth::AdminAuthService,
        client_keys::ClientKeyService,
        settings::{
            AdminQuotaWarningThresholds, AdminSettings, AdminSettingsPatch, SettingsService,
            SettingsServiceError,
        },
    },
    auth::{
        oauth::{
            DeviceCode, OAuthConfig, OAuthError, PkceLogin, PkceSessionStore, RefreshFailure,
            TokenPair,
        },
        ports::{OAuthClient, TokenRefresher},
    },
    events::{
        model::{EventLevel, EventLog},
        service::EventLogService,
    },
    gateway::{
        conversation::{build_conversation_identity, ensure_prompt_cache_key},
        fingerprint::Fingerprint,
        ports::CodexModelCatalogClient,
    },
    models::{
        model::ModelConfig,
        ports::ModelSnapshotStore,
        service::{ModelRefreshResult, ModelService, ModelServiceError},
    },
    protocol::{
        codex::{
            events::{
                extract_sse_usage, extract_usage, parse_rate_limit_headers, rate_limit_quota,
                TokenUsage,
            },
            responses::{CodexCompactRequest, CodexResponsesRequest},
            sse::{
                encode_sse_event, parse_sse_events, sse_body_has_done, SseError, DONE_SSE_FRAME,
            },
        },
        openai::chat::chat_completion_from_codex_sse,
        openai::responses::{
            completed_response_metadata, reconvert_responses_sse_event_tuple_values,
            response_failed_sse_event, response_from_codex_sse, CollectedResponse,
            ResponsesSseFailure,
        },
    },
    serving::{
        affinity::{
            compute_variant_hash, hash_instructions, prepare_variant_identity,
            SessionAffinityEntry, SessionAffinityMap,
        },
        fallback::{
            status_code_is_quota_exhausted, status_code_is_rate_limited,
            status_code_is_transient_upstream,
        },
        implicit_resume::{
            continuation_input_start, implicit_resume_allowed, ImplicitResumeSnapshot,
        },
        quota::{
            quota_from_usage, quota_reached, quota_snapshot_limit_reached,
            quota_snapshot_limit_window_seconds, quota_snapshot_reset_at,
        },
        reasoning_replay::ReasoningReplayCache,
        recovery::status_code_allows_same_account_retry,
        responses::apply_response_model_options,
    },
    usage::service::UsageService,
};
use codex_proxy_platform::{
    config::{AppConfig, ConfigWriteError, QuotaWarningThresholds},
    identity::{hash_admin_password, verify_admin_password},
    json::Page,
};
use futures::{stream, Stream, StreamExt};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot, RwLock},
    time::sleep,
};

use crate::{repositories::Repositories, upstream};

mod account_pool;
mod admin_accounts;
mod admin_client_keys;
mod admin_logs;
mod admin_models;
mod admin_oauth;
mod admin_sessions;
mod admin_usage;
mod dispatch_chat;
mod dispatch_responses;
mod session_affinity;
mod settings;

use account_pool::{account_pool_options, CloudflareRecovery};

pub use account_pool::{RuntimeAccountPoolError, RuntimeAccountPoolService};
pub use admin_accounts::{
    AdminAccountError, AdminAccountMetadata, AdminAccountProbeOutcome, AdminAccountProbeResult,
    AdminAccountQuota, AdminAccountQuotaWarning, AdminAccountQuotaWarnings, AdminAccountRefresh,
    AdminAccountResetUsage, AdminAccountService, AdminAuthLogout, AdminAuthPoolStatus,
    AdminAuthStatus, AdminQuotaWarningLevel, AdminQuotaWarningWindow, AdminStoredAccount,
    BatchDeleteAccounts, BatchUpdateAccountStatus, ImportedAccounts, UpdatedAccountStatus,
};
pub use admin_client_keys::{
    AdminClientKeyError, AdminClientKeyService, AdminCreatedClientApiKey, AdminStoredClientApiKey,
    BatchDeleteClientApiKeys, ImportedClientApiKey, ImportedClientApiKeys,
    UpdatedClientApiKeyStatus,
};
pub use admin_logs::{
    AdminClearLogs, AdminLogError, AdminLogFilter, AdminLogService, AdminLogState,
    AdminLogStateUpdate,
};
pub use admin_models::{AdminModelError, AdminModelService};
pub use admin_oauth::{AdminDevicePoll, AdminOAuthCallback, AdminOAuthError, AdminOAuthService};
pub use admin_sessions::{AdminLoginSession, AdminSessionError, AdminSessionService};
pub use admin_usage::{AdminUsageError, AdminUsageRecord, AdminUsageService, AdminUsageSummary};
pub use dispatch_chat::{ChatDispatchError, ChatDispatchService};
pub use dispatch_responses::{
    ResponseBodyStream, ResponseDispatchError, ResponseDispatchService, ResponseDispatchStream,
    ResponseDispatchStreamError,
};
pub use session_affinity::{RuntimeSessionAffinityError, RuntimeSessionAffinityService};
pub use settings::{RuntimeSettingsError, RuntimeSettingsService};

/// 运行时服务集合。
#[derive(Clone)]
pub struct Services {
    /// 模型目录服务。
    pub models: Arc<ModelService>,
    /// 管理端模型服务。
    pub admin_models: Arc<AdminModelService>,
    /// 账号存储。
    pub accounts: Arc<dyn AccountStore>,
    /// 模型快照存储。
    pub model_snapshots: Arc<dyn ModelSnapshotStore>,
    /// 客户端 API Key 服务。
    pub client_keys: Arc<ClientKeyService>,
    /// 管理端客户端 API Key 服务。
    pub admin_client_keys: Arc<AdminClientKeyService>,
    /// 管理员会话服务。
    pub admin_sessions: Arc<AdminSessionService>,
    /// 管理端设置服务。
    pub settings: Arc<RuntimeSettingsService>,
    /// 管理端账号服务。
    pub admin_accounts: Arc<AdminAccountService>,
    /// 管理端 OAuth 服务。
    pub admin_oauth: Arc<AdminOAuthService>,
    /// 管理端日志服务。
    pub logs: Arc<AdminLogService>,
    /// 管理端用量服务。
    pub usage: Arc<AdminUsageService>,
    /// 运行时账号池服务。
    pub account_pool: Arc<RuntimeAccountPoolService>,
    /// OpenAI Chat Completions 调度服务。
    pub chat: Arc<ChatDispatchService>,
    /// OpenAI Responses 调度服务。
    pub responses: Arc<ResponseDispatchService>,
    /// 运行时会话亲和性服务。
    pub session_affinity: Arc<RuntimeSessionAffinityService>,
    /// Codex 上游客户端。
    pub codex: Arc<CodexBackendClient>,
    /// 当前运行时指纹。
    pub fingerprint: Fingerprint,
    /// 当前运行时 Codex installation id。
    pub installation_id: Option<String>,
    /// 后台任务需要的具体存储适配器。
    pub background_tasks: BackgroundTaskStores,
}

/// Codex upstream connectivity probe result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUpstreamProbe {
    /// Probed target name.
    pub target: &'static str,
    /// Configured backend base URL.
    pub backend_base_url: String,
    /// Full endpoint URL.
    pub endpoint: String,
    /// Whether the endpoint responded at transport level.
    pub reachable: bool,
    /// HTTP status code, when available.
    pub status_code: Option<u16>,
    /// Authentication outcome inferred from status.
    pub authorization: &'static str,
}

/// 后台任务需要的具体存储适配器集合。
#[derive(Clone)]
pub struct BackgroundTaskStores {
    /// 账号存储。
    pub accounts: SqliteAccountStore,
    /// 管理员会话存储。
    pub admin_sessions: SqliteAdminSessionStore,
    /// Cookie 存储。
    pub cookies: SqliteCookieStore,
    /// 指纹存储。
    pub fingerprints: FingerprintRepository,
    /// 会话亲和性存储。
    pub session_affinity: SqliteSessionAffinityStore,
    /// 账号刷新租约存储。
    pub refresh_leases: SqliteRefreshLeaseStore,
}

impl Services {
    /// 从仓储和上游适配器构造运行时服务集合。
    pub fn new(config: &AppConfig, repositories: Repositories, fingerprint: Fingerprint) -> Self {
        Self::with_installation_id(config, repositories, fingerprint, None)
    }

    /// 从仓储、上游适配器和 installation id 构造运行时服务集合。
    pub fn with_installation_id(
        config: &AppConfig,
        repositories: Repositories,
        fingerprint: Fingerprint,
        installation_id: Option<String>,
    ) -> Self {
        Self::with_installation_id_and_local_config_path(
            config,
            repositories,
            fingerprint,
            installation_id,
            "local.yaml",
        )
    }

    /// 从仓储、上游适配器、installation id 和本地配置路径构造运行时服务集合。
    pub fn with_installation_id_and_local_config_path(
        config: &AppConfig,
        repositories: Repositories,
        fingerprint: Fingerprint,
        installation_id: Option<String>,
        local_config_path: impl Into<PathBuf>,
    ) -> Self {
        let token_refresher: Arc<dyn TokenRefresher> =
            Arc::new(default_openai_oauth_client(oauth_config(config)));
        let oauth_client: Arc<dyn OAuthClient> =
            Arc::new(default_openai_oauth_client(oauth_config(config)));
        Self::with_installation_id_local_config_path_and_oauth_clients(
            config,
            repositories,
            fingerprint,
            installation_id,
            local_config_path,
            token_refresher,
            oauth_client,
        )
    }

    /// 从仓储、上游适配器、installation id、本地配置路径和 token refresher 构造服务集合。
    pub fn with_installation_id_local_config_path_and_token_refresher(
        config: &AppConfig,
        repositories: Repositories,
        fingerprint: Fingerprint,
        installation_id: Option<String>,
        local_config_path: impl Into<PathBuf>,
        token_refresher: Arc<dyn TokenRefresher>,
    ) -> Self {
        let oauth_client: Arc<dyn OAuthClient> =
            Arc::new(default_openai_oauth_client(oauth_config(config)));
        Self::with_installation_id_local_config_path_and_oauth_clients(
            config,
            repositories,
            fingerprint,
            installation_id,
            local_config_path,
            token_refresher,
            oauth_client,
        )
    }

    /// 从仓储、上游适配器、installation id、本地配置路径和 OAuth 端口构造服务集合。
    pub fn with_installation_id_local_config_path_and_oauth_clients(
        config: &AppConfig,
        repositories: Repositories,
        fingerprint: Fingerprint,
        installation_id: Option<String>,
        local_config_path: impl Into<PathBuf>,
        token_refresher: Arc<dyn TokenRefresher>,
        oauth_client: Arc<dyn OAuthClient>,
    ) -> Self {
        let installation_id = installation_id.filter(|id| !id.trim().is_empty());
        let runtime_fingerprint = fingerprint.clone();
        let model_config = ModelConfig {
            default_model: config.model.default_model.clone(),
            default_reasoning_effort: config.model.default_reasoning_effort.clone(),
            service_tier: config.model.service_tier.clone(),
            aliases: config.model.aliases.clone(),
        };
        let account_store = repositories.accounts;
        let cookies = repositories.cookies;
        let background_tasks = BackgroundTaskStores {
            accounts: account_store.clone(),
            admin_sessions: repositories.admin_sessions.clone(),
            cookies: cookies.clone(),
            fingerprints: repositories.fingerprints.clone(),
            session_affinity: repositories.session_affinity.clone(),
            refresh_leases: repositories.refresh_leases.clone(),
        };
        let accounts = Arc::new(account_store.clone()) as Arc<dyn AccountStore>;
        let account_pool = Arc::new(RuntimeAccountPoolService::new(
            accounts.clone(),
            account_pool_options(config),
            config.auth.request_interval_ms,
        ));
        let model_snapshots = Arc::new(repositories.model_snapshots) as Arc<dyn ModelSnapshotStore>;
        let model_catalog_client =
            upstream::model_catalog_client(config.api.base_url.clone(), fingerprint.clone())
                as Arc<dyn CodexModelCatalogClient>;
        let models = Arc::new(ModelService::new(
            model_config,
            Some(model_snapshots.clone()),
            Some(model_catalog_client),
            Some(Arc::new(tokio::sync::Mutex::new(BTreeMap::new()))),
        ));
        let client_key_store = repositories.client_keys;
        let client_keys = Arc::new(ClientKeyService::new(Arc::new(client_key_store.clone())));
        let admin_client_keys = Arc::new(AdminClientKeyService::new(client_key_store));
        let admin_sessions = Arc::new(AdminSessionService::new(
            repositories.admin_sessions,
            config.admin.default_username.clone(),
            config.admin.session_ttl_minutes,
        ));
        let settings = Arc::new(RuntimeSettingsService::with_local_config_path(
            config.clone(),
            local_config_path,
        ));
        let admin_models = Arc::new(AdminModelService::new(
            models.clone(),
            accounts.clone(),
            installation_id.clone(),
        ));
        let codex = upstream::codex_backend_client(
            config.api.base_url.clone(),
            fingerprint,
            &config.ws_pool,
        );
        let admin_accounts = Arc::new(AdminAccountService::new(
            account_store.clone(),
            cookies.clone(),
            config.quota.warning_thresholds.clone(),
            codex.clone(),
            account_pool.clone(),
            token_refresher,
            config.auth.refresh_margin_seconds,
            installation_id.clone(),
        ));
        let admin_oauth = Arc::new(AdminOAuthService::new(oauth_config(config), oauth_client));
        let logs = Arc::new(AdminLogService::new(
            repositories.event_logs,
            config.logging.enabled,
            config.logging.capacity,
            config.logging.capture_body,
        ));
        let session_affinity = Arc::new(RuntimeSessionAffinityService::new(
            repositories.session_affinity,
        ));
        let usage = Arc::new(AdminUsageService::new(account_store));
        let cloudflare_recovery = CloudflareRecovery::new(
            cookies,
            CloudflarePathBlockTracker::new(),
            CloudflareChallengeCooldownTracker::new(),
        );
        let chat = Arc::new(ChatDispatchService::new(
            account_pool.clone(),
            models.clone(),
            codex.clone(),
            logs.clone(),
            installation_id.clone(),
            cloudflare_recovery.clone(),
        ));
        let responses = Arc::new(ResponseDispatchService::new(
            account_pool.clone(),
            models.clone(),
            codex.clone(),
            session_affinity.clone(),
            logs.clone(),
            installation_id.clone(),
            cloudflare_recovery,
        ));

        Self {
            models,
            admin_models,
            accounts,
            model_snapshots,
            client_keys,
            admin_client_keys,
            admin_sessions,
            settings,
            admin_accounts,
            admin_oauth,
            logs,
            usage,
            account_pool,
            chat,
            responses,
            session_affinity,
            codex,
            fingerprint: runtime_fingerprint,
            installation_id,
            background_tasks,
        }
    }
}

impl Services {
    /// Probe the Codex models endpoint without exposing account secrets.
    pub async fn probe_codex_models_endpoint(&self, request_id: &str) -> RuntimeUpstreamProbe {
        let config = self.settings.current();
        let probe = self
            .codex
            .probe_models_endpoint(CodexRequestContext {
                access_token: "",
                account_id: None,
                request_id,
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: None,
                installation_id: self.installation_id.as_deref(),
                session_id: None,
            })
            .await;

        match probe {
            Ok(probe) => RuntimeUpstreamProbe {
                target: "codexModels",
                backend_base_url: config.api.base_url.clone(),
                endpoint: probe.endpoint,
                reachable: true,
                status_code: Some(probe.status.as_u16()),
                authorization: if probe.status.as_u16() == 401 {
                    "rejected"
                } else {
                    "unknown"
                },
            },
            Err(_) => RuntimeUpstreamProbe {
                target: "codexModels",
                backend_base_url: config.api.base_url.clone(),
                endpoint: format!(
                    "{}/codex/models?client_version={}",
                    config.api.base_url.trim_end_matches('/'),
                    self.fingerprint.app_version
                ),
                reachable: false,
                status_code: None,
                authorization: "unknown",
            },
        }
    }
}

pub(crate) fn oauth_config(config: &AppConfig) -> OAuthConfig {
    OAuthConfig {
        client_id: config.auth.oauth_client_id.clone(),
        auth_endpoint: config.auth.oauth_auth_endpoint.clone(),
        device_code_endpoint: oauth_device_code_endpoint(&config.auth.oauth_token_endpoint),
        token_endpoint: config.auth.oauth_token_endpoint.clone(),
    }
}

fn oauth_device_code_endpoint(token_endpoint: &str) -> String {
    token_endpoint
        .strip_suffix("/token")
        .map(|prefix| format!("{prefix}/device/code"))
        .unwrap_or_else(|| "https://auth.openai.com/oauth/device/code".to_string())
}
