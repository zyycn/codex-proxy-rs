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
            NewAccount, SqliteAccountStore, StoredAccount, StoredAccountMetadata,
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
        cloudflare::CloudflarePathBlockTracker,
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
            events::{extract_sse_usage, parse_rate_limit_headers, rate_limit_quota, TokenUsage},
            responses::{CodexCompactRequest, CodexResponsesRequest},
            sse::{encode_sse_event, parse_sse_events, SseError},
        },
        openai::chat::chat_completion_from_codex_sse,
        openai::responses::{
            completed_response_metadata, reconvert_responses_sse_event_tuple_values,
            response_from_codex_sse, CollectedResponse, ResponsesSseFailure,
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
        quota::{quota_from_usage, quota_reached},
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

#[derive(Clone, Copy)]
enum ExhaustedAccountClass {
    QuotaExhausted,
    RateLimited,
    Expired,
    CloudflareChallenge,
    CloudflarePathBlocked,
    ModelUnsupported,
}

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
        let cloudflare_recovery =
            CloudflareRecovery::new(cookies, CloudflarePathBlockTracker::new());
        let chat = Arc::new(ChatDispatchService::new(
            account_pool.clone(),
            models.clone(),
            codex.clone(),
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

fn account_pool_options(config: &AppConfig) -> AccountPoolOptions {
    AccountPoolOptions {
        max_concurrent_per_account: config.auth.max_concurrent_per_account,
        stale_slot_ttl: Duration::minutes(5),
        rotation_strategy: match config.auth.rotation_strategy.as_str() {
            "round_robin" => RotationStrategy::RoundRobin,
            "sticky" => RotationStrategy::Sticky,
            _ => RotationStrategy::LeastUsed,
        },
        skip_quota_limited: config.quota.skip_exhausted,
        tier_priority: config.auth.tier_priority.clone(),
        model_plan_allowlist: BTreeMap::new(),
    }
}

/// 运行时账号池服务。
#[derive(Clone)]
pub struct RuntimeAccountPoolService {
    accounts: Arc<dyn AccountStore>,
    pool: Arc<tokio::sync::Mutex<AccountPool>>,
    request_interval: StdDuration,
}

impl RuntimeAccountPoolService {
    /// 构造运行时账号池服务。
    pub fn new(
        accounts: Arc<dyn AccountStore>,
        options: AccountPoolOptions,
        request_interval_ms: u64,
    ) -> Self {
        Self {
            accounts,
            pool: Arc::new(tokio::sync::Mutex::new(AccountPool::with_options(options))),
            request_interval: StdDuration::from_millis(request_interval_ms),
        }
    }

    /// 从账号存储恢复账号池内容。
    pub async fn restore_from_repository(&self) -> Result<usize, RuntimeAccountPoolError> {
        let accounts = self.accounts.list_pool_accounts().await?;
        let restored = accounts.len();
        let mut pool = self.pool.lock().await;
        pool.clear();
        for account in accounts {
            pool.insert(account);
        }
        Ok(restored)
    }

    /// 从账号存储同步单个账号到运行时账号池；账号已不存在时从池中移除。
    pub async fn sync_account_from_repository(
        &self,
        account_id: &str,
    ) -> Result<bool, RuntimeAccountPoolError> {
        let account = self.accounts.get_pool_account(account_id).await?;
        let mut pool = self.pool.lock().await;
        if let Some(account) = account {
            pool.insert(account);
            return Ok(true);
        }
        Ok(pool.remove(account_id))
    }

    /// 从运行时账号池移除账号。
    pub async fn remove_account(&self, account_id: &str) -> bool {
        self.pool.lock().await.remove(account_id)
    }

    /// 清空运行时账号池。
    pub async fn clear(&self) {
        self.pool.lock().await.clear();
    }

    /// 从账号池获取指定模型可用账号。
    pub async fn acquire(&self, model: &str, now: DateTime<Utc>) -> Option<AcquiredAccount> {
        self.acquire_with(AccountAcquireRequest::new(model, now))
            .await
    }

    /// 使用完整获取请求从账号池获取账号。
    pub async fn acquire_with(&self, request: AccountAcquireRequest) -> Option<AcquiredAccount> {
        let acquired = self.pool.lock().await.acquire_with(request)?;
        if let Err(error) = self.accounts.record_request(&acquired.account.id).await {
            tracing::warn!(
                account_id = acquired.account.id,
                error = %error,
                "failed to persist account request usage"
            );
        }
        Some(acquired)
    }

    /// 等待同一账号前一个在途请求满足配置的发送间隔。
    pub async fn wait_for_request_interval(&self, acquired: &AcquiredAccount) {
        if self.request_interval.is_zero() {
            return;
        }
        let Some(previous_slot_at) = acquired.previous_slot_at else {
            return;
        };
        let elapsed = Utc::now()
            .signed_duration_since(previous_slot_at)
            .to_std()
            .unwrap_or_default();
        if elapsed < self.request_interval {
            sleep(self.request_interval - elapsed).await;
        }
    }

    /// 释放账号的一个在途槽位。
    pub async fn release(&self, account_id: &str) {
        self.pool.lock().await.release(account_id);
    }

    /// Return a snapshot of runtime account-pool capacity.
    pub async fn capacity_summary(&self, now: DateTime<Utc>) -> AccountCapacitySummary {
        self.pool.lock().await.capacity_summary(now)
    }

    /// Return a snapshot of runtime account-pool capacity using the current time.
    pub async fn capacity_summary_now(&self) -> AccountCapacitySummary {
        self.capacity_summary(Utc::now()).await
    }

    /// 标记账号因配额限流进入冷却。
    pub async fn mark_quota_limited_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> bool {
        let persisted = match self
            .accounts
            .mark_quota_limited_until(account_id, cooldown_until)
            .await
        {
            Ok(persisted) => persisted,
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist quota cooldown state"
                );
                false
            }
        };
        let in_memory = self
            .pool
            .lock()
            .await
            .mark_quota_limited_until(account_id, cooldown_until);
        persisted || in_memory
    }

    /// 标记账号处于 Cloudflare 冷却期。
    pub async fn set_cloudflare_cooldown_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> bool {
        let persisted = match self
            .accounts
            .set_cloudflare_cooldown_until(account_id, cooldown_until)
            .await
        {
            Ok(persisted) => persisted,
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist Cloudflare cooldown state"
                );
                false
            }
        };
        let in_memory = self
            .pool
            .lock()
            .await
            .set_cloudflare_cooldown_until(account_id, cooldown_until);
        persisted || in_memory
    }

    /// 更新账号状态。
    pub async fn set_status(&self, account_id: &str, status: AccountStatus) -> bool {
        let persisted = match self.accounts.set_status(account_id, status).await {
            Ok(persisted) => persisted,
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist account status"
                );
                false
            }
        };
        let in_memory = self.pool.lock().await.set_status(account_id, status);
        persisted || in_memory
    }

    /// 清零运行时账号池中的累计和窗口用量。
    pub async fn reset_usage(&self, account_id: &str) -> bool {
        self.pool.lock().await.reset_usage(account_id)
    }

    /// 记录账号成功响应的 token 用量。
    pub async fn record_token_usage(&self, account_id: &str, usage: TokenUsage) {
        self.record_response_usage(account_id, usage, false).await;
    }

    /// 记录 Responses 成功响应的 token 与工具用量。
    pub async fn record_response_usage(
        &self,
        account_id: &str,
        usage: TokenUsage,
        image_generation_requested: bool,
    ) {
        let image_request_succeeded = image_generation_requested && usage.image_output_tokens > 0;
        let image_request_failed = image_generation_requested && !image_request_succeeded;
        let mut persisted_usage = UsageService::account_delta_from_token_usage(usage);
        persisted_usage.image_requests = bool_to_u64(image_request_succeeded);
        persisted_usage.image_request_failures = bool_to_u64(image_request_failed);
        if let Err(error) = self
            .accounts
            .record_usage_delta(account_id, persisted_usage)
            .await
        {
            tracing::warn!(
                account_id,
                error = %error,
                "failed to persist account token usage"
            );
        }
        self.pool.lock().await.record_window_token_usage(
            account_id,
            AccountWindowUsageDelta {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cached_tokens: usage.cached_tokens,
                image_input_tokens: usage.image_input_tokens,
                image_output_tokens: usage.image_output_tokens,
                image_request_succeeded,
                image_request_failed,
            },
        );
    }

    /// 记录 Responses 空响应尝试。
    pub async fn record_empty_response_attempt(
        &self,
        account_id: &str,
        image_generation_requested: bool,
    ) {
        let usage = codex_proxy_core::accounts::usage::AccountUsageDelta {
            empty_responses: 1,
            image_request_failures: bool_to_u64(image_generation_requested),
            ..codex_proxy_core::accounts::usage::AccountUsageDelta::default()
        };
        if let Err(error) = self.accounts.record_usage_delta(account_id, usage).await {
            tracing::warn!(
                account_id,
                error = %error,
                "failed to persist empty response usage"
            );
        }
        if image_generation_requested {
            self.pool.lock().await.record_window_token_usage(
                account_id,
                AccountWindowUsageDelta {
                    image_request_failed: true,
                    ..AccountWindowUsageDelta::default()
                },
            );
        }
    }

    /// 将上游成功响应头里的 rate-limit 状态被动写回配额和窗口缓存。
    pub async fn sync_passive_rate_limit_headers(
        &self,
        account: &Account,
        headers: &[(String, String)],
    ) {
        let Some(rate_limits) = parse_rate_limit_headers(headers) else {
            return;
        };
        let existing_quota = match self.accounts.get_quota_json(&account.id).await {
            Ok(Some(quota_json)) => serde_json::from_str::<Value>(&quota_json).ok(),
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(
                    account_id = %account.id,
                    error = %error,
                    "failed to read existing quota json before passive rate-limit sync"
                );
                None
            }
        };
        let quota = rate_limit_quota(
            &rate_limits,
            account.plan_type.as_deref(),
            existing_quota.as_ref(),
        );
        if let Err(error) = self
            .accounts
            .update_quota_json(&account.id, &quota.to_string())
            .await
        {
            tracing::warn!(
                account_id = %account.id,
                error = %error,
                "failed to persist passive rate-limit quota"
            );
        }

        let Some(primary) = rate_limits.primary else {
            return;
        };
        let Some(reset_at) = primary.reset_at_datetime() else {
            return;
        };
        let limit_window_seconds = primary.limit_window_seconds();
        if let Err(error) = self
            .accounts
            .sync_rate_limit_window(&account.id, reset_at, limit_window_seconds)
            .await
        {
            tracing::warn!(
                account_id = %account.id,
                error = %error,
                "failed to persist passive rate-limit window"
            );
        }
        self.pool
            .lock()
            .await
            .sync_rate_limit_window(&account.id, reset_at, limit_window_seconds);
        let limit_reached = quota
            .pointer("/rate_limit/limit_reached")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if limit_reached {
            self.mark_quota_limited_until(&account.id, reset_at).await;
        }
    }
}

/// 运行时账号池错误。
#[derive(Debug, Error)]
pub enum RuntimeAccountPoolError {
    /// 账号存储访问失败。
    #[error("account store error: {0}")]
    Store(#[from] AccountStoreError),
}

const CLOUDFLARE_CHALLENGE_COOLDOWN_SECONDS: i64 = 10;

#[derive(Clone)]
pub(crate) struct CloudflareRecovery {
    cookies: SqliteCookieStore,
    path_block_tracker: CloudflarePathBlockTracker,
}

impl CloudflareRecovery {
    fn new(cookies: SqliteCookieStore, path_block_tracker: CloudflarePathBlockTracker) -> Self {
        Self {
            cookies,
            path_block_tracker,
        }
    }

    async fn cookie_header_for_request(
        &self,
        account_id: &str,
        request_path: &str,
    ) -> Option<String> {
        match self
            .cookies
            .cookie_header_for_request(account_id, "chatgpt.com", request_path)
            .await
        {
            Ok(cookie_header) => cookie_header,
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to read account cookies for upstream request"
                );
                None
            }
        }
    }

    async fn capture_set_cookie_headers(&self, account_id: &str, headers: &[String]) {
        for header in headers {
            if let Err(error) = self.cookies.capture_set_cookie(account_id, header).await {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist upstream set-cookie header"
                );
            }
        }
    }

    async fn apply_challenge(&self, account_pool: &RuntimeAccountPoolService, account_id: &str) {
        self.delete_account_cookies(account_id, "Cloudflare challenge")
            .await;
        let cooldown_until = Utc::now() + Duration::seconds(CLOUDFLARE_CHALLENGE_COOLDOWN_SECONDS);
        account_pool
            .set_cloudflare_cooldown_until(account_id, cooldown_until)
            .await;
    }

    async fn apply_path_block(&self, account_pool: &RuntimeAccountPoolService, account_id: &str) {
        self.delete_account_cookies(account_id, "Cloudflare path-block")
            .await;
        let now = Utc::now();
        let count = self
            .path_block_tracker
            .record_path_block(account_id, now)
            .await;
        if self
            .path_block_tracker
            .should_disable(account_id, now)
            .await
        {
            account_pool
                .set_status(account_id, AccountStatus::Disabled)
                .await;
        }
        tracing::warn!(
            account_id,
            path_block_count = count,
            "upstream returned Cloudflare path-block"
        );
    }

    async fn reset_path_block(&self, account_id: &str) {
        self.path_block_tracker.reset(account_id).await;
    }

    async fn delete_account_cookies(&self, account_id: &str, reason: &str) {
        if let Err(error) = self.cookies.delete_account_cookies(account_id).await {
            tracing::warn!(
                account_id,
                reason,
                error = %error,
                "failed to delete account cookies after Cloudflare recovery signal"
            );
        }
    }
}

/// 默认会话亲和性 TTL 秒数。
const DEFAULT_SESSION_AFFINITY_TTL_SECS: i64 = 4 * 60 * 60;

/// 运行时会话亲和性服务。
#[derive(Clone)]
pub struct RuntimeSessionAffinityService {
    store: SqliteSessionAffinityStore,
    map: Arc<tokio::sync::RwLock<SessionAffinityMap>>,
    ttl: Duration,
}

/// 默认 reasoning replay TTL 秒数。
const DEFAULT_REASONING_REPLAY_TTL_SECS: i64 = 55 * 60;
const IMPLICIT_RESUME_MAX_AGE_SECS: i64 = DEFAULT_REASONING_REPLAY_TTL_SECS;

impl RuntimeSessionAffinityService {
    /// 构造运行时会话亲和性服务。
    pub fn new(store: SqliteSessionAffinityStore) -> Self {
        let ttl = Duration::seconds(DEFAULT_SESSION_AFFINITY_TTL_SECS);
        Self {
            store,
            map: Arc::new(tokio::sync::RwLock::new(SessionAffinityMap::new(ttl))),
            ttl,
        }
    }

    /// 从 SQLite 恢复未过期的会话亲和性记录。
    pub async fn restore_from_repository(
        &self,
        now: DateTime<Utc>,
    ) -> Result<usize, RuntimeSessionAffinityError> {
        let records = self.store.list_active(now).await?;
        Ok(self.map.write().await.restore(records, now))
    }

    /// 记录并持久化响应 ID 的亲和性条目。
    pub async fn record(
        &self,
        response_id: String,
        entry: SessionAffinityEntry,
    ) -> Result<(), RuntimeSessionAffinityError> {
        self.store.upsert(&response_id, &entry, self.ttl).await?;
        self.map.write().await.record(response_id, entry);
        Ok(())
    }

    /// 根据响应 ID 查找账号 ID。
    pub async fn lookup_account(&self, response_id: &str, now: DateTime<Utc>) -> Option<String> {
        self.map.read().await.lookup_account(response_id, now)
    }

    /// 根据响应 ID 查找对话 ID。
    pub async fn lookup_conversation_id(
        &self,
        response_id: &str,
        now: DateTime<Utc>,
    ) -> Option<String> {
        self.map
            .read()
            .await
            .lookup_conversation_id(response_id, now)
    }

    /// 根据响应 ID 查找 turn state。
    pub async fn lookup_turn_state(&self, response_id: &str, now: DateTime<Utc>) -> Option<String> {
        self.map.read().await.lookup_turn_state(response_id, now)
    }

    /// 根据响应 ID 查找指令哈希。
    pub async fn lookup_instructions_hash(
        &self,
        response_id: &str,
        now: DateTime<Utc>,
    ) -> Option<String> {
        self.map
            .read()
            .await
            .lookup_instructions_hash(response_id, now)
    }

    /// 根据响应 ID 查找函数调用 ID 列表。
    pub async fn lookup_function_call_ids(
        &self,
        response_id: &str,
        now: DateTime<Utc>,
    ) -> Vec<String> {
        self.map
            .read()
            .await
            .lookup_function_call_ids(response_id, now)
    }

    /// 查找指定对话和变体下最新的响应 ID。
    pub async fn lookup_latest_response_by_conversation(
        &self,
        conversation_id: &str,
        max_age: Option<Duration>,
        variant_hash: Option<&str>,
        now: DateTime<Utc>,
    ) -> Option<String> {
        self.map
            .read()
            .await
            .lookup_latest_response_by_conversation(conversation_id, max_age, variant_hash, now)
    }

    /// 删除响应 ID 的内存亲和性映射。
    pub async fn forget(&self, response_id: &str) -> bool {
        self.map.write().await.forget(response_id)
    }
}

/// 运行时会话亲和性错误。
#[derive(Debug, Error)]
pub enum RuntimeSessionAffinityError {
    /// 存储访问失败。
    #[error("session affinity store error: {0}")]
    Store(#[from] SqliteSessionAffinityStoreError),
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

/// 管理端模型服务。
#[derive(Clone)]
pub struct AdminModelService {
    models: Arc<ModelService>,
    accounts: Arc<dyn AccountStore>,
    installation_id: Option<String>,
}

impl AdminModelService {
    /// 构造管理端模型服务。
    pub fn new(
        models: Arc<ModelService>,
        accounts: Arc<dyn AccountStore>,
        installation_id: Option<String>,
    ) -> Self {
        Self {
            models,
            accounts,
            installation_id,
        }
    }

    /// 用当前活跃账号刷新上游模型快照。
    pub async fn refresh_backend_models(
        &self,
        request_id: &str,
    ) -> Result<ModelRefreshResult, AdminModelError> {
        let accounts = self
            .accounts
            .list_pool_accounts()
            .await
            .map_err(|_| AdminModelError::ListAccounts)?;
        self.models
            .refresh_backend_models_with_installation_id(
                &accounts,
                request_id,
                self.installation_id.as_deref(),
            )
            .await
            .map_err(AdminModelError::from)
    }
}

/// 管理端模型错误。
#[derive(Debug, Error)]
pub enum AdminModelError {
    /// 列出账号失败。
    #[error("failed to list accounts")]
    ListAccounts,
    /// 没有可用账号。
    #[error("no active accounts available for model refresh")]
    NoAccounts,
    /// 模型快照存储不可用。
    #[error("model snapshot store is unavailable")]
    SnapshotStoreUnavailable,
    /// 上游客户端不可用。
    #[error("model upstream client is unavailable")]
    UpstreamClientUnavailable,
    /// 写入快照失败。
    #[error("failed to store model snapshot")]
    StoreSnapshot,
    /// 读取快照失败。
    #[error("failed to load model snapshots")]
    LoadSnapshots,
    /// 所有计划刷新失败。
    #[error("all model refresh plans failed")]
    AllPlansFailed(ModelRefreshResult),
}

impl From<ModelServiceError> for AdminModelError {
    fn from(error: ModelServiceError) -> Self {
        match error {
            ModelServiceError::SnapshotStoreUnavailable => Self::SnapshotStoreUnavailable,
            ModelServiceError::UpstreamClientUnavailable => Self::UpstreamClientUnavailable,
            ModelServiceError::NoAccounts => Self::NoAccounts,
            ModelServiceError::StoreSnapshot => Self::StoreSnapshot,
            ModelServiceError::LoadSnapshots => Self::LoadSnapshots,
            ModelServiceError::AllPlansFailed(result) => Self::AllPlansFailed(result),
        }
    }
}

/// 管理端 OAuth 服务。
#[derive(Clone)]
pub struct AdminOAuthService {
    config: OAuthConfig,
    client: Arc<dyn OAuthClient>,
    sessions: Arc<tokio::sync::Mutex<PkceSessionStore>>,
}

impl AdminOAuthService {
    /// 构造管理端 OAuth 服务。
    pub fn new(config: OAuthConfig, client: Arc<dyn OAuthClient>) -> Self {
        Self {
            config,
            client,
            sessions: Arc::new(tokio::sync::Mutex::new(PkceSessionStore::default())),
        }
    }

    /// 开始 PKCE 登录。
    pub async fn start_pkce_login(&self, return_host: &str) -> PkceLogin {
        self.sessions
            .lock()
            .await
            .start_login(return_host, &self.config)
    }

    /// 请求设备码登录信息。
    pub async fn request_device_code(&self) -> Result<DeviceCode, AdminOAuthError> {
        self.client
            .request_device_code()
            .await
            .map_err(AdminOAuthError::OAuth)
    }

    /// 轮询设备码 token。
    pub async fn poll_device_token(
        &self,
        device_code: &str,
    ) -> Result<AdminDevicePoll, AdminOAuthError> {
        match self.client.poll_device_token(device_code).await {
            Ok(tokens) => Ok(AdminDevicePoll::Authorized(tokens)),
            Err(error) => {
                if let Some(code) = error.pending_code() {
                    Ok(AdminDevicePoll::Pending { code })
                } else {
                    Err(AdminOAuthError::OAuth(error))
                }
            }
        }
    }

    /// 使用回调 code/state 完成 PKCE token 交换。
    pub async fn exchange_callback(
        &self,
        code: &str,
        state: &str,
    ) -> Result<AdminOAuthCallback, AdminOAuthError> {
        let session = self
            .sessions
            .lock()
            .await
            .try_acquire(state)
            .ok_or(AdminOAuthError::InvalidState)?;

        match self
            .client
            .exchange_code(code, &session.code_verifier, &session.redirect_uri)
            .await
        {
            Ok(tokens) => {
                self.sessions.lock().await.complete(state);
                Ok(AdminOAuthCallback {
                    tokens,
                    return_host: session.return_host,
                })
            }
            Err(error) => {
                self.sessions.lock().await.release(state);
                Err(AdminOAuthError::OAuth(error))
            }
        }
    }
}

/// 设备码轮询结果。
#[derive(Debug, Clone)]
pub enum AdminDevicePoll {
    /// 授权还未完成。
    Pending {
        /// OAuth 标准 pending 错误码。
        code: &'static str,
    },
    /// 已换取 token。
    Authorized(TokenPair),
}

/// PKCE 回调换取的 token 和返回 host。
#[derive(Debug, Clone)]
pub struct AdminOAuthCallback {
    /// OAuth token 对。
    pub tokens: TokenPair,
    /// 登录前的管理端 host。
    pub return_host: String,
}

/// 管理端 OAuth 错误。
#[derive(Debug, Error)]
pub enum AdminOAuthError {
    /// callback URL 或 query 缺少必需字段。
    #[error("invalid OAuth callback")]
    InvalidCallback,
    /// OAuth state 不存在、过期或正在处理。
    #[error("invalid OAuth state")]
    InvalidState,
    /// OAuth 上游错误。
    #[error("{0}")]
    OAuth(OAuthError),
}

/// 管理端用量服务。
#[derive(Clone)]
pub struct AdminUsageService {
    store: SqliteAccountStore,
}

impl AdminUsageService {
    /// 构造管理端用量服务。
    pub fn new(store: SqliteAccountStore) -> Self {
        Self { store }
    }

    /// 分页列出账号用量统计。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<AdminUsageRecord>, AdminUsageError> {
        let page = self
            .store
            .list_usage(cursor, limit)
            .await
            .map_err(|_| AdminUsageError::List)?;
        Ok(Page {
            items: page.items.into_iter().map(AdminUsageRecord::from).collect(),
            next_cursor: page.next_cursor,
        })
    }

    /// 汇总账号用量统计。
    pub async fn summary(&self) -> Result<AdminUsageSummary, AdminUsageError> {
        self.store
            .usage_summary()
            .await
            .map(AdminUsageSummary::from)
            .map_err(|_| AdminUsageError::Summary)
    }
}

/// 管理端用量错误。
#[derive(Debug, Error)]
pub enum AdminUsageError {
    /// 列表失败。
    #[error("failed to list account usage")]
    List,
    /// 汇总失败。
    #[error("failed to summarize account usage")]
    Summary,
}

/// 管理端用量记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminUsageRecord {
    /// 账号 ID。
    pub account_id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 管理标签。
    pub label: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// 请求数。
    pub request_count: i64,
    /// 空响应数。
    pub empty_response_count: i64,
    /// 输入 token 数。
    pub input_tokens: i64,
    /// 输出 token 数。
    pub output_tokens: i64,
    /// 缓存 token 数。
    pub cached_tokens: i64,
    /// 图片输入 token 数。
    pub image_input_tokens: i64,
    /// 图片输出 token 数。
    pub image_output_tokens: i64,
    /// 图片请求数。
    pub image_request_count: i64,
    /// 图片请求失败数。
    pub image_request_failed_count: i64,
    /// 最近使用时间。
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 管理端用量汇总。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminUsageSummary {
    /// 有用量记录的账号数。
    pub account_count: i64,
    /// 请求总数。
    pub request_count: i64,
    /// 空响应总数。
    pub empty_response_count: i64,
    /// 输入 token 总数。
    pub input_tokens: i64,
    /// 输出 token 总数。
    pub output_tokens: i64,
    /// 缓存 token 总数。
    pub cached_tokens: i64,
    /// 图片输入 token 总数。
    pub image_input_tokens: i64,
    /// 图片输出 token 总数。
    pub image_output_tokens: i64,
    /// 图片请求总数。
    pub image_request_count: i64,
    /// 图片请求失败总数。
    pub image_request_failed_count: i64,
}

/// OpenAI Chat Completions 调度服务。
#[derive(Clone)]
pub struct ChatDispatchService {
    account_pool: Arc<RuntimeAccountPoolService>,
    models: Arc<ModelService>,
    codex: Arc<CodexBackendClient>,
    installation_id: Option<String>,
    cloudflare: CloudflareRecovery,
}

impl ChatDispatchService {
    /// 构造 Chat Completions 调度服务。
    pub(crate) fn new(
        account_pool: Arc<RuntimeAccountPoolService>,
        models: Arc<ModelService>,
        codex: Arc<CodexBackendClient>,
        installation_id: Option<String>,
        cloudflare: CloudflareRecovery,
    ) -> Self {
        Self {
            account_pool,
            models,
            codex,
            installation_id,
            cloudflare,
        }
    }

    /// 调度非流式 Chat Completions 请求到 Codex Responses 上游。
    pub async fn complete(
        &self,
        request_id: &str,
        mut request: CodexResponsesRequest,
        requested_model: &str,
    ) -> Result<Value, ChatDispatchError> {
        let catalog = self.models.catalog().await;
        let parsed_model = catalog.parse_model_name(requested_model);
        let display_model =
            codex_proxy_core::models::catalog::ModelCatalog::build_display_model_name(
                &parsed_model,
            );
        request.model = parsed_model.model_id;
        let include_reasoning = request.reasoning.is_some();
        let tuple_schema = request.tuple_schema.clone();
        let mut excluded_account_ids = Vec::new();
        let mut rate_limited_count = 0usize;
        let mut last_rate_limit_error = None;
        let mut quota_exhausted_count = 0usize;
        let mut last_quota_error = None;
        let mut expired_count = 0usize;
        let mut last_auth_error = None;
        let mut cloudflare_challenge_count = 0usize;
        let mut last_cloudflare_challenge_error = None;
        let mut cloudflare_path_block_count = 0usize;
        let mut last_cloudflare_path_block_error = None;
        let mut model_unsupported_count = 0usize;
        let mut last_model_unsupported_error = None;
        let mut model_unsupported_retry_used = false;
        let (account_id, response) = loop {
            let acquire_request = AccountAcquireRequest::new(&request.model, Utc::now())
                .with_exclude_account_ids(excluded_account_ids.iter().cloned());
            let acquired = match self.account_pool.acquire_with(acquire_request).await {
                Some(acquired) => acquired,
                None if quota_exhausted_count > 0 => {
                    return Err(ChatDispatchError::QuotaExhausted {
                        count: quota_exhausted_count,
                        upstream_error: last_quota_error.unwrap_or_default(),
                    });
                }
                None if rate_limited_count > 0 => {
                    return Err(ChatDispatchError::RateLimited {
                        count: rate_limited_count,
                        upstream_error: last_rate_limit_error.unwrap_or_default(),
                    });
                }
                None if expired_count > 0 => {
                    return Err(ChatDispatchError::Expired {
                        count: expired_count,
                        upstream_error: last_auth_error.unwrap_or_default(),
                    });
                }
                None if cloudflare_challenge_count > 0 => {
                    return Err(ChatDispatchError::CloudflareChallenge {
                        count: cloudflare_challenge_count,
                        upstream_error: last_cloudflare_challenge_error.unwrap_or_default(),
                    });
                }
                None if cloudflare_path_block_count > 0 => {
                    return Err(ChatDispatchError::CloudflarePathBlocked {
                        count: cloudflare_path_block_count,
                        upstream_error: last_cloudflare_path_block_error.unwrap_or_default(),
                    });
                }
                None if model_unsupported_count > 0 => {
                    return Err(ChatDispatchError::ModelUnsupported {
                        count: model_unsupported_count,
                        upstream_error: last_model_unsupported_error.unwrap_or_default(),
                    });
                }
                None => return Err(ChatDispatchError::NoActiveAccount),
            };
            self.account_pool.wait_for_request_interval(&acquired).await;
            let account = acquired.account;
            let release_account_id = account.id.clone();
            let response_result = create_response_with_account(
                &self.codex,
                self.installation_id.as_deref(),
                &self.cloudflare,
                &request,
                request_id,
                &account,
            )
            .await;
            self.account_pool.release(&release_account_id).await;

            match response_result {
                Ok(response) => break (release_account_id, response),
                Err(error) if is_rate_limit_upstream_error(&error) => {
                    rate_limited_count += 1;
                    last_rate_limit_error = Some(upstream_error_body(&error));
                    let cooldown_until = rate_limit_cooldown_until(&error, Utc::now());
                    self.account_pool
                        .mark_quota_limited_until(&release_account_id, cooldown_until)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_quota_exhausted_upstream_error(&error) => {
                    quota_exhausted_count += 1;
                    last_quota_error = Some(upstream_error_body(&error));
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_auth_upstream_error(&error) => {
                    expired_count += 1;
                    last_auth_error = Some(upstream_error_body(&error));
                    let account_status = auth_failure_account_status(&error);
                    self.account_pool
                        .set_status(&release_account_id, account_status)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_challenge_upstream_error(&error) => {
                    cloudflare_challenge_count += 1;
                    last_cloudflare_challenge_error =
                        Some(cloudflare_challenge_error_message().to_string());
                    self.cloudflare
                        .apply_challenge(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_path_block_upstream_error(&error) => {
                    cloudflare_path_block_count += 1;
                    last_cloudflare_path_block_error =
                        Some(cloudflare_path_block_error_message().to_string());
                    self.cloudflare
                        .apply_path_block(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_model_unsupported_upstream_error(&error) => {
                    let upstream_error = upstream_error_body(&error);
                    if model_unsupported_retry_used {
                        return Err(ChatDispatchError::ModelUnsupported {
                            count: model_unsupported_count + 1,
                            upstream_error,
                        });
                    }
                    model_unsupported_count += 1;
                    last_model_unsupported_error = Some(upstream_error);
                    model_unsupported_retry_used = true;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_banned_upstream_error(&error) => {
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::Banned)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) => return Err(ChatDispatchError::Upstream(error)),
            }
        };
        let body = chat_completion_from_codex_sse(
            &response.body,
            &display_model,
            include_reasoning,
            tuple_schema.as_ref(),
        )
        .map_err(ChatDispatchError::InvalidSse)?
        .ok_or(ChatDispatchError::EmptyUpstreamResponse)?;
        self.cloudflare.reset_path_block(&account_id).await;
        if let Some(usage) = response.usage {
            self.account_pool
                .record_token_usage(&account_id, usage)
                .await;
        }
        Ok(body)
    }
}

/// Chat Completions 调度错误。
#[derive(Debug, Error)]
pub enum ChatDispatchError {
    /// 账号存储失败。
    #[error("failed to list runtime accounts")]
    AccountStore,
    /// 没有活跃账号。
    #[error("no active account is available")]
    NoActiveAccount,
    /// 所有账号都因配额耗尽不可用。
    #[error("all accounts exhausted by quota")]
    QuotaExhausted {
        /// 配额耗尽账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
    },
    /// 所有账号都因限流不可用。
    #[error("all accounts exhausted by rate limit")]
    RateLimited {
        /// 限流账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
    },
    /// 所有账号都因认证失效不可用。
    #[error("all accounts exhausted by expired auth")]
    Expired {
        /// 认证失效账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
    },
    /// 所有账号都因 Cloudflare challenge 暂不可用。
    #[error("all accounts exhausted by Cloudflare challenge")]
    CloudflareChallenge {
        /// Cloudflare challenge 账号数量。
        count: usize,
        /// 最后一个上游错误说明。
        upstream_error: String,
    },
    /// 所有账号都因 Cloudflare path-block 暂不可用。
    #[error("all accounts exhausted by Cloudflare path-block")]
    CloudflarePathBlocked {
        /// Cloudflare path-block 账号数量。
        count: usize,
        /// 最后一个上游错误说明。
        upstream_error: String,
    },
    /// 所有账号都不支持请求模型。
    #[error("all accounts do not support the requested model")]
    ModelUnsupported {
        /// 不支持模型的账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
    },
    /// 上游请求失败。
    #[error("upstream request failed: {0}")]
    Upstream(#[from] CodexClientError),
    /// 上游 SSE 无法解析。
    #[error("invalid upstream SSE response: {0}")]
    InvalidSse(#[from] SseError),
    /// 上游响应没有完成事件。
    #[error("upstream response did not include response.completed")]
    EmptyUpstreamResponse,
}

/// OpenAI Responses 调度服务。
#[derive(Clone)]
pub struct ResponseDispatchService {
    account_pool: Arc<RuntimeAccountPoolService>,
    models: Arc<ModelService>,
    codex: Arc<CodexBackendClient>,
    session_affinity: Arc<RuntimeSessionAffinityService>,
    reasoning_replay: Arc<tokio::sync::Mutex<ReasoningReplayCache>>,
    logs: Arc<AdminLogService>,
    installation_id: Option<String>,
    cloudflare: CloudflareRecovery,
}

/// Responses live SSE 响应体流。
pub type ResponseBodyStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, ResponseDispatchStreamError>> + Send + 'static>>;

/// Responses live SSE 调度结果。
pub struct ResponseDispatchStream {
    /// 可直接转为 HTTP body 的 SSE 字节流。
    pub body: ResponseBodyStream,
}

struct MpscResponseBodyStream {
    receiver: mpsc::Receiver<Result<Bytes, ResponseDispatchStreamError>>,
    cancel: Option<oneshot::Sender<()>>,
}

impl Drop for MpscResponseBodyStream {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

impl Stream for MpscResponseBodyStream {
    type Item = Result<Bytes, ResponseDispatchStreamError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

impl ResponseDispatchService {
    /// 构造 Responses 调度服务。
    pub(crate) fn new(
        account_pool: Arc<RuntimeAccountPoolService>,
        models: Arc<ModelService>,
        codex: Arc<CodexBackendClient>,
        session_affinity: Arc<RuntimeSessionAffinityService>,
        logs: Arc<AdminLogService>,
        installation_id: Option<String>,
        cloudflare: CloudflareRecovery,
    ) -> Self {
        Self {
            account_pool,
            models,
            codex,
            session_affinity,
            reasoning_replay: Arc::new(tokio::sync::Mutex::new(ReasoningReplayCache::new(
                Duration::seconds(DEFAULT_REASONING_REPLAY_TTL_SECS),
            ))),
            logs,
            installation_id,
            cloudflare,
        }
    }

    async fn prepare_response_session(
        &self,
        request: &mut CodexResponsesRequest,
    ) -> Option<ImplicitResumeSnapshot> {
        prepare_variant_identity(request);
        if let Some(previous_response_id) = request.previous_response_id.clone() {
            if request.prompt_cache_key.is_none() {
                request.prompt_cache_key = self
                    .session_affinity
                    .lookup_conversation_id(&previous_response_id, Utc::now())
                    .await;
            }
            if request.turn_state.as_deref().is_none_or(str::is_empty) {
                request.turn_state = self
                    .session_affinity
                    .lookup_turn_state(&previous_response_id, Utc::now())
                    .await;
            }
            ensure_prompt_cache_key(request);
            return None;
        }

        ensure_prompt_cache_key(request);
        self.apply_implicit_resume(request).await
    }

    async fn apply_implicit_resume(
        &self,
        request: &mut CodexResponsesRequest,
    ) -> Option<ImplicitResumeSnapshot> {
        let continuation_start = continuation_input_start(&request.input);
        if continuation_start == 0 || continuation_start >= request.input.len() {
            return None;
        }
        let conversation_id = request
            .prompt_cache_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())?
            .to_string();
        let snapshot = ImplicitResumeSnapshot::capture(request);
        let variant_hash = compute_variant_hash(request);
        let now = Utc::now();
        let previous_response_id = self
            .session_affinity
            .lookup_latest_response_by_conversation(
                &conversation_id,
                Some(Duration::seconds(IMPLICIT_RESUME_MAX_AGE_SECS)),
                Some(&variant_hash),
                now,
            )
            .await?;
        let current_instructions_hash = hash_instructions(Some(&request.instructions));
        if self
            .session_affinity
            .lookup_instructions_hash(&previous_response_id, now)
            .await
            .as_deref()
            != Some(current_instructions_hash.as_str())
        {
            return None;
        }
        let stored_function_call_ids = self
            .session_affinity
            .lookup_function_call_ids(&previous_response_id, now)
            .await;
        if !implicit_resume_allowed(
            &request.input[continuation_start..],
            &request.input,
            &stored_function_call_ids,
        ) {
            return None;
        }
        let account_id = self
            .session_affinity
            .lookup_account(&previous_response_id, now)
            .await?;
        let replay_items = self.reasoning_replay.lock().await.lookup(
            &previous_response_id,
            &account_id,
            &conversation_id,
            &variant_hash,
            now,
        );
        let continuation = request.input[continuation_start..].to_vec();
        let mut input = replay_items;
        input.extend(continuation);

        request.previous_response_id = Some(previous_response_id.clone());
        request.use_websocket = true;
        request.force_http_sse = false;
        request.input = input;
        if let Some(turn_state) = self
            .session_affinity
            .lookup_turn_state(&previous_response_id, now)
            .await
        {
            request.turn_state = Some(turn_state);
        }

        Some(snapshot)
    }

    async fn preferred_account_id_for_request(
        &self,
        request: &CodexResponsesRequest,
        now: DateTime<Utc>,
    ) -> Option<String> {
        let previous_response_id = request.previous_response_id.as_deref()?;
        self.session_affinity
            .lookup_account(previous_response_id, now)
            .await
    }

    async fn recover_request_history(
        &self,
        request: &mut CodexResponsesRequest,
        implicit_resume: &mut Option<ImplicitResumeSnapshot>,
    ) {
        if let Some(previous_response_id) = request.previous_response_id.as_deref() {
            self.session_affinity.forget(previous_response_id).await;
        }
        if let Some(snapshot) = implicit_resume.take() {
            snapshot.restore(request);
            request.previous_response_id = None;
            request.turn_state = None;
            request.turn_metadata = None;
        } else {
            strip_request_history(request);
        }
    }

    async fn evict_reasoning_replay(&self, request: &CodexResponsesRequest, account_id: &str) {
        evict_reasoning_replay(&self.reasoning_replay, request, account_id).await;
    }

    async fn record_response_affinity(
        &self,
        request: &CodexResponsesRequest,
        account_id: &str,
        body: &str,
        turn_state: Option<String>,
        usage: Option<TokenUsage>,
    ) {
        record_response_affinity(
            &self.session_affinity,
            &self.reasoning_replay,
            request,
            account_id,
            body,
            turn_state,
            usage,
        )
        .await;
    }

    /// 调度非流式 Responses 请求到 Codex Responses 上游。
    pub async fn complete(
        &self,
        request_id: &str,
        mut request: CodexResponsesRequest,
        requested_model: &str,
    ) -> Result<Value, ResponseDispatchError> {
        let started_at = Instant::now();
        let catalog = self.models.catalog().await;
        let parsed_model = catalog.parse_model_name(requested_model);
        apply_response_model_options(&mut request, &parsed_model, self.models.config());
        let tuple_schema = request.tuple_schema.clone();
        let image_generation_requested = request.expects_image_generation();
        let now = Utc::now();
        let mut implicit_resume = self.prepare_response_session(&mut request).await;
        let preferred_account_id = self.preferred_account_id_for_request(&request, now).await;
        let mut acquire_request = AccountAcquireRequest::new(&request.model, now);
        if let Some(preferred_account_id) = preferred_account_id {
            acquire_request = acquire_request.with_preferred_account_id(preferred_account_id);
        }
        let mut excluded_account_ids = Vec::new();
        let mut rate_limited_count = 0usize;
        let mut last_rate_limit_error = None;
        let mut quota_exhausted_count = 0usize;
        let mut last_quota_error = None;
        let mut expired_count = 0usize;
        let mut last_auth_error = None;
        let mut cloudflare_challenge_count = 0usize;
        let mut last_cloudflare_challenge_error = None;
        let mut cloudflare_path_block_count = 0usize;
        let mut last_cloudflare_path_block_error = None;
        let mut model_unsupported_count = 0usize;
        let mut last_model_unsupported_error = None;
        let mut model_unsupported_retry_used = false;
        let mut history_recovery_used = false;
        let mut last_exhausted_account_class = None;
        let mut empty_response_retries = 0u8;
        const MAX_EMPTY_RESPONSE_RETRIES: u8 = 2;
        let (account, response, collected_response) = loop {
            let mut attempt_acquire_request = acquire_request
                .clone()
                .with_exclude_account_ids(excluded_account_ids.iter().cloned());
            attempt_acquire_request.now = Utc::now();
            let acquired = match self
                .account_pool
                .acquire_with(attempt_acquire_request)
                .await
            {
                Some(acquired) => acquired,
                None => match last_exhausted_account_class {
                    Some(ExhaustedAccountClass::QuotaExhausted) => {
                        return Err(ResponseDispatchError::QuotaExhausted {
                            count: quota_exhausted_count,
                            upstream_error: last_quota_error.unwrap_or_default(),
                        });
                    }
                    Some(ExhaustedAccountClass::RateLimited) => {
                        return Err(ResponseDispatchError::RateLimited {
                            count: rate_limited_count,
                            upstream_error: last_rate_limit_error.unwrap_or_default(),
                        });
                    }
                    Some(ExhaustedAccountClass::Expired) => {
                        return Err(ResponseDispatchError::Expired {
                            count: expired_count,
                            upstream_error: last_auth_error.unwrap_or_default(),
                        });
                    }
                    Some(ExhaustedAccountClass::CloudflareChallenge) => {
                        return Err(ResponseDispatchError::CloudflareChallenge {
                            count: cloudflare_challenge_count,
                            upstream_error: last_cloudflare_challenge_error.unwrap_or_default(),
                        });
                    }
                    Some(ExhaustedAccountClass::CloudflarePathBlocked) => {
                        return Err(ResponseDispatchError::CloudflarePathBlocked {
                            count: cloudflare_path_block_count,
                            upstream_error: last_cloudflare_path_block_error.unwrap_or_default(),
                        });
                    }
                    Some(ExhaustedAccountClass::ModelUnsupported) => {
                        return Err(ResponseDispatchError::ModelUnsupported {
                            count: model_unsupported_count,
                            upstream_error: last_model_unsupported_error.unwrap_or_default(),
                        });
                    }
                    None => return Err(ResponseDispatchError::NoActiveAccount),
                },
            };
            self.account_pool.wait_for_request_interval(&acquired).await;
            let account = acquired.account;
            let release_account_id = account.id.clone();
            let response_result = create_response_with_account_retrying_5xx(
                &self.codex,
                self.installation_id.as_deref(),
                &self.cloudflare,
                &request,
                request_id,
                &account,
            )
            .await;
            self.account_pool.release(&release_account_id).await;

            match response_result {
                Ok(response) => {
                    let collected_response =
                        response_from_codex_sse(&response.body, tuple_schema.as_ref())
                            .map_err(ResponseDispatchError::InvalidSse)?;
                    if matches!(collected_response, CollectedResponse::Empty) {
                        self.account_pool
                            .record_empty_response_attempt(
                                &release_account_id,
                                image_generation_requested,
                            )
                            .await;
                        empty_response_retries += 1;
                        if empty_response_retries <= MAX_EMPTY_RESPONSE_RETRIES {
                            continue;
                        }
                    }
                    if let CollectedResponse::Failed(failure) = &collected_response {
                        if is_history_recovery_sse_failure(failure) && !history_recovery_used {
                            if sse_failure_invalid_reasoning_replay(failure) {
                                self.evict_reasoning_replay(&request, &release_account_id)
                                    .await;
                            }
                            self.recover_request_history(&mut request, &mut implicit_resume)
                                .await;
                            history_recovery_used = true;
                            continue;
                        }
                        if is_model_unsupported_sse_failure(failure) {
                            let upstream_error = sse_failure_error_body(failure);
                            if model_unsupported_retry_used {
                                return Err(ResponseDispatchError::ModelUnsupported {
                                    count: model_unsupported_count + 1,
                                    upstream_error,
                                });
                            }
                            model_unsupported_count += 1;
                            last_model_unsupported_error = Some(upstream_error);
                            last_exhausted_account_class =
                                Some(ExhaustedAccountClass::ModelUnsupported);
                            model_unsupported_retry_used = true;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        if is_quota_exhausted_sse_failure(failure) {
                            quota_exhausted_count += 1;
                            last_quota_error = Some(failure.message.clone());
                            last_exhausted_account_class =
                                Some(ExhaustedAccountClass::QuotaExhausted);
                            self.account_pool
                                .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                        if is_auth_sse_failure(failure) {
                            expired_count += 1;
                            last_auth_error = Some(sse_failure_error_body(failure));
                            last_exhausted_account_class = Some(ExhaustedAccountClass::Expired);
                            let account_status = auth_sse_failure_account_status(failure);
                            self.account_pool
                                .set_status(&release_account_id, account_status)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            continue;
                        }
                    }
                    break (account, response, collected_response);
                }
                Err(error) if is_rate_limit_upstream_error(&error) => {
                    rate_limited_count += 1;
                    last_rate_limit_error = Some(upstream_error_body(&error));
                    last_exhausted_account_class = Some(ExhaustedAccountClass::RateLimited);
                    let cooldown_until = rate_limit_cooldown_until(&error, Utc::now());
                    self.account_pool
                        .mark_quota_limited_until(&release_account_id, cooldown_until)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error)
                    if is_history_recovery_upstream_error(&error) && !history_recovery_used =>
                {
                    if client_error_invalid_reasoning_replay(&error) {
                        self.evict_reasoning_replay(&request, &release_account_id)
                            .await;
                    }
                    self.recover_request_history(&mut request, &mut implicit_resume)
                        .await;
                    history_recovery_used = true;
                }
                Err(error) if is_quota_exhausted_upstream_error(&error) => {
                    quota_exhausted_count += 1;
                    last_quota_error = Some(upstream_error_body(&error));
                    last_exhausted_account_class = Some(ExhaustedAccountClass::QuotaExhausted);
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_auth_upstream_error(&error) => {
                    expired_count += 1;
                    last_auth_error = Some(upstream_error_body(&error));
                    last_exhausted_account_class = Some(ExhaustedAccountClass::Expired);
                    let account_status = auth_failure_account_status(&error);
                    self.account_pool
                        .set_status(&release_account_id, account_status)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_challenge_upstream_error(&error) => {
                    cloudflare_challenge_count += 1;
                    last_cloudflare_challenge_error =
                        Some(cloudflare_challenge_error_message().to_string());
                    last_exhausted_account_class = Some(ExhaustedAccountClass::CloudflareChallenge);
                    self.cloudflare
                        .apply_challenge(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_path_block_upstream_error(&error) => {
                    cloudflare_path_block_count += 1;
                    last_cloudflare_path_block_error =
                        Some(cloudflare_path_block_error_message().to_string());
                    last_exhausted_account_class =
                        Some(ExhaustedAccountClass::CloudflarePathBlocked);
                    self.cloudflare
                        .apply_path_block(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_model_unsupported_upstream_error(&error) => {
                    let upstream_error = upstream_error_body(&error);
                    if model_unsupported_retry_used {
                        return Err(ResponseDispatchError::ModelUnsupported {
                            count: model_unsupported_count + 1,
                            upstream_error,
                        });
                    }
                    model_unsupported_count += 1;
                    last_model_unsupported_error = Some(upstream_error);
                    last_exhausted_account_class = Some(ExhaustedAccountClass::ModelUnsupported);
                    model_unsupported_retry_used = true;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_banned_upstream_error(&error) => {
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::Banned)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) => return Err(ResponseDispatchError::Upstream(error)),
            }
        };

        match collected_response {
            CollectedResponse::Completed(body) => {
                self.cloudflare.reset_path_block(&account.id).await;
                self.cloudflare
                    .capture_set_cookie_headers(&account.id, &response.set_cookie_headers)
                    .await;
                self.account_pool
                    .sync_passive_rate_limit_headers(&account, &response.rate_limit_headers)
                    .await;
                if let Some(usage) = response.usage {
                    self.account_pool
                        .record_response_usage(&account.id, usage, image_generation_requested)
                        .await;
                }
                self.record_response_affinity(
                    &request,
                    &account.id,
                    &response.body,
                    response.turn_state.clone(),
                    response.usage,
                )
                .await;
                record_response_event(ResponseEventRecord {
                    logs: &self.logs,
                    request_id,
                    account_id: &account.id,
                    model: requested_model,
                    started_at,
                    status_code: 200,
                    level: EventLevel::Info,
                    message: "v1 responses completed",
                    metadata: serde_json::json!({
                        "stream": false,
                        "usage": response.usage,
                    }),
                    rate_limit_headers: &response.rate_limit_headers,
                })
                .await;
                Ok(body)
            }
            CollectedResponse::Failed(failure) => Err(ResponseDispatchError::Failed(failure)),
            CollectedResponse::MissingCompleted => Err(ResponseDispatchError::MissingCompleted),
            CollectedResponse::Empty => Err(ResponseDispatchError::EmptyUpstreamResponse),
        }
    }

    /// 调度 Responses compact 请求到 Codex compact 上游。
    pub async fn compact(
        &self,
        request_id: &str,
        mut request: CodexCompactRequest,
        requested_model: &str,
    ) -> Result<Value, ResponseDispatchError> {
        let catalog = self.models.catalog().await;
        let parsed_model = catalog.parse_model_name(requested_model);
        request.model = parsed_model.model_id;
        let mut excluded_account_ids = Vec::new();
        let mut rate_limited_count = 0usize;
        let mut last_rate_limit_error = None;
        let mut quota_exhausted_count = 0usize;
        let mut last_quota_error = None;
        let mut expired_count = 0usize;
        let mut last_auth_error = None;
        let mut cloudflare_challenge_count = 0usize;
        let mut last_cloudflare_challenge_error = None;
        let mut cloudflare_path_block_count = 0usize;
        let mut last_cloudflare_path_block_error = None;
        let mut model_unsupported_count = 0usize;
        let mut last_model_unsupported_error = None;
        let mut model_unsupported_retry_used = false;

        loop {
            let acquire_request = AccountAcquireRequest::new(&request.model, Utc::now())
                .with_exclude_account_ids(excluded_account_ids.iter().cloned());
            let acquired = match self.account_pool.acquire_with(acquire_request).await {
                Some(acquired) => acquired,
                None if quota_exhausted_count > 0 => {
                    return Err(ResponseDispatchError::QuotaExhausted {
                        count: quota_exhausted_count,
                        upstream_error: last_quota_error.unwrap_or_default(),
                    });
                }
                None if rate_limited_count > 0 => {
                    return Err(ResponseDispatchError::RateLimited {
                        count: rate_limited_count,
                        upstream_error: last_rate_limit_error.unwrap_or_default(),
                    });
                }
                None if expired_count > 0 => {
                    return Err(ResponseDispatchError::Expired {
                        count: expired_count,
                        upstream_error: last_auth_error.unwrap_or_default(),
                    });
                }
                None if cloudflare_challenge_count > 0 => {
                    return Err(ResponseDispatchError::CloudflareChallenge {
                        count: cloudflare_challenge_count,
                        upstream_error: last_cloudflare_challenge_error.unwrap_or_default(),
                    });
                }
                None if cloudflare_path_block_count > 0 => {
                    return Err(ResponseDispatchError::CloudflarePathBlocked {
                        count: cloudflare_path_block_count,
                        upstream_error: last_cloudflare_path_block_error.unwrap_or_default(),
                    });
                }
                None if model_unsupported_count > 0 => {
                    return Err(ResponseDispatchError::ModelUnsupported {
                        count: model_unsupported_count,
                        upstream_error: last_model_unsupported_error.unwrap_or_default(),
                    });
                }
                None => return Err(ResponseDispatchError::NoActiveAccount),
            };
            let account = acquired.account;
            let release_account_id = account.id.clone();
            let response_result = create_compact_response_with_account_retrying_5xx(
                &self.codex,
                self.installation_id.as_deref(),
                &self.cloudflare,
                &request,
                request_id,
                &account,
            )
            .await;
            self.account_pool.release(&release_account_id).await;

            match response_result {
                Ok(response) => {
                    self.cloudflare.reset_path_block(&account.id).await;
                    return Ok(response.body);
                }
                Err(error) if is_rate_limit_upstream_error(&error) => {
                    rate_limited_count += 1;
                    last_rate_limit_error = Some(upstream_error_body(&error));
                    let cooldown_until = rate_limit_cooldown_until(&error, Utc::now());
                    self.account_pool
                        .mark_quota_limited_until(&release_account_id, cooldown_until)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_quota_exhausted_upstream_error(&error) => {
                    quota_exhausted_count += 1;
                    last_quota_error = Some(upstream_error_body(&error));
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_auth_upstream_error(&error) => {
                    expired_count += 1;
                    last_auth_error = Some(upstream_error_body(&error));
                    let account_status = auth_failure_account_status(&error);
                    self.account_pool
                        .set_status(&release_account_id, account_status)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_challenge_upstream_error(&error) => {
                    cloudflare_challenge_count += 1;
                    last_cloudflare_challenge_error =
                        Some(cloudflare_challenge_error_message().to_string());
                    self.cloudflare
                        .apply_challenge(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_path_block_upstream_error(&error) => {
                    cloudflare_path_block_count += 1;
                    last_cloudflare_path_block_error =
                        Some(cloudflare_path_block_error_message().to_string());
                    self.cloudflare
                        .apply_path_block(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_model_unsupported_upstream_error(&error) => {
                    let upstream_error = upstream_error_body(&error);
                    if model_unsupported_retry_used {
                        return Err(ResponseDispatchError::ModelUnsupported {
                            count: model_unsupported_count + 1,
                            upstream_error,
                        });
                    }
                    model_unsupported_count += 1;
                    last_model_unsupported_error = Some(upstream_error);
                    model_unsupported_retry_used = true;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_banned_upstream_error(&error) => {
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::Banned)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) => return Err(ResponseDispatchError::Upstream(error)),
            }
        }
    }

    /// 调度流式 Responses 请求到 Codex Responses 上游。
    pub async fn stream(
        &self,
        request_id: &str,
        mut request: CodexResponsesRequest,
        requested_model: &str,
    ) -> Result<ResponseDispatchStream, ResponseDispatchError> {
        let catalog = self.models.catalog().await;
        let parsed_model = catalog.parse_model_name(requested_model);
        apply_response_model_options(&mut request, &parsed_model, self.models.config());
        request.stream = true;
        let started_at = Instant::now();
        let tuple_schema = request.tuple_schema.clone();
        let now = Utc::now();
        let mut implicit_resume = self.prepare_response_session(&mut request).await;
        let preferred_account_id = self.preferred_account_id_for_request(&request, now).await;
        let mut acquire_request = AccountAcquireRequest::new(&request.model, now);
        if let Some(preferred_account_id) = preferred_account_id {
            acquire_request = acquire_request.with_preferred_account_id(preferred_account_id);
        }
        let mut excluded_account_ids = Vec::new();
        let mut rate_limited_count = 0usize;
        let mut last_rate_limit_error = None;
        let mut quota_exhausted_count = 0usize;
        let mut last_quota_error = None;
        let mut expired_count = 0usize;
        let mut last_auth_error = None;
        let mut cloudflare_challenge_count = 0usize;
        let mut last_cloudflare_challenge_error = None;
        let mut cloudflare_path_block_count = 0usize;
        let mut last_cloudflare_path_block_error = None;
        let mut model_unsupported_count = 0usize;
        let mut last_model_unsupported_error = None;
        let mut model_unsupported_retry_used = false;
        let mut history_recovery_used = false;
        let mut last_exhausted_account_class = None;
        loop {
            let mut attempt_acquire_request = acquire_request
                .clone()
                .with_exclude_account_ids(excluded_account_ids.iter().cloned());
            attempt_acquire_request.now = Utc::now();
            let acquired = match self
                .account_pool
                .acquire_with(attempt_acquire_request)
                .await
            {
                Some(acquired) => acquired,
                None => match last_exhausted_account_class {
                    Some(ExhaustedAccountClass::QuotaExhausted) => {
                        return Err(ResponseDispatchError::QuotaExhausted {
                            count: quota_exhausted_count,
                            upstream_error: last_quota_error.unwrap_or_default(),
                        });
                    }
                    Some(ExhaustedAccountClass::RateLimited) => {
                        return Err(ResponseDispatchError::RateLimited {
                            count: rate_limited_count,
                            upstream_error: last_rate_limit_error.unwrap_or_default(),
                        });
                    }
                    Some(ExhaustedAccountClass::Expired) => {
                        return Err(ResponseDispatchError::Expired {
                            count: expired_count,
                            upstream_error: last_auth_error.unwrap_or_default(),
                        });
                    }
                    Some(ExhaustedAccountClass::CloudflareChallenge) => {
                        return Err(ResponseDispatchError::CloudflareChallenge {
                            count: cloudflare_challenge_count,
                            upstream_error: last_cloudflare_challenge_error.unwrap_or_default(),
                        });
                    }
                    Some(ExhaustedAccountClass::CloudflarePathBlocked) => {
                        return Err(ResponseDispatchError::CloudflarePathBlocked {
                            count: cloudflare_path_block_count,
                            upstream_error: last_cloudflare_path_block_error.unwrap_or_default(),
                        });
                    }
                    Some(ExhaustedAccountClass::ModelUnsupported) => {
                        return Err(ResponseDispatchError::ModelUnsupported {
                            count: model_unsupported_count,
                            upstream_error: last_model_unsupported_error.unwrap_or_default(),
                        });
                    }
                    None => return Err(ResponseDispatchError::NoActiveAccount),
                },
            };
            self.account_pool.wait_for_request_interval(&acquired).await;
            let account = acquired.account;
            let release_account_id = account.id.clone();
            let response_result = create_response_stream_with_account_retrying_5xx(
                &self.codex,
                self.installation_id.as_deref(),
                &self.cloudflare,
                &request,
                request_id,
                &account,
            )
            .await;

            match response_result {
                Ok(response) => {
                    let transport = response.transport;
                    let rate_limit_headers = response.rate_limit_headers;
                    let rate_limit_header_updates = response.rate_limit_header_updates;
                    let turn_state_update = response.turn_state_update;
                    let turn_state = response.turn_state;
                    let (prefetched, body) = match prefetch_first_sse_chunk(response.body).await {
                        Ok(prefetched) => prefetched,
                        Err(ResponseDispatchError::Upstream(error))
                            if is_history_recovery_upstream_error(&error)
                                && !history_recovery_used =>
                        {
                            self.account_pool.release(&release_account_id).await;
                            if client_error_invalid_reasoning_replay(&error) {
                                self.evict_reasoning_replay(&request, &release_account_id)
                                    .await;
                            }
                            self.recover_request_history(&mut request, &mut implicit_resume)
                                .await;
                            history_recovery_used = true;
                            continue;
                        }
                        Err(error) => {
                            self.account_pool.release(&release_account_id).await;
                            return Err(error);
                        }
                    };
                    if let Some(failure) = first_sse_failure(&prefetched)? {
                        if is_history_recovery_sse_failure(&failure) && !history_recovery_used {
                            self.account_pool.release(&release_account_id).await;
                            if sse_failure_invalid_reasoning_replay(&failure) {
                                self.evict_reasoning_replay(&request, &release_account_id)
                                    .await;
                            }
                            self.recover_request_history(&mut request, &mut implicit_resume)
                                .await;
                            history_recovery_used = true;
                            continue;
                        }
                        if is_model_unsupported_sse_failure(&failure) {
                            let upstream_error = sse_failure_error_body(&failure);
                            if model_unsupported_retry_used {
                                self.account_pool.release(&release_account_id).await;
                                return Err(ResponseDispatchError::ModelUnsupported {
                                    count: model_unsupported_count + 1,
                                    upstream_error,
                                });
                            }
                            model_unsupported_count += 1;
                            last_model_unsupported_error = Some(upstream_error);
                            model_unsupported_retry_used = true;
                            excluded_account_ids.push(release_account_id);
                            self.account_pool.release(&account.id).await;
                            continue;
                        }
                        if is_quota_exhausted_sse_failure(&failure) {
                            quota_exhausted_count += 1;
                            last_quota_error = Some(failure.message.clone());
                            last_exhausted_account_class =
                                Some(ExhaustedAccountClass::QuotaExhausted);
                            self.account_pool
                                .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            self.account_pool.release(&account.id).await;
                            continue;
                        }
                        if is_auth_sse_failure(&failure) {
                            expired_count += 1;
                            last_auth_error = Some(sse_failure_error_body(&failure));
                            last_exhausted_account_class = Some(ExhaustedAccountClass::Expired);
                            let account_status = auth_sse_failure_account_status(&failure);
                            self.account_pool
                                .set_status(&release_account_id, account_status)
                                .await;
                            excluded_account_ids.push(release_account_id);
                            self.account_pool.release(&account.id).await;
                            continue;
                        }
                        self.account_pool.release(&release_account_id).await;
                        return Err(ResponseDispatchError::Failed(failure.clone()));
                    }
                    return Ok(spawn_live_response_stream(
                        LiveResponseStreamContext {
                            account_pool: Arc::clone(&self.account_pool),
                            session_affinity: Arc::clone(&self.session_affinity),
                            reasoning_replay: Arc::clone(&self.reasoning_replay),
                            logs: Arc::clone(&self.logs),
                            cloudflare: self.cloudflare.clone(),
                            account_id: account.id,
                            request_id: request_id.to_string(),
                            model: requested_model.to_string(),
                            request,
                            tuple_schema,
                            transport,
                            rate_limit_headers,
                            rate_limit_header_updates,
                            turn_state_update,
                            turn_state,
                            started_at,
                        },
                        prefetched,
                        body,
                    ));
                }
                Err(error) if is_rate_limit_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    rate_limited_count += 1;
                    last_rate_limit_error = Some(upstream_error_body(&error));
                    last_exhausted_account_class = Some(ExhaustedAccountClass::RateLimited);
                    let cooldown_until = rate_limit_cooldown_until(&error, Utc::now());
                    self.account_pool
                        .mark_quota_limited_until(&release_account_id, cooldown_until)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_quota_exhausted_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    quota_exhausted_count += 1;
                    last_quota_error = Some(upstream_error_body(&error));
                    last_exhausted_account_class = Some(ExhaustedAccountClass::QuotaExhausted);
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::QuotaExhausted)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error)
                    if is_history_recovery_upstream_error(&error) && !history_recovery_used =>
                {
                    self.account_pool.release(&release_account_id).await;
                    if client_error_invalid_reasoning_replay(&error) {
                        self.evict_reasoning_replay(&request, &release_account_id)
                            .await;
                    }
                    self.recover_request_history(&mut request, &mut implicit_resume)
                        .await;
                    history_recovery_used = true;
                }
                Err(error) if is_auth_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    expired_count += 1;
                    last_auth_error = Some(upstream_error_body(&error));
                    last_exhausted_account_class = Some(ExhaustedAccountClass::Expired);
                    let account_status = auth_failure_account_status(&error);
                    self.account_pool
                        .set_status(&release_account_id, account_status)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_challenge_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    cloudflare_challenge_count += 1;
                    last_cloudflare_challenge_error =
                        Some(cloudflare_challenge_error_message().to_string());
                    last_exhausted_account_class = Some(ExhaustedAccountClass::CloudflareChallenge);
                    self.cloudflare
                        .apply_challenge(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_cloudflare_path_block_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    cloudflare_path_block_count += 1;
                    last_cloudflare_path_block_error =
                        Some(cloudflare_path_block_error_message().to_string());
                    last_exhausted_account_class =
                        Some(ExhaustedAccountClass::CloudflarePathBlocked);
                    self.cloudflare
                        .apply_path_block(self.account_pool.as_ref(), &release_account_id)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_model_unsupported_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    let upstream_error = upstream_error_body(&error);
                    if model_unsupported_retry_used {
                        return Err(ResponseDispatchError::ModelUnsupported {
                            count: model_unsupported_count + 1,
                            upstream_error,
                        });
                    }
                    model_unsupported_count += 1;
                    last_model_unsupported_error = Some(upstream_error);
                    last_exhausted_account_class = Some(ExhaustedAccountClass::ModelUnsupported);
                    model_unsupported_retry_used = true;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) if is_banned_upstream_error(&error) => {
                    self.account_pool.release(&release_account_id).await;
                    self.account_pool
                        .set_status(&release_account_id, AccountStatus::Banned)
                        .await;
                    excluded_account_ids.push(release_account_id);
                }
                Err(error) => {
                    self.account_pool.release(&release_account_id).await;
                    return Err(ResponseDispatchError::Upstream(error));
                }
            }
        }
    }
}

async fn create_response_with_account(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexResponsesRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexBackendResponse, CodexClientError> {
    let cookie_header = cloudflare
        .cookie_header_for_request(&account.id, "/codex/responses")
        .await;
    let identity = build_conversation_identity(
        request.prompt_cache_key.as_deref(),
        request.codex_window_id.as_deref(),
        &account.id,
    );
    codex
        .create_response(
            request,
            CodexRequestContext {
                access_token: &account.access_token,
                account_id: account.account_id.as_deref(),
                request_id,
                turn_state: request.turn_state.as_deref(),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: identity.window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
                cookie_header: cookie_header.as_deref(),
                installation_id,
                session_id: identity.conversation_id.as_deref(),
            },
        )
        .await
}

async fn create_response_stream_with_account(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexResponsesRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexBackendStreamingResponse, CodexClientError> {
    let cookie_header = cloudflare
        .cookie_header_for_request(&account.id, "/codex/responses")
        .await;
    let identity = build_conversation_identity(
        request.prompt_cache_key.as_deref(),
        request.codex_window_id.as_deref(),
        &account.id,
    );
    codex
        .create_response_stream(
            request,
            CodexRequestContext {
                access_token: &account.access_token,
                account_id: account.account_id.as_deref(),
                request_id,
                turn_state: request.turn_state.as_deref(),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: identity.window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
                cookie_header: cookie_header.as_deref(),
                installation_id,
                session_id: identity.conversation_id.as_deref(),
            },
        )
        .await
}

async fn create_compact_response_with_account(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexCompactRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexCompactResponse, CodexClientError> {
    let cookie_header = cloudflare
        .cookie_header_for_request(&account.id, "/codex/responses/compact")
        .await;
    codex
        .create_compact_response(
            request,
            CodexRequestContext {
                access_token: &account.access_token,
                account_id: account.account_id.as_deref(),
                request_id,
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: cookie_header.as_deref(),
                installation_id,
                session_id: None,
            },
        )
        .await
}

const MAX_UPSTREAM_5XX_RETRIES_PER_ACCOUNT: usize = 2;

async fn create_response_with_account_retrying_5xx(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexResponsesRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexBackendResponse, CodexClientError> {
    let mut retries = 0;
    loop {
        let result = create_response_with_account(
            codex,
            installation_id,
            cloudflare,
            request,
            request_id,
            account,
        )
        .await;
        match result {
            Err(error)
                if is_retryable_upstream_5xx_error(&error)
                    && retries < MAX_UPSTREAM_5XX_RETRIES_PER_ACCOUNT =>
            {
                retries += 1;
            }
            result => return result,
        }
    }
}

async fn create_response_stream_with_account_retrying_5xx(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexResponsesRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexBackendStreamingResponse, CodexClientError> {
    let mut retries = 0;
    loop {
        let result = create_response_stream_with_account(
            codex,
            installation_id,
            cloudflare,
            request,
            request_id,
            account,
        )
        .await;
        match result {
            Err(error)
                if is_retryable_upstream_5xx_error(&error)
                    && retries < MAX_UPSTREAM_5XX_RETRIES_PER_ACCOUNT =>
            {
                retries += 1;
            }
            result => return result,
        }
    }
}

async fn create_compact_response_with_account_retrying_5xx(
    codex: &CodexBackendClient,
    installation_id: Option<&str>,
    cloudflare: &CloudflareRecovery,
    request: &CodexCompactRequest,
    request_id: &str,
    account: &Account,
) -> Result<CodexCompactResponse, CodexClientError> {
    let mut retries = 0;
    loop {
        let result = create_compact_response_with_account(
            codex,
            installation_id,
            cloudflare,
            request,
            request_id,
            account,
        )
        .await;
        match result {
            Err(error)
                if is_retryable_upstream_5xx_error(&error)
                    && retries < MAX_UPSTREAM_5XX_RETRIES_PER_ACCOUNT =>
            {
                retries += 1;
            }
            result => return result,
        }
    }
}

fn is_rate_limit_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, .. } if status_code_is_rate_limited(status.as_u16())
    )
}

fn is_retryable_upstream_5xx_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, body, .. }
            if status_code_is_transient_upstream(status.as_u16())
                && status_code_allows_same_account_retry(status.as_u16())
                && !is_history_recovery_signal(body)
    )
}

fn is_quota_exhausted_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, .. } if status_code_is_quota_exhausted(status.as_u16())
    )
}

fn is_auth_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, .. } if status.as_u16() == 401
    )
}

fn is_cloudflare_challenge_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, body, .. }
            if status.as_u16() == 403 && is_cloudflare_challenge_signal(body)
    )
}

fn is_cloudflare_path_block_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, body, .. }
            if status.as_u16() == 404 && body.trim().is_empty()
    )
}

fn is_model_unsupported_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, body, .. }
            if status.is_client_error()
                && !matches!(status.as_u16(), 401 | 402 | 403 | 404 | 429)
                && is_model_unsupported_signal(body)
    )
}

fn is_history_recovery_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { body, .. } if is_history_recovery_signal(body)
    )
}

fn is_banned_upstream_error(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { status, body, .. }
            if status.as_u16() == 403 && is_banned_auth_signal(body)
    )
}

fn auth_failure_account_status(error: &CodexClientError) -> AccountStatus {
    match error {
        CodexClientError::Upstream { body, .. } if is_banned_auth_signal(body) => {
            AccountStatus::Banned
        }
        _ => AccountStatus::Expired,
    }
}

fn upstream_error_body(error: &CodexClientError) -> String {
    match error {
        CodexClientError::Upstream { body, .. } => body.clone(),
        error => error.to_string(),
    }
}

fn sse_failure_error_body(failure: &ResponsesSseFailure) -> String {
    match failure.upstream_code.as_deref() {
        Some(code) => serde_json::json!({
            "error": {
                "code": code,
                "message": failure.message.as_str(),
            }
        })
        .to_string(),
        None => failure.message.clone(),
    }
}

fn is_quota_exhausted_sse_failure(failure: &ResponsesSseFailure) -> bool {
    failure
        .upstream_code
        .as_deref()
        .is_some_and(|code| matches!(code, "quota_exceeded" | "insufficient_quota"))
        || failure.message.to_ascii_lowercase().contains("quota")
}

fn is_auth_sse_failure(failure: &ResponsesSseFailure) -> bool {
    failure.upstream_code.as_deref().is_some_and(|code| {
        let code = code.to_ascii_lowercase();
        matches!(
            code.as_str(),
            "token_invalid"
                | "token_expired"
                | "token_revoked"
                | "account_deactivated"
                | "unauthorized"
                | "invalid_api_key"
        )
    }) || {
        let message = failure.message.to_ascii_lowercase();
        message.contains("token revoked")
            || message.contains("token invalid")
            || message.contains("token expired")
    }
}

fn is_model_unsupported_sse_failure(failure: &ResponsesSseFailure) -> bool {
    failure
        .upstream_code
        .as_deref()
        .is_some_and(is_model_unsupported_signal)
        || is_model_unsupported_signal(&failure.message)
}

fn is_history_recovery_sse_failure(failure: &ResponsesSseFailure) -> bool {
    failure
        .upstream_code
        .as_deref()
        .is_some_and(is_history_recovery_signal)
        || is_history_recovery_signal(&failure.message)
}

fn sse_failure_invalid_reasoning_replay(failure: &ResponsesSseFailure) -> bool {
    failure
        .upstream_code
        .as_deref()
        .is_some_and(is_invalid_encrypted_content_signal)
        || is_invalid_encrypted_content_signal(&failure.message)
}

fn client_error_invalid_reasoning_replay(error: &CodexClientError) -> bool {
    matches!(
        error,
        CodexClientError::Upstream { body, .. } if is_invalid_encrypted_content_signal(body)
    )
}

fn auth_sse_failure_account_status(failure: &ResponsesSseFailure) -> AccountStatus {
    if failure
        .upstream_code
        .as_deref()
        .is_some_and(is_banned_auth_signal)
        || is_banned_auth_signal(&failure.message)
    {
        AccountStatus::Banned
    } else {
        AccountStatus::Expired
    }
}

fn is_banned_auth_signal(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("account_deactivated")
        || value.contains("account deactivated")
        || value.contains("account has been deactivated")
        || value.contains("deactivated")
        || value.contains("banned")
}

fn is_cloudflare_challenge_signal(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("cf-mitigated")
        || value.contains("cf-chl-bypass")
        || value.contains("_cf_chl")
        || value.contains("cf_chl")
        || value.contains("attention required")
        || value.contains("just a moment")
}

fn is_model_unsupported_signal(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("model_not_supported")
        || value.contains("model_not_available")
        || (value.contains("model")
            && (value.contains("not supported")
                || value.contains("not available")
                || value.contains("not_supported")
                || value.contains("not_available")))
}

fn is_history_recovery_signal(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("previous_response_not_found")
        || (value.contains("previous response") && value.contains("not found"))
        || value.contains("no tool output found for function call")
        || is_invalid_encrypted_content_signal(&value)
}

fn is_invalid_encrypted_content_signal(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("invalid_encrypted_content")
        || (value.contains("invalid") && value.contains("encrypted") && value.contains("content"))
}

fn strip_request_history(request: &mut CodexResponsesRequest) {
    request.previous_response_id = None;
    request.turn_state = None;
    request.turn_metadata = None;
    if !request.explicit_prompt_cache_key {
        request.prompt_cache_key = None;
    }
}

fn cloudflare_challenge_error_message() -> &'static str {
    "Upstream blocked the request (Cloudflare challenge)"
}

fn cloudflare_path_block_error_message() -> &'static str {
    "Upstream blocked the request (Cloudflare path-block)"
}

fn rate_limit_cooldown_until(error: &CodexClientError, now: DateTime<Utc>) -> DateTime<Utc> {
    let retry_after_seconds = match error {
        CodexClientError::Upstream {
            retry_after_seconds,
            ..
        } => retry_after_seconds.unwrap_or(60),
        _ => 60,
    };
    now + Duration::seconds(retry_after_seconds.min(i64::MAX as u64) as i64)
}

const MAX_STREAM_PREFETCH_BYTES: usize = 64 * 1024;
const DONE_SSE_FRAME_TEXT: &str = "data: [DONE]\n\n";
const DONE_SSE_FRAME: &[u8] = b"data: [DONE]\n\n";

async fn prefetch_first_sse_chunk(
    mut body: CodexBackendSseStream,
) -> Result<(Bytes, CodexBackendSseStream), ResponseDispatchError> {
    let mut prefetched = Vec::new();
    while !contains_sse_event_separator(&prefetched) {
        let Some(next) = body.next().await else {
            if prefetched.is_empty() {
                return Err(ResponseDispatchError::EmptyUpstreamResponse);
            }
            return Err(ResponseDispatchError::MissingCompleted);
        };
        let chunk = next.map_err(ResponseDispatchError::Upstream)?;
        prefetched.extend_from_slice(&chunk);
        if prefetched.len() > MAX_STREAM_PREFETCH_BYTES {
            return Err(ResponseDispatchError::InvalidSse(
                SseError::BufferExceeded {
                    max_bytes: MAX_STREAM_PREFETCH_BYTES,
                },
            ));
        }
    }

    Ok((Bytes::from(prefetched), body))
}

fn contains_sse_event_separator(bytes: &[u8]) -> bool {
    bytes.windows(2).any(|window| window == b"\n\n")
        || bytes.windows(4).any(|window| window == b"\r\n\r\n")
}

fn first_sse_failure(prefetched: &[u8]) -> Result<Option<ResponsesSseFailure>, SseError> {
    let body = String::from_utf8_lossy(prefetched);
    match response_from_codex_sse(&body, None)? {
        CollectedResponse::Failed(failure) => Ok(Some(failure)),
        CollectedResponse::Completed(_)
        | CollectedResponse::MissingCompleted
        | CollectedResponse::Empty => Ok(None),
    }
}

struct LiveResponseStreamContext {
    account_pool: Arc<RuntimeAccountPoolService>,
    session_affinity: Arc<RuntimeSessionAffinityService>,
    reasoning_replay: Arc<tokio::sync::Mutex<ReasoningReplayCache>>,
    logs: Arc<AdminLogService>,
    cloudflare: CloudflareRecovery,
    account_id: String,
    request_id: String,
    model: String,
    request: CodexResponsesRequest,
    tuple_schema: Option<Value>,
    transport: CodexBackendTransport,
    rate_limit_headers: Vec<(String, String)>,
    rate_limit_header_updates: Option<CodexRateLimitHeaderUpdates>,
    turn_state_update: Option<CodexTurnStateUpdate>,
    turn_state: Option<String>,
    started_at: Instant,
}

fn spawn_live_response_stream(
    context: LiveResponseStreamContext,
    prefetched: Bytes,
    mut body: CodexBackendSseStream,
) -> ResponseDispatchStream {
    let (sender, receiver) = mpsc::channel(8);
    let (cancel_sender, mut cancel_receiver) = oneshot::channel();
    tokio::spawn(async move {
        let mut tuple_transformer = context
            .tuple_schema
            .clone()
            .map(TupleSseEventTransformer::new);
        let mut body_bytes = Vec::new();
        if !send_live_response_stream_chunk(
            &sender,
            &mut body_bytes,
            tuple_transformer.as_mut(),
            prefetched,
        )
        .await
        {
            context.account_pool.release(&context.account_id).await;
            return;
        }

        loop {
            let next = tokio::select! {
                _ = &mut cancel_receiver => {
                    context.account_pool.release(&context.account_id).await;
                    return;
                }
                next = body.next() => next,
            };
            let Some(next) = next else {
                break;
            };
            match next {
                Ok(chunk) => {
                    if !send_live_response_stream_chunk(
                        &sender,
                        &mut body_bytes,
                        tuple_transformer.as_mut(),
                        chunk,
                    )
                    .await
                    {
                        context.account_pool.release(&context.account_id).await;
                        return;
                    }
                }
                Err(error) => {
                    if !flush_live_response_stream_transformer(
                        &sender,
                        &mut body_bytes,
                        tuple_transformer.as_mut(),
                    )
                    .await
                    {
                        context.account_pool.release(&context.account_id).await;
                        return;
                    }
                    let detail = error.to_string();
                    let Some(body_text) =
                        send_live_response_stream_tail(&sender, &mut body_bytes, Some(&detail))
                            .await
                    else {
                        context.account_pool.release(&context.account_id).await;
                        return;
                    };
                    finalize_live_response_stream(context, body_text).await;
                    return;
                }
            }
        }

        if !flush_live_response_stream_transformer(
            &sender,
            &mut body_bytes,
            tuple_transformer.as_mut(),
        )
        .await
        {
            context.account_pool.release(&context.account_id).await;
            return;
        }
        let Some(body_text) = send_live_response_stream_tail(&sender, &mut body_bytes, None).await
        else {
            context.account_pool.release(&context.account_id).await;
            return;
        };

        finalize_live_response_stream(context, body_text).await;
    });

    ResponseDispatchStream {
        body: Box::pin(MpscResponseBodyStream {
            receiver,
            cancel: Some(cancel_sender),
        }),
    }
}

async fn send_live_response_stream_chunk(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    body_bytes: &mut Vec<u8>,
    transformer: Option<&mut TupleSseEventTransformer>,
    chunk: Bytes,
) -> bool {
    let chunks = match transformer {
        Some(transformer) => transformer.push(&chunk),
        None => vec![chunk],
    };
    send_live_response_stream_chunks(sender, body_bytes, chunks).await
}

async fn flush_live_response_stream_transformer(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    body_bytes: &mut Vec<u8>,
    transformer: Option<&mut TupleSseEventTransformer>,
) -> bool {
    let Some(transformer) = transformer else {
        return true;
    };
    send_live_response_stream_chunks(sender, body_bytes, transformer.finish()).await
}

async fn send_live_response_stream_chunks(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    body_bytes: &mut Vec<u8>,
    chunks: Vec<Bytes>,
) -> bool {
    for chunk in chunks {
        body_bytes.extend_from_slice(&chunk);
        if sender.send(Ok(chunk)).await.is_err() {
            return false;
        }
    }
    true
}

struct TupleSseEventTransformer {
    tuple_schema: Value,
    pending: Vec<u8>,
}

impl TupleSseEventTransformer {
    fn new(tuple_schema: Value) -> Self {
        Self {
            tuple_schema,
            pending: Vec::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<Bytes> {
        self.pending.extend_from_slice(chunk);
        let mut chunks = Vec::new();
        while let Some(frame_end) = next_sse_frame_end(&self.pending) {
            let frame = self.pending.drain(..frame_end).collect::<Vec<_>>();
            chunks.push(self.transform_frame(&frame));
        }
        chunks
    }

    fn finish(&mut self) -> Vec<Bytes> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let frame = std::mem::take(&mut self.pending);
        vec![self.transform_frame(&frame)]
    }

    fn transform_frame(&self, frame: &[u8]) -> Bytes {
        let frame_text = String::from_utf8_lossy(frame);
        let Ok(events) = parse_sse_events(&frame_text) else {
            return Bytes::copy_from_slice(frame);
        };
        let [event] = events.as_slice() else {
            return Bytes::copy_from_slice(frame);
        };
        let Ok(data) = serde_json::from_str::<Value>(&event.data) else {
            return Bytes::copy_from_slice(frame);
        };
        let transformed = reconvert_responses_sse_event_tuple_values(
            event.event.as_deref(),
            data,
            &self.tuple_schema,
        );
        Bytes::from(encode_sse_event(
            event.event.as_deref().unwrap_or_default(),
            &transformed.to_string(),
        ))
    }
}

fn next_sse_frame_end(bytes: &[u8]) -> Option<usize> {
    let lf_lf = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| position + 2);
    let crlf_crlf = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4);
    match (lf_lf, crlf_crlf) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(end), None) | (None, Some(end)) => Some(end),
        (None, None) => None,
    }
}

async fn send_live_response_stream_tail(
    sender: &mpsc::Sender<Result<Bytes, ResponseDispatchStreamError>>,
    body_bytes: &mut Vec<u8>,
    failure_detail: Option<&str>,
) -> Option<String> {
    let mut body_text = String::from_utf8_lossy(body_bytes).to_string();
    if !sse_body_has_terminal_event(&body_text) {
        if let Some(separator) = missing_sse_event_separator(&body_text) {
            body_text.push_str(separator);
            body_bytes.extend_from_slice(separator.as_bytes());
            if sender
                .send(Ok(Bytes::copy_from_slice(separator.as_bytes())))
                .await
                .is_err()
            {
                return None;
            }
        }
        let failure = premature_close_failed_event(failure_detail);
        body_text.push_str(&failure);
        body_bytes.extend_from_slice(failure.as_bytes());
        if sender.send(Ok(Bytes::from(failure))).await.is_err() {
            return None;
        }
    }

    if !sse_body_has_done(&body_text) {
        body_text.push_str(DONE_SSE_FRAME_TEXT);
        body_bytes.extend_from_slice(DONE_SSE_FRAME);
        if sender
            .send(Ok(Bytes::from_static(DONE_SSE_FRAME)))
            .await
            .is_err()
        {
            return None;
        }
    }

    Some(body_text)
}

fn sse_body_has_terminal_event(body: &str) -> bool {
    parse_sse_events(body).is_ok_and(|events| {
        events.iter().any(|event| {
            matches!(
                event.event.as_deref(),
                Some("response.completed" | "response.failed" | "error")
            )
        })
    })
}

fn missing_sse_event_separator(body: &str) -> Option<&'static str> {
    if body.is_empty()
        || body.ends_with("\n\n")
        || body.ends_with("\r\n\r\n")
        || body.ends_with("\r\r")
    {
        None
    } else if body.ends_with('\n') || body.ends_with('\r') {
        Some("\n")
    } else {
        Some("\n\n")
    }
}

fn premature_close_failed_event(detail: Option<&str>) -> String {
    let message = match detail.filter(|value| !value.trim().is_empty()) {
        Some(detail) => format!("Upstream stream closed before response.completed: {detail}"),
        None => "Upstream stream closed before response.completed".to_string(),
    };
    response_stream_failed_sse_event("server_error", "stream_disconnected", &message)
}

fn response_stream_failed_sse_event(error_type: &str, code: &str, message: &str) -> String {
    let error = serde_json::json!({
        "type": error_type,
        "code": code,
        "message": message,
    });
    let data = serde_json::json!({
        "type": "response.failed",
        "response": {
            "id": format!("resp_proxy_{}", uuid::Uuid::new_v4().simple()),
            "status": "failed",
            "error": error,
        },
        "error": error,
    });
    encode_sse_event("response.failed", &data.to_string())
}

fn sse_body_has_done(body: &str) -> bool {
    body.trim_end_matches(['\r', '\n'])
        .ends_with("data: [DONE]")
}

async fn finalize_live_response_stream(context: LiveResponseStreamContext, body: String) {
    let rate_limit_headers = live_response_rate_limit_headers(&context).await;
    let turn_state = live_response_turn_state(&context).await;
    let usage = match extract_sse_usage(&body) {
        Ok(Some(usage)) => {
            context
                .account_pool
                .record_token_usage(&context.account_id, usage)
                .await;
            Some(usage)
        }
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(account_id = %context.account_id, error = %error, "failed to extract streaming token usage");
            None
        }
    };

    match response_from_codex_sse(&body, context.tuple_schema.as_ref()) {
        Ok(CollectedResponse::Completed(completed)) => {
            context
                .cloudflare
                .reset_path_block(&context.account_id)
                .await;
            let response_id = completed.get("id").and_then(Value::as_str);
            record_response_affinity(
                &context.session_affinity,
                &context.reasoning_replay,
                &context.request,
                &context.account_id,
                &body,
                turn_state,
                usage,
            )
            .await;
            record_live_response_stream_event(
                &context,
                200,
                EventLevel::Info,
                "v1 responses stream completed",
                serde_json::json!({
                    "stream": true,
                    "completed": true,
                    "responseId": response_id,
                    "usage": usage,
                }),
                &rate_limit_headers,
                &body,
            )
            .await;
        }
        Ok(CollectedResponse::Failed(failure)) => {
            if sse_failure_invalid_reasoning_replay(&failure) {
                evict_reasoning_replay(
                    &context.reasoning_replay,
                    &context.request,
                    &context.account_id,
                )
                .await;
            }
            tracing::warn!(
                account_id = %context.account_id,
                event = %failure.event,
                code = ?failure.upstream_code.as_deref(),
                "live upstream stream ended with response.failed"
            );
            record_live_response_stream_event(
                &context,
                status_code_for_stream_failure(&failure),
                EventLevel::Error,
                "v1 responses stream failed",
                stream_failure_metadata(&failure, usage),
                &rate_limit_headers,
                &body,
            )
            .await;
        }
        Ok(CollectedResponse::MissingCompleted | CollectedResponse::Empty) => {
            tracing::warn!(
                account_id = %context.account_id,
                "live upstream stream ended without response.completed"
            );
            record_live_response_stream_event(
                &context,
                502,
                EventLevel::Error,
                "v1 responses stream ended without response.completed",
                serde_json::json!({
                    "stream": true,
                    "failed": true,
                    "upstreamCode": "missing_completed",
                    "usage": usage,
                }),
                &rate_limit_headers,
                &body,
            )
            .await;
        }
        Err(error) => {
            tracing::warn!(account_id = %context.account_id, error = %error, "failed to parse completed live stream");
            record_live_response_stream_event(
                &context,
                502,
                EventLevel::Warn,
                "v1 responses stream SSE response invalid",
                serde_json::json!({
                    "stream": true,
                    "sseParseError": error.to_string(),
                    "usage": usage,
                }),
                &rate_limit_headers,
                &body,
            )
            .await;
        }
    }

    context.account_pool.release(&context.account_id).await;
}

async fn record_response_affinity(
    session_affinity: &Arc<RuntimeSessionAffinityService>,
    reasoning_replay: &Arc<tokio::sync::Mutex<ReasoningReplayCache>>,
    request: &CodexResponsesRequest,
    account_id: &str,
    body: &str,
    turn_state: Option<String>,
    usage: Option<TokenUsage>,
) {
    let Some(conversation_id) = request
        .prompt_cache_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let metadata = match completed_response_metadata(body) {
        Ok(Some(metadata)) => metadata,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "failed to parse completed response metadata for session affinity"
            );
            return;
        }
    };

    let variant_hash = compute_variant_hash(request);
    let entry = SessionAffinityEntry {
        account_id: account_id.to_string(),
        conversation_id: conversation_id.to_string(),
        turn_state: turn_state
            .filter(|value| !value.trim().is_empty())
            .or_else(|| request.turn_state.clone()),
        instructions_hash: Some(hash_instructions(Some(&request.instructions))),
        input_tokens: usage.map(|usage| usage.input_tokens),
        function_call_ids: metadata.function_call_ids,
        variant_hash: Some(variant_hash.clone()),
        created_at: Utc::now(),
    };
    if let Err(error) = session_affinity
        .record(metadata.response_id.clone(), entry)
        .await
    {
        tracing::warn!(
            error = %error,
            response_id = %metadata.response_id,
            account_id = %account_id,
            "failed to record session affinity"
        );
    }

    reasoning_replay.lock().await.record(
        metadata.response_id,
        account_id,
        conversation_id,
        &variant_hash,
        &metadata.replay_items,
        Utc::now(),
    );
}

async fn evict_reasoning_replay(
    reasoning_replay: &Arc<tokio::sync::Mutex<ReasoningReplayCache>>,
    request: &CodexResponsesRequest,
    account_id: &str,
) {
    let Some(conversation_id) = request
        .prompt_cache_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let variant_hash = compute_variant_hash(request);
    let evicted = reasoning_replay.lock().await.evict_by_identity(
        account_id,
        conversation_id,
        &variant_hash,
        Utc::now(),
    );
    if evicted > 0 {
        tracing::info!(
            account_id = %account_id,
            conversation_id = %conversation_id,
            variant_hash = %variant_hash,
            evicted,
            "evicted reasoning replay after invalid encrypted content"
        );
    }
}

struct ResponseEventRecord<'a> {
    logs: &'a AdminLogService,
    request_id: &'a str,
    account_id: &'a str,
    model: &'a str,
    started_at: Instant,
    status_code: i64,
    level: EventLevel,
    message: &'a str,
    metadata: Value,
    rate_limit_headers: &'a [(String, String)],
}

async fn record_response_event(mut record: ResponseEventRecord<'_>) {
    if let Some(object) = record.metadata.as_object_mut() {
        if !record.rate_limit_headers.is_empty() {
            object
                .entry("rateLimitHeaders".to_string())
                .or_insert_with(|| serde_json::json!(record.rate_limit_headers));
        }
    }
    let mut event = EventLog::new("v1.response", record.level, record.message);
    event.request_id = Some(record.request_id.to_string());
    event.account_id = Some(record.account_id.to_string());
    event.route = Some("/v1/responses".to_string());
    event.model = Some(record.model.to_string());
    event.status_code = Some(record.status_code);
    event.latency_ms = Some(elapsed_millis_i64(record.started_at));
    event.metadata = record.metadata;
    if let Err(error) = record.logs.record(event).await {
        tracing::warn!(account_id = record.account_id, error = %error, "failed to record response event");
    }
}

async fn record_live_response_stream_event(
    context: &LiveResponseStreamContext,
    status_code: i64,
    level: EventLevel,
    message: &str,
    mut metadata: Value,
    rate_limit_headers: &[(String, String)],
    body: &str,
) {
    ensure_stream_metadata_flag(&mut metadata);
    enrich_live_response_stream_metadata(context, rate_limit_headers, &mut metadata, body);
    let mut event = EventLog::new("v1.response", level, message);
    event.request_id = Some(context.request_id.clone());
    event.account_id = Some(context.account_id.clone());
    event.route = Some("/v1/responses".to_string());
    event.model = Some(context.model.clone());
    event.status_code = Some(status_code);
    event.latency_ms = Some(elapsed_millis_i64(context.started_at));
    event.metadata = metadata;
    if let Err(error) = context.logs.record(event).await {
        tracing::warn!(account_id = %context.account_id, error = %error, "failed to record live response stream event");
    }
}

fn ensure_stream_metadata_flag(metadata: &mut Value) {
    let Some(object) = metadata.as_object_mut() else {
        *metadata = serde_json::json!({ "stream": true });
        return;
    };
    object
        .entry("stream".to_string())
        .or_insert(Value::Bool(true));
}

fn enrich_live_response_stream_metadata(
    context: &LiveResponseStreamContext,
    rate_limit_headers: &[(String, String)],
    metadata: &mut Value,
    body: &str,
) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    if context.transport == CodexBackendTransport::WebSocket {
        object
            .entry("transport".to_string())
            .or_insert_with(|| Value::String("websocket".to_string()));
    }
    if !rate_limit_headers.is_empty() {
        object
            .entry("rateLimitHeaders".to_string())
            .or_insert_with(|| serde_json::json!(rate_limit_headers));
    }
    object
        .entry("requestBody".to_string())
        .or_insert_with(|| serde_json::json!(context.request));
    object
        .entry("responseBody".to_string())
        .or_insert_with(|| Value::String(body.to_string()));
}

async fn live_response_rate_limit_headers(
    context: &LiveResponseStreamContext,
) -> Vec<(String, String)> {
    let mut headers = context.rate_limit_headers.clone();
    if let Some(updates) = &context.rate_limit_header_updates {
        headers.extend(updates.lock().await.iter().cloned());
    }
    headers
}

async fn live_response_turn_state(context: &LiveResponseStreamContext) -> Option<String> {
    if let Some(update) = &context.turn_state_update {
        return update.lock().await.clone();
    }
    context.turn_state.clone()
}

fn elapsed_millis_i64(started_at: Instant) -> i64 {
    started_at.elapsed().as_millis().min(i64::MAX as u128) as i64
}

fn bool_to_u64(value: bool) -> u64 {
    if value {
        1
    } else {
        0
    }
}

fn stream_failure_metadata(failure: &ResponsesSseFailure, usage: Option<TokenUsage>) -> Value {
    serde_json::json!({
        "stream": true,
        "failed": true,
        "failureEvent": failure.event,
        "failureMessage": failure.message,
        "upstreamCode": failure.upstream_code,
        "usage": usage,
    })
}

fn status_code_for_stream_failure(failure: &ResponsesSseFailure) -> i64 {
    let code = failure
        .upstream_code
        .as_deref()
        .unwrap_or("error")
        .to_ascii_lowercase();
    if code.contains("model") && (code.contains("not_supported") || code.contains("not_available"))
    {
        return 400;
    }
    if code.contains("invalid_request") || code.contains("not_found") {
        return 400;
    }
    if code.contains("rate_limit") || code.contains("usage_limit") {
        return 429;
    }
    if code.contains("unauthorized")
        || code.contains("invalid_api_key")
        || code == "token_invalid"
        || code == "token_expired"
        || code == "account_deactivated"
    {
        return 401;
    }
    if code.contains("forbidden") || code.contains("banned") {
        return 403;
    }
    if code.contains("payment") || code.contains("quota") {
        return 402;
    }
    502
}

/// Responses 调度错误。
#[derive(Debug, Error)]
pub enum ResponseDispatchError {
    /// 账号存储失败。
    #[error("failed to list runtime accounts")]
    AccountStore,
    /// 没有活跃账号。
    #[error("no active account is available")]
    NoActiveAccount,
    /// 所有账号都因配额耗尽不可用。
    #[error("all accounts exhausted by quota")]
    QuotaExhausted {
        /// 配额耗尽账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
    },
    /// 所有账号都因限流不可用。
    #[error("all accounts exhausted by rate limit")]
    RateLimited {
        /// 限流账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
    },
    /// 所有账号都因认证失效不可用。
    #[error("all accounts exhausted by expired auth")]
    Expired {
        /// 认证失效账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
    },
    /// 所有账号都因 Cloudflare challenge 暂不可用。
    #[error("all accounts exhausted by Cloudflare challenge")]
    CloudflareChallenge {
        /// Cloudflare challenge 账号数量。
        count: usize,
        /// 最后一个上游错误说明。
        upstream_error: String,
    },
    /// 所有账号都因 Cloudflare path-block 暂不可用。
    #[error("all accounts exhausted by Cloudflare path-block")]
    CloudflarePathBlocked {
        /// Cloudflare path-block 账号数量。
        count: usize,
        /// 最后一个上游错误说明。
        upstream_error: String,
    },
    /// 所有候选账号都不支持当前模型。
    #[error("all accounts exhausted by unsupported model")]
    ModelUnsupported {
        /// 模型不支持账号数量。
        count: usize,
        /// 最后一个上游错误体。
        upstream_error: String,
    },
    /// 上游请求失败。
    #[error("upstream request failed: {0}")]
    Upstream(#[from] CodexClientError),
    /// 上游 SSE 无法解析。
    #[error("invalid upstream SSE response: {0}")]
    InvalidSse(#[from] SseError),
    /// 上游响应没有完成事件。
    #[error("upstream response did not include response.completed")]
    MissingCompleted,
    /// 上游响应完成但没有可见输出。
    #[error("upstream response did not include visible output")]
    EmptyUpstreamResponse,
    /// 上游返回失败事件。
    #[error("upstream response failed: {0:?}")]
    Failed(ResponsesSseFailure),
}

/// Responses live SSE body stream error.
#[derive(Debug, Error)]
pub enum ResponseDispatchStreamError {
    /// 上游字节流读取失败。
    #[error("upstream stream failed: {0}")]
    Upstream(#[from] CodexClientError),
}

/// 管理端日志服务。
#[derive(Clone)]
pub struct AdminLogService {
    store: SqliteEventLogStore,
    settings: Arc<RwLock<AdminLogSettings>>,
}

#[derive(Debug, Clone, Copy)]
struct AdminLogSettings {
    enabled: bool,
    capacity: u32,
    capture_body: bool,
}

impl AdminLogService {
    /// 构造管理端日志服务。
    pub fn new(
        store: SqliteEventLogStore,
        enabled: bool,
        capacity: u32,
        capture_body: bool,
    ) -> Self {
        Self {
            store,
            settings: Arc::new(RwLock::new(AdminLogSettings {
                enabled,
                capacity,
                capture_body,
            })),
        }
    }

    /// 分页查询日志。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
        filter: AdminLogFilter,
    ) -> Result<Page<EventLog>, AdminLogError> {
        self.store
            .list(filter.into(), cursor, limit)
            .await
            .map_err(|_| AdminLogError::List)
    }

    /// 按 ID 读取日志。
    pub async fn get(&self, id: &str) -> Result<Option<EventLog>, AdminLogError> {
        self.store.get(id).await.map_err(|_| AdminLogError::Get)
    }

    /// 读取日志状态。
    pub async fn state(&self) -> Result<AdminLogState, AdminLogError> {
        let settings = *self.settings.read().await;
        Ok(AdminLogState {
            enabled: settings.enabled,
            capacity: settings.capacity,
            capture_body: settings.capture_body,
            stored_count: self.store.count().await.map_err(|_| AdminLogError::Count)?,
        })
    }

    /// 更新日志状态。
    pub async fn update_state(
        &self,
        update: AdminLogStateUpdate,
    ) -> Result<AdminLogState, AdminLogError> {
        if matches!(update.capacity, Some(0)) {
            return Err(AdminLogError::InvalidCapacity);
        }

        let trim_capacity = {
            let mut settings = self.settings.write().await;
            if let Some(enabled) = update.enabled {
                settings.enabled = enabled;
            }
            if let Some(capacity) = update.capacity {
                settings.capacity = capacity;
            }
            if let Some(capture_body) = update.capture_body {
                settings.capture_body = capture_body;
            }
            update.capacity
        };

        if let Some(capacity) = trim_capacity {
            self.store
                .trim_to_capacity(capacity)
                .await
                .map_err(|_| AdminLogError::Trim)?;
        }

        self.state().await
    }

    /// 清空日志。
    pub async fn clear(&self) -> Result<AdminClearLogs, AdminLogError> {
        self.store
            .clear()
            .await
            .map(|cleared| AdminClearLogs { cleared })
            .map_err(|_| AdminLogError::Clear)
    }

    async fn record(&self, mut event: EventLog) -> Result<(), AdminLogError> {
        let settings = *self.settings.read().await;
        let policy = EventLogService::new(settings.enabled);
        if !policy.should_record(&event) {
            return Ok(());
        }
        apply_capture_body_policy(&mut event, settings.capture_body);
        self.store
            .append(&event)
            .await
            .map_err(|_| AdminLogError::Append)?;
        self.store
            .trim_to_capacity(settings.capacity)
            .await
            .map_err(|_| AdminLogError::Trim)?;
        Ok(())
    }
}

fn apply_capture_body_policy(event: &mut EventLog, capture_body: bool) {
    if capture_body {
        return;
    }
    let Some(metadata) = event.metadata.as_object_mut() else {
        return;
    };
    for key in [
        "body",
        "rawBody",
        "requestBody",
        "responseBody",
        "upstreamBody",
    ] {
        metadata.remove(key);
    }
}

/// 日志查询过滤器。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminLogFilter {
    /// 事件类别。
    pub kind: Option<String>,
    /// 事件等级。
    pub level: Option<EventLevel>,
    /// 请求 ID。
    pub request_id: Option<String>,
    /// 账号 ID。
    pub account_id: Option<String>,
    /// 路由。
    pub route: Option<String>,
    /// 模型。
    pub model: Option<String>,
    /// HTTP 状态码。
    pub status_code: Option<i64>,
    /// 搜索关键词。
    pub search: Option<String>,
}

/// 日志状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLogState {
    /// 是否启用。
    pub enabled: bool,
    /// 内存容量。
    pub capacity: u32,
    /// 是否捕获请求体。
    pub capture_body: bool,
    /// 已存储数量。
    pub stored_count: u64,
}

/// 日志状态更新。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminLogStateUpdate {
    /// 是否启用。
    pub enabled: Option<bool>,
    /// 日志容量。
    pub capacity: Option<u32>,
    /// 是否捕获请求体。
    pub capture_body: Option<bool>,
}

/// 清空日志结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminClearLogs {
    /// 清理数量。
    pub cleared: u64,
}

/// 管理端日志错误。
#[derive(Debug, Error)]
pub enum AdminLogError {
    /// 列表失败。
    #[error("failed to list event logs")]
    List,
    /// 读取失败。
    #[error("failed to get event log")]
    Get,
    /// 计数失败。
    #[error("failed to count event logs")]
    Count,
    /// 清空失败。
    #[error("failed to clear event logs")]
    Clear,
    /// 写入失败。
    #[error("failed to append event log")]
    Append,
    /// 裁剪失败。
    #[error("failed to trim event logs")]
    Trim,
    /// 日志容量非法。
    #[error("log capacity must be greater than zero")]
    InvalidCapacity,
}

/// 管理端账号服务。
#[derive(Clone)]
pub struct AdminAccountService {
    store: SqliteAccountStore,
    cookies: SqliteCookieStore,
    quota_thresholds: QuotaWarningThresholds,
    codex: Arc<CodexBackendClient>,
    account_pool: Arc<RuntimeAccountPoolService>,
    token_refresher: Arc<dyn TokenRefresher>,
    installation_id: Option<String>,
}

impl AdminAccountService {
    /// 构造管理端账号服务。
    pub fn new(
        store: SqliteAccountStore,
        cookies: SqliteCookieStore,
        quota_thresholds: QuotaWarningThresholds,
        codex: Arc<CodexBackendClient>,
        account_pool: Arc<RuntimeAccountPoolService>,
        token_refresher: Arc<dyn TokenRefresher>,
        installation_id: Option<String>,
    ) -> Self {
        Self {
            store,
            cookies,
            quota_thresholds,
            codex,
            account_pool,
            token_refresher,
            installation_id,
        }
    }

    /// 分页列出账号元数据。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<AdminAccountMetadata>, AdminAccountError> {
        let page = self
            .store
            .list_metadata(cursor, limit)
            .await
            .map_err(|_| AdminAccountError::List)?;
        Ok(Page {
            items: page
                .items
                .into_iter()
                .map(AdminAccountMetadata::from)
                .collect(),
            next_cursor: page.next_cursor,
        })
    }

    /// 返回管理端认证状态摘要。
    pub async fn auth_status(&self) -> Result<AdminAuthStatus, AdminAccountError> {
        let mut cursor = None;
        let mut pool = AdminAuthPoolStatus::default();
        let mut user = None;
        loop {
            let page = self
                .store
                .list_metadata(cursor, 200)
                .await
                .map_err(|_| AdminAccountError::List)?;
            for account in page.items {
                pool.record(account.status);
                if user.is_none() && account.status == AccountStatus::Active {
                    user = Some(AdminAccountMetadata::from(account));
                }
            }
            if page.next_cursor.is_none() {
                break;
            }
            cursor = page.next_cursor;
        }

        Ok(AdminAuthStatus {
            authenticated: pool.total > 0,
            user,
            pool,
        })
    }

    /// 清空管理端 OAuth 登录账号。
    pub async fn logout(&self) -> Result<AdminAuthLogout, AdminAccountError> {
        let deleted = self
            .store
            .delete_all()
            .await
            .map_err(|_| AdminAccountError::Delete)?;
        self.account_pool.clear().await;
        Ok(AdminAuthLogout {
            success: true,
            deleted,
        })
    }

    /// 导出账号；包含可重新导入的 token，只应暴露给管理端会话。
    pub async fn export(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<AdminStoredAccount>, AdminAccountError> {
        if ids.is_empty() {
            let mut accounts = Vec::new();
            let mut cursor = None;
            loop {
                let page = self
                    .store
                    .list(cursor, 200)
                    .await
                    .map_err(|_| AdminAccountError::Export)?;
                accounts.extend(page.items.into_iter().map(AdminStoredAccount::from));
                if page.next_cursor.is_none() {
                    return Ok(accounts);
                }
                cursor = page.next_cursor;
            }
        }

        let mut accounts = Vec::with_capacity(ids.len());
        for id in ids {
            match self.store.get(&id).await {
                Ok(Some(account)) => accounts.push(AdminStoredAccount::from(account)),
                Ok(None) => {}
                Err(_) => return Err(AdminAccountError::Export),
            }
        }
        Ok(accounts)
    }

    /// 导入 native 账号导出数据。
    pub async fn import(&self, payload: &Value) -> Result<ImportedAccounts, AdminAccountError> {
        let entries = parse_account_import_payload(payload)?;
        if entries.is_empty() {
            return Err(AdminAccountError::NoImportableAccounts);
        }

        let mut imported = 0u32;
        let mut skipped = 0u32;
        for entry in entries {
            match self.import_entry(entry).await? {
                ImportedAccountState::Imported(account_id) => {
                    imported += 1;
                    self.sync_account_pool(&account_id).await?;
                }
                ImportedAccountState::Skipped => skipped += 1,
            }
        }

        Ok(ImportedAccounts { imported, skipped })
    }

    /// 手动创建或更新一个经 JWT claims 校验的账号。
    pub async fn create(
        &self,
        token: Option<String>,
        refresh_token: Option<String>,
    ) -> Result<AdminAccountMetadata, AdminAccountError> {
        let provided_refresh_token = normalize_nonempty(refresh_token);
        let tokens =
            if let Some(access_token) = normalize_nonempty(token.map(normalize_bearer_token)) {
                ManualCreateTokens {
                    access_token,
                    refresh_token_for_new: provided_refresh_token.clone(),
                    refresh_token_for_existing: provided_refresh_token,
                }
            } else if let Some(refresh_token) = provided_refresh_token {
                let token_pair = self
                    .token_refresher
                    .refresh(&refresh_token)
                    .await
                    .map_err(AdminAccountError::RefreshTokenExchange)?;
                let access_token =
                    normalize_nonempty(Some(normalize_bearer_token(token_pair.access_token)))
                        .ok_or(AdminAccountError::TokenRequired)?;
                ManualCreateTokens {
                    access_token,
                    refresh_token_for_new: token_pair
                        .refresh_token
                        .clone()
                        .or_else(|| Some(refresh_token.clone())),
                    refresh_token_for_existing: token_pair.refresh_token,
                }
            } else {
                return Err(AdminAccountError::TokenRequired);
            };

        let claims = manual_account_claims(&tokens.access_token, chrono::Utc::now())
            .map_err(AdminAccountError::InvalidToken)?;
        let existing = self
            .store
            .find_by_chatgpt_identity(&claims.account_id, claims.user_id.as_deref())
            .await
            .map_err(|_| AdminAccountError::Inspect)?;

        let account_id = if let Some(existing) = existing {
            let refresh_token = tokens.refresh_token_for_existing;
            let updated = self
                .store
                .update_from_claims(
                    &existing.id,
                    AccountClaimsUpdate {
                        email: claims.email.clone(),
                        account_id: Some(claims.account_id.clone()),
                        user_id: claims.user_id.clone(),
                        plan_type: claims.plan_type.clone(),
                        access_token: SecretString::new(tokens.access_token.into()),
                        refresh_token: refresh_token.map(|token| SecretString::new(token.into())),
                        access_token_expires_at: Some(claims.expires_at),
                        status: AccountStatus::Active,
                    },
                )
                .await
                .map_err(|_| AdminAccountError::UpdateClaims)?;
            if !updated {
                return Err(AdminAccountError::NotFound);
            }
            existing.id
        } else {
            let id = normalized_account_id(None);
            let refresh_token = tokens.refresh_token_for_new;
            self.store
                .insert(NewAccount {
                    id: id.clone(),
                    email: claims.email.clone(),
                    account_id: Some(claims.account_id.clone()),
                    user_id: claims.user_id.clone(),
                    label: None,
                    plan_type: claims.plan_type.clone(),
                    access_token: SecretString::new(tokens.access_token.into()),
                    refresh_token: refresh_token.map(|token| SecretString::new(token.into())),
                    access_token_expires_at: Some(claims.expires_at),
                    status: AccountStatus::Active,
                })
                .await
                .map_err(|_| AdminAccountError::Import)?;
            id
        };

        self.sync_account_pool(&account_id).await?;

        self.store
            .get(&account_id)
            .await
            .map_err(|_| AdminAccountError::Inspect)?
            .map(AdminAccountMetadata::from)
            .ok_or(AdminAccountError::NotFound)
    }

    /// 导入 Codex CLI 的 auth.json 内容。
    pub async fn import_codex_cli_auth(
        &self,
        payload: &Value,
    ) -> Result<AdminAccountMetadata, AdminAccountError> {
        let token = first_string(payload, &["access_token", "accessToken", "token"]);
        let refresh_token = first_string(payload, &["refresh_token", "refreshToken"]);
        if token.is_none() && refresh_token.is_none() {
            return Err(AdminAccountError::NoImportableAccounts);
        }
        self.create(token, refresh_token).await
    }

    async fn import_entry(
        &self,
        entry: AccountImportEntry,
    ) -> Result<ImportedAccountState, AdminAccountError> {
        let Some(access_token) = normalize_nonempty(entry.token) else {
            return Ok(ImportedAccountState::Skipped);
        };
        let id = normalized_account_id(entry.id);

        match self.store.get(&id).await {
            Ok(Some(_)) => return Ok(ImportedAccountState::Skipped),
            Ok(None) => {}
            Err(_) => return Err(AdminAccountError::Inspect),
        }

        let label = normalize_label(entry.label);
        if label
            .as_ref()
            .is_some_and(|label| label.chars().count() > 64)
        {
            return Err(AdminAccountError::LabelTooLong);
        }

        let access_token_expires_at = entry
            .access_token_expires_at
            .as_deref()
            .map(parse_account_import_datetime)
            .transpose()?;
        let status = parse_account_import_status(entry.status.as_deref())?;
        let account_id = id.clone();
        let account = NewAccount {
            id,
            email: normalize_nonempty(entry.email),
            account_id: normalize_nonempty(entry.account_id),
            user_id: normalize_nonempty(entry.user_id),
            label,
            plan_type: normalize_nonempty(entry.plan_type),
            access_token: SecretString::new(access_token.into()),
            refresh_token: normalize_nonempty(entry.refresh_token)
                .map(|token| SecretString::new(token.into())),
            access_token_expires_at,
            status,
        };
        self.store
            .insert(account)
            .await
            .map_err(|_| AdminAccountError::Import)?;

        Ok(ImportedAccountState::Imported(account_id))
    }

    /// 更新账号标签。
    pub async fn update_label(
        &self,
        account_id: &str,
        label: Option<String>,
    ) -> Result<bool, AdminAccountError> {
        if label
            .as_ref()
            .is_some_and(|label| label.chars().count() > 64)
        {
            return Err(AdminAccountError::LabelTooLong);
        }
        let updated = self
            .store
            .set_label(account_id, label)
            .await
            .map_err(|_| AdminAccountError::UpdateLabel)?;
        if updated {
            self.sync_account_pool_best_effort(account_id, "account label update")
                .await;
        }
        Ok(updated)
    }

    /// 更新账号状态。
    pub async fn update_status(
        &self,
        account_id: &str,
        status: &str,
    ) -> Result<Option<UpdatedAccountStatus>, AdminAccountError> {
        let status = parse_account_status(status)?;
        match self.store.set_status(account_id, status).await {
            Ok(true) => {
                self.sync_account_pool_best_effort(account_id, "account status update")
                    .await;
                self.evict_account_websocket_pool(account_id).await;
                Ok(Some(UpdatedAccountStatus {
                    id: account_id.to_string(),
                    status,
                }))
            }
            Ok(false) => Ok(None),
            Err(_) => Err(AdminAccountError::UpdateStatus),
        }
    }

    /// 删除账号。
    pub async fn delete(&self, account_id: &str) -> Result<bool, AdminAccountError> {
        let deleted = self
            .store
            .delete(account_id)
            .await
            .map_err(|_| AdminAccountError::Delete)?;
        if deleted {
            self.account_pool.remove_account(account_id).await;
        }
        Ok(deleted)
    }

    /// 批量删除账号。
    pub async fn batch_delete(
        &self,
        ids: Vec<String>,
    ) -> Result<BatchDeleteAccounts, AdminAccountError> {
        if ids.is_empty() {
            return Err(AdminAccountError::EmptyIds);
        }

        let mut deleted = 0u32;
        let mut not_found = Vec::new();
        for id in ids {
            match self.store.delete(&id).await {
                Ok(true) => {
                    deleted += 1;
                    self.account_pool.remove_account(&id).await;
                }
                Ok(false) => not_found.push(id),
                Err(_) => return Err(AdminAccountError::Delete),
            }
        }

        Ok(BatchDeleteAccounts { deleted, not_found })
    }

    /// 批量更新账号状态。
    pub async fn batch_update_status(
        &self,
        ids: Vec<String>,
        status: &str,
    ) -> Result<BatchUpdateAccountStatus, AdminAccountError> {
        if ids.is_empty() {
            return Err(AdminAccountError::EmptyIds);
        }
        let status = parse_batch_account_status(status)?;

        let mut updated = 0u32;
        let mut not_found = Vec::new();
        for id in ids {
            match self.store.set_status(&id, status).await {
                Ok(true) => {
                    updated += 1;
                    self.sync_account_pool_best_effort(&id, "account batch status update")
                        .await;
                    self.evict_account_websocket_pool(&id).await;
                }
                Ok(false) => not_found.push(id),
                Err(_) => return Err(AdminAccountError::UpdateStatus),
            }
        }

        Ok(BatchUpdateAccountStatus { updated, not_found })
    }

    /// 使用账号 refresh token 刷新 access token。
    pub async fn refresh_account(
        &self,
        account_id: &str,
    ) -> Result<AdminAccountRefresh, AdminAccountError> {
        let account = match self.store.get(account_id).await {
            Ok(Some(account)) => account,
            Ok(None) => return Err(AdminAccountError::NotFound),
            Err(_) => return Err(AdminAccountError::Inspect),
        };
        let previous_status = account.status;
        let Some(refresh_token) = account.refresh_token.as_ref() else {
            return Err(AdminAccountError::TokenRequired);
        };

        match self
            .token_refresher
            .refresh(refresh_token.expose_secret())
            .await
        {
            Ok(tokens) => {
                let access_token =
                    normalize_nonempty(Some(normalize_bearer_token(tokens.access_token)))
                        .ok_or(AdminAccountError::TokenRequired)?;
                let claims = manual_account_claims(&access_token, chrono::Utc::now())
                    .map_err(AdminAccountError::InvalidToken)?;
                let updated = self
                    .store
                    .update_from_claims(
                        account_id,
                        AccountClaimsUpdate {
                            email: claims.email,
                            account_id: Some(claims.account_id),
                            user_id: claims.user_id,
                            plan_type: claims.plan_type,
                            access_token: SecretString::new(access_token.into()),
                            refresh_token: tokens
                                .refresh_token
                                .map(|token| SecretString::new(token.into())),
                            access_token_expires_at: Some(claims.expires_at),
                            status: AccountStatus::Active,
                        },
                    )
                    .await
                    .map_err(|_| AdminAccountError::UpdateClaims)?;
                if !updated {
                    return Err(AdminAccountError::NotFound);
                }
                self.sync_account_pool(account_id).await?;
                Ok(AdminAccountRefresh {
                    id: account_id.to_string(),
                    previous_status,
                    outcome: AdminAccountProbeOutcome::Alive,
                    status: Some(AccountStatus::Active),
                    error: None,
                })
            }
            Err(failure) => {
                let status = refresh_failure_status(failure);
                let updated = self
                    .store
                    .set_status(account_id, status)
                    .await
                    .map_err(|_| AdminAccountError::UpdateStatus)?;
                if !updated {
                    return Err(AdminAccountError::NotFound);
                }
                self.sync_account_pool_best_effort(account_id, "account refresh failure")
                    .await;
                Ok(AdminAccountRefresh {
                    id: account_id.to_string(),
                    previous_status,
                    outcome: AdminAccountProbeOutcome::Dead,
                    status: Some(status),
                    error: Some(failure.to_string()),
                })
            }
        }
    }

    /// 重置账号本地用量计数。
    pub async fn reset_usage(
        &self,
        account_id: &str,
    ) -> Result<AdminAccountResetUsage, AdminAccountError> {
        match self.store.get(account_id).await {
            Ok(Some(_)) => {}
            Ok(None) => return Err(AdminAccountError::NotFound),
            Err(_) => return Err(AdminAccountError::Inspect),
        }

        self.store
            .reset_usage(account_id)
            .await
            .map_err(|_| AdminAccountError::ResetUsage)?;
        self.sync_account_pool(account_id).await?;

        Ok(AdminAccountResetUsage {
            id: account_id.to_string(),
            reset: true,
        })
    }

    /// 读取账号 Cookie 请求头。
    pub async fn cookies(&self, account_id: &str) -> Result<Option<String>, AdminAccountError> {
        self.ensure_cookie_account_exists(account_id).await?;
        self.cookies
            .cookie_header(account_id, "chatgpt.com")
            .await
            .map_err(|_| AdminAccountError::LoadCookies)
    }

    /// 设置账号 Cookie 请求头。
    pub async fn set_cookies(
        &self,
        account_id: &str,
        cookie_header: &str,
    ) -> Result<Option<String>, AdminAccountError> {
        self.ensure_cookie_account_exists(account_id).await?;
        match self
            .cookies
            .set_cookie_header(account_id, cookie_header)
            .await
        {
            Ok(0) => Err(AdminAccountError::NoValidCookies),
            Ok(_) => self
                .cookies
                .cookie_header(account_id, "chatgpt.com")
                .await
                .map_err(|_| AdminAccountError::LoadCookies),
            Err(_) => Err(AdminAccountError::StoreCookies),
        }
    }

    /// 删除账号 Cookie。
    pub async fn delete_cookies(&self, account_id: &str) -> Result<(), AdminAccountError> {
        self.ensure_cookie_account_exists(account_id).await?;
        self.cookies
            .delete_account_cookies(account_id)
            .await
            .map(|_| ())
            .map_err(|_| AdminAccountError::DeleteCookies)
    }

    /// 返回基于缓存配额快照的账号预警。
    pub async fn quota_warnings(&self) -> Result<AdminAccountQuotaWarnings, AdminAccountError> {
        let snapshots = self
            .store
            .list_quota_snapshots()
            .await
            .map_err(|_| AdminAccountError::QuotaWarnings)?;
        Ok(quota_warnings_from_snapshots(
            snapshots,
            &self.quota_thresholds,
        ))
    }

    /// 拉取并持久化单个账号的 Codex usage 配额快照。
    pub async fn account_quota(
        &self,
        account_id: &str,
        request_id: &str,
    ) -> Result<AdminAccountQuota, AdminAccountError> {
        let account = match self.store.get(account_id).await {
            Ok(Some(account)) => account,
            Ok(None) => return Err(AdminAccountError::NotFound),
            Err(_) => return Err(AdminAccountError::Inspect),
        };
        if account.status != AccountStatus::Active {
            return Err(AdminAccountError::Inactive(account.status));
        }

        let raw = self
            .codex
            .fetch_usage(CodexRequestContext {
                access_token: account.access_token.expose_secret(),
                account_id: account.account_id.as_deref(),
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
            .await
            .map_err(|error| AdminAccountError::FetchQuota(error.to_string()))?;
        let quota = quota_from_usage(&raw);
        let updated = self
            .store
            .update_quota_json(&account.id, &quota.to_string())
            .await
            .map_err(|_| AdminAccountError::StoreQuota)?;
        if !updated {
            return Err(AdminAccountError::NotFound);
        }

        Ok(AdminAccountQuota { quota, raw })
    }

    /// 对账号执行 refresh-token 健康探测。
    pub async fn health_check_accounts(
        &self,
        ids: Option<Vec<String>>,
        concurrency: usize,
        stagger_ms: u64,
        _request_id: &str,
    ) -> Result<Vec<AdminAccountProbeResult>, AdminAccountError> {
        let accounts = self.health_check_candidates(ids).await?;
        let concurrency = concurrency.max(1);
        let results = stream::iter(accounts.into_iter().enumerate())
            .map(|(index, account)| {
                let service = self.clone();
                async move {
                    if stagger_ms > 0 && index > 0 {
                        let multiplier = index.min(concurrency);
                        tokio::time::sleep(std::time::Duration::from_millis(
                            stagger_ms.saturating_mul(multiplier as u64),
                        ))
                        .await;
                    }
                    service.probe_account_refresh(account).await
                }
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;
        Ok(results)
    }

    async fn health_check_candidates(
        &self,
        ids: Option<Vec<String>>,
    ) -> Result<Vec<StoredAccount>, AdminAccountError> {
        if let Some(ids) = ids {
            let mut accounts = Vec::with_capacity(ids.len());
            for id in ids {
                match self.store.get(&id).await {
                    Ok(Some(account)) => accounts.push(account),
                    Ok(None) => {}
                    Err(_) => return Err(AdminAccountError::HealthCheck),
                }
            }
            return Ok(accounts);
        }

        let mut accounts = Vec::new();
        let mut cursor = None;
        loop {
            let page = self
                .store
                .list(cursor, 200)
                .await
                .map_err(|_| AdminAccountError::HealthCheck)?;
            accounts.extend(page.items);
            if page.next_cursor.is_none() {
                return Ok(accounts);
            }
            cursor = page.next_cursor;
        }
    }

    async fn probe_account_refresh(&self, account: StoredAccount) -> AdminAccountProbeResult {
        let started_at = Instant::now();
        let previous_status = account.status;
        if account.status == AccountStatus::Disabled {
            return skipped_admin_account_probe_result(account, "manually disabled");
        }
        let Some(refresh_token) = account.refresh_token.as_ref() else {
            return skipped_admin_account_probe_result(account, "no refresh token");
        };

        match self
            .token_refresher
            .refresh(refresh_token.expose_secret())
            .await
        {
            Ok(tokens) => {
                let Some(access_token) =
                    normalize_nonempty(Some(normalize_bearer_token(tokens.access_token)))
                else {
                    return dead_admin_account_probe_result(
                        account,
                        previous_status,
                        "token or refreshToken is required".to_string(),
                        started_at,
                    );
                };
                let claims = match manual_account_claims(&access_token, Utc::now()) {
                    Ok(claims) => claims,
                    Err(error) => {
                        return dead_admin_account_probe_result(
                            account,
                            previous_status,
                            error.to_string(),
                            started_at,
                        );
                    }
                };

                match self
                    .store
                    .update_from_claims(
                        &account.id,
                        AccountClaimsUpdate {
                            email: claims.email,
                            account_id: Some(claims.account_id),
                            user_id: claims.user_id,
                            plan_type: claims.plan_type,
                            access_token: SecretString::new(access_token.into()),
                            refresh_token: tokens
                                .refresh_token
                                .map(|token| SecretString::new(token.into())),
                            access_token_expires_at: Some(claims.expires_at),
                            status: AccountStatus::Active,
                        },
                    )
                    .await
                {
                    Ok(true) => {
                        self.sync_account_pool_best_effort(&account.id, "account health refresh")
                            .await;
                        AdminAccountProbeResult {
                            id: account.id,
                            email: account.email,
                            previous_status,
                            outcome: AdminAccountProbeOutcome::Alive,
                            status: Some(AccountStatus::Active),
                            error: None,
                            duration_ms: Some(started_at.elapsed().as_millis()),
                        }
                    }
                    Ok(false) => dead_admin_account_probe_result(
                        account,
                        previous_status,
                        AdminAccountError::NotFound.to_string(),
                        started_at,
                    ),
                    Err(_) => dead_admin_account_probe_result(
                        account,
                        previous_status,
                        AdminAccountError::UpdateClaims.to_string(),
                        started_at,
                    ),
                }
            }
            Err(failure) => {
                let status = health_check_failure_status(failure);
                if let Some(status) = status {
                    match self.store.set_status(&account.id, status).await {
                        Ok(true) => {
                            self.sync_account_pool_best_effort(
                                &account.id,
                                "account health refresh failure",
                            )
                            .await;
                        }
                        Ok(false) => {
                            return dead_admin_account_probe_result(
                                account,
                                previous_status,
                                AdminAccountError::NotFound.to_string(),
                                started_at,
                            );
                        }
                        Err(_) => {
                            return dead_admin_account_probe_result(
                                account,
                                previous_status,
                                AdminAccountError::UpdateStatus.to_string(),
                                started_at,
                            );
                        }
                    }
                }
                AdminAccountProbeResult {
                    id: account.id,
                    email: account.email,
                    previous_status,
                    outcome: AdminAccountProbeOutcome::Dead,
                    status,
                    error: Some(failure.to_string()),
                    duration_ms: Some(started_at.elapsed().as_millis()),
                }
            }
        }
    }

    async fn ensure_cookie_account_exists(
        &self,
        account_id: &str,
    ) -> Result<(), AdminAccountError> {
        match self.cookies.account_exists(account_id).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(AdminAccountError::NotFound),
            Err(_) => Err(AdminAccountError::Inspect),
        }
    }

    async fn sync_account_pool(&self, account_id: &str) -> Result<(), AdminAccountError> {
        self.account_pool
            .sync_account_from_repository(account_id)
            .await
            .map(|_| ())
            .map_err(|_| AdminAccountError::SyncAccountPool)
    }

    async fn sync_account_pool_best_effort(&self, account_id: &str, operation: &str) {
        if let Err(error) = self
            .account_pool
            .sync_account_from_repository(account_id)
            .await
        {
            tracing::warn!(
                account_id,
                operation,
                error = %error,
                "failed to sync runtime account pool after admin account update"
            );
        }
    }

    async fn evict_account_websocket_pool(&self, account_id: &str) {
        self.codex.evict_websocket_account(account_id).await;
        match self.store.get(account_id).await {
            Ok(Some(account)) => {
                if let Some(upstream_account_id) = account
                    .account_id
                    .as_deref()
                    .filter(|value| *value != account_id)
                {
                    self.codex
                        .evict_websocket_account(upstream_account_id)
                        .await;
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to inspect account while evicting websocket pool"
                );
            }
        }
    }
}

fn skipped_admin_account_probe_result(
    account: StoredAccount,
    error: &str,
) -> AdminAccountProbeResult {
    AdminAccountProbeResult {
        id: account.id,
        email: account.email,
        previous_status: account.status,
        outcome: AdminAccountProbeOutcome::Skipped,
        status: Some(account.status),
        error: Some(error.to_string()),
        duration_ms: None,
    }
}

fn dead_admin_account_probe_result(
    account: StoredAccount,
    previous_status: AccountStatus,
    error: String,
    started_at: Instant,
) -> AdminAccountProbeResult {
    AdminAccountProbeResult {
        id: account.id,
        email: account.email,
        previous_status,
        outcome: AdminAccountProbeOutcome::Dead,
        status: None,
        error: Some(error),
        duration_ms: Some(started_at.elapsed().as_millis()),
    }
}

fn health_check_failure_status(failure: RefreshFailure) -> Option<AccountStatus> {
    match failure {
        RefreshFailure::InvalidGrant => Some(AccountStatus::Expired),
        RefreshFailure::QuotaExhausted => Some(AccountStatus::QuotaExhausted),
        RefreshFailure::Banned => Some(AccountStatus::Banned),
        RefreshFailure::Disabled => Some(AccountStatus::Disabled),
        RefreshFailure::Transport => None,
    }
}

/// 管理端账号错误。
#[derive(Debug, Error)]
pub enum AdminAccountError {
    /// 列表失败。
    #[error("failed to list accounts")]
    List,
    /// 导出失败。
    #[error("failed to export accounts")]
    Export,
    /// 导入失败。
    #[error("failed to import accounts")]
    Import,
    /// 检查账号失败。
    #[error("failed to inspect account")]
    Inspect,
    /// 更新标签失败。
    #[error("failed to update account label")]
    UpdateLabel,
    /// 更新状态失败。
    #[error("failed to update account status")]
    UpdateStatus,
    /// 删除失败。
    #[error("failed to delete account")]
    Delete,
    /// 重置用量失败。
    #[error("failed to reset account usage")]
    ResetUsage,
    /// 账号不存在。
    #[error("account not found")]
    NotFound,
    /// 读取 Cookie 失败。
    #[error("failed to load account cookies")]
    LoadCookies,
    /// 写入 Cookie 失败。
    #[error("failed to store account cookies")]
    StoreCookies,
    /// 删除 Cookie 失败。
    #[error("failed to delete account cookies")]
    DeleteCookies,
    /// 根据 JWT claims 更新账号失败。
    #[error("failed to update account claims")]
    UpdateClaims,
    /// 读取配额预警失败。
    #[error("failed to load account quota warnings")]
    QuotaWarnings,
    /// 写入配额快照失败。
    #[error("failed to store account quota")]
    StoreQuota,
    /// 拉取配额失败。
    #[error("failed to fetch account quota: {0}")]
    FetchQuota(String),
    /// 健康检查失败。
    #[error("failed to health-check accounts")]
    HealthCheck,
    /// 账号非 active，不能执行需要上游访问的操作。
    #[error("account is {0:?}, cannot query quota")]
    Inactive(AccountStatus),
    /// token 为空。
    #[error("token or refreshToken is required")]
    TokenRequired,
    /// token 非法。
    #[error("{0}")]
    InvalidToken(&'static str),
    /// refresh token 换取 access token 失败。
    #[error("failed to exchange refreshToken: {0}")]
    RefreshTokenExchange(RefreshFailure),
    /// 同步运行时账号池失败。
    #[error("failed to sync runtime account pool")]
    SyncAccountPool,
    /// 没有有效 Cookie。
    #[error("No valid cookies found")]
    NoValidCookies,
    /// 标签过长。
    #[error("account label must be 64 characters or fewer")]
    LabelTooLong,
    /// 状态值无效。
    #[error("unsupported account status: {0}")]
    InvalidStatus(String),
    /// ID 列表为空。
    #[error("account ids are required")]
    EmptyIds,
    /// 没有可导入账号。
    #[error("No importable accounts found")]
    NoImportableAccounts,
    /// access token 过期时间非法。
    #[error("invalid accessTokenExpiresAt")]
    InvalidAccessTokenExpiresAt,
}

/// 管理端账号元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAccountMetadata {
    /// 账号 ID。
    pub id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// ChatGPT 账号 ID。
    pub account_id: Option<String>,
    /// ChatGPT 用户 ID。
    pub user_id: Option<String>,
    /// 管理标签。
    pub label: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// access token 过期时间。
    pub access_token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 账号状态。
    pub status: codex_proxy_core::accounts::model::AccountStatus,
    /// 创建时间。
    pub added_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 管理端账号配额拉取结果。
#[derive(Debug, Clone, PartialEq)]
pub struct AdminAccountQuota {
    /// 归一化后的配额快照。
    pub quota: Value,
    /// Codex usage 原始响应。
    pub raw: Value,
}

/// 管理端可导出的完整账号数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminStoredAccount {
    /// 账号 ID。
    pub id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// ChatGPT 账号 ID。
    pub account_id: Option<String>,
    /// ChatGPT 用户 ID。
    pub user_id: Option<String>,
    /// 管理标签。
    pub label: Option<String>,
    /// 计划类型。
    pub plan_type: Option<String>,
    /// access token 明文。
    pub access_token: String,
    /// refresh token 明文。
    pub refresh_token: Option<String>,
    /// access token 过期时间。
    pub access_token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 账号状态。
    pub status: codex_proxy_core::accounts::model::AccountStatus,
    /// 创建时间。
    pub added_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 管理端账号配额预警集合。
#[derive(Debug, Clone, PartialEq)]
pub struct AdminAccountQuotaWarnings {
    /// 预警列表。
    pub warnings: Vec<AdminAccountQuotaWarning>,
    /// 产生预警的快照中最新的拉取时间。
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// 管理端账号配额预警。
#[derive(Debug, Clone, PartialEq)]
pub struct AdminAccountQuotaWarning {
    /// 账号 ID。
    pub account_id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 配额窗口。
    pub window: AdminQuotaWarningWindow,
    /// 预警级别。
    pub level: AdminQuotaWarningLevel,
    /// 已使用百分比。
    pub used_percent: f64,
    /// 重置时间戳。
    pub reset_at: Option<i64>,
}

/// 配额预警窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminQuotaWarningWindow {
    /// 主窗口。
    Primary,
    /// 次窗口。
    Secondary,
}

impl AdminQuotaWarningWindow {
    /// 返回 API 字符串值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Secondary => "secondary",
        }
    }
}

/// 配额预警级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminQuotaWarningLevel {
    /// 普通预警。
    Warning,
    /// 临界预警。
    Critical,
}

impl AdminQuotaWarningLevel {
    /// 返回 API 字符串值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// 账号健康探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAccountProbeResult {
    /// 账号 ID。
    pub id: String,
    /// 邮箱。
    pub email: Option<String>,
    /// 探测前状态。
    pub previous_status: AccountStatus,
    /// 探测结果。
    pub outcome: AdminAccountProbeOutcome,
    /// 探测后的状态。
    pub status: Option<AccountStatus>,
    /// 错误信息。
    pub error: Option<String>,
    /// 耗时毫秒。
    pub duration_ms: Option<u128>,
}

/// 账号健康探测结果类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAccountProbeOutcome {
    /// 上游 usage 请求成功。
    Alive,
    /// 上游 usage 请求失败。
    Dead,
    /// 未执行上游探测。
    Skipped,
}

impl AdminAccountProbeOutcome {
    /// 返回 API 字符串值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::Dead => "dead",
            Self::Skipped => "skipped",
        }
    }
}

/// 账号导入结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportedAccounts {
    /// 成功导入数量。
    pub imported: u32,
    /// 跳过数量。
    pub skipped: u32,
}

/// 管理端认证状态摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAuthStatus {
    /// 是否存在已导入账号。
    pub authenticated: bool,
    /// 当前可展示的 active 账号元数据。
    pub user: Option<AdminAccountMetadata>,
    /// 账号池状态计数。
    pub pool: AdminAuthPoolStatus,
}

/// 管理端认证状态中的账号池计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdminAuthPoolStatus {
    /// 账号总数。
    pub total: u32,
    /// active 账号数。
    pub active: u32,
    /// expired 账号数。
    pub expired: u32,
    /// quota_exhausted 账号数。
    pub quota_exhausted: u32,
    /// refreshing 账号数。
    pub refreshing: u32,
    /// disabled 账号数。
    pub disabled: u32,
    /// banned 账号数。
    pub banned: u32,
}

impl AdminAuthPoolStatus {
    fn record(&mut self, status: AccountStatus) {
        self.total += 1;
        match status {
            AccountStatus::Active => self.active += 1,
            AccountStatus::Expired => self.expired += 1,
            AccountStatus::QuotaExhausted => self.quota_exhausted += 1,
            AccountStatus::Refreshing => self.refreshing += 1,
            AccountStatus::Disabled => self.disabled += 1,
            AccountStatus::Banned => self.banned += 1,
        }
    }
}

/// 管理端登出结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminAuthLogout {
    /// 是否成功。
    pub success: bool,
    /// 删除账号数。
    pub deleted: u64,
}

#[derive(Debug, Clone)]
struct ManualCreateTokens {
    access_token: String,
    refresh_token_for_new: Option<String>,
    refresh_token_for_existing: Option<String>,
}

#[derive(Debug, Clone)]
struct AccountImportEntry {
    id: Option<String>,
    email: Option<String>,
    account_id: Option<String>,
    user_id: Option<String>,
    label: Option<String>,
    plan_type: Option<String>,
    token: Option<String>,
    refresh_token: Option<String>,
    access_token_expires_at: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ImportedAccountState {
    Imported(String),
    Skipped,
}

/// 账号状态更新结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedAccountStatus {
    /// 账号 ID。
    pub id: String,
    /// 新状态。
    pub status: codex_proxy_core::accounts::model::AccountStatus,
}

/// 批量删除账号结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDeleteAccounts {
    /// 成功删除数量。
    pub deleted: u32,
    /// 未找到的账号 ID。
    pub not_found: Vec<String>,
}

/// 批量状态更新结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchUpdateAccountStatus {
    /// 成功更新数量。
    pub updated: u32,
    /// 未找到的账号 ID。
    pub not_found: Vec<String>,
}

/// 管理端手动刷新账号结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAccountRefresh {
    /// 账号 ID。
    pub id: String,
    /// 刷新前状态。
    pub previous_status: codex_proxy_core::accounts::model::AccountStatus,
    /// 刷新结果。
    pub outcome: AdminAccountProbeOutcome,
    /// 刷新后状态。
    pub status: Option<codex_proxy_core::accounts::model::AccountStatus>,
    /// 错误信息。
    pub error: Option<String>,
}

/// 管理端重置用量结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminAccountResetUsage {
    /// 账号 ID。
    pub id: String,
    /// 是否已处理。
    pub reset: bool,
}

/// 运行时设置服务。
#[derive(Clone)]
pub struct RuntimeSettingsService {
    current: Arc<StdRwLock<Arc<AppConfig>>>,
    local_config_path: Arc<PathBuf>,
}

impl RuntimeSettingsService {
    /// 构造运行时设置服务。
    pub fn new(config: AppConfig) -> Self {
        Self::with_local_config_path(config, "local.yaml")
    }

    /// 构造带本地配置覆盖路径的运行时设置服务。
    pub fn with_local_config_path(
        config: AppConfig,
        local_config_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            current: Arc::new(StdRwLock::new(Arc::new(config))),
            local_config_path: Arc::new(local_config_path.into()),
        }
    }

    /// 返回当前配置快照。
    pub fn current(&self) -> Arc<AppConfig> {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Return the local settings overlay path configured for this runtime.
    pub fn local_config_path(&self) -> Arc<PathBuf> {
        self.local_config_path.clone()
    }

    /// 更新当前设置并写入本地配置覆盖文件。
    pub async fn update(
        &self,
        patch: AdminSettingsPatch,
    ) -> Result<Arc<AppConfig>, RuntimeSettingsError> {
        let mut next = (*self.current()).clone();
        let mut settings = admin_settings_from_config(&next);
        SettingsService::apply_patch(&mut settings, patch)?;
        apply_admin_settings_to_config(&mut next, settings);
        next.write_settings_overlay(self.local_config_path.as_ref())
            .await?;
        let next = Arc::new(next);
        *self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next.clone();
        Ok(next)
    }
}

/// 运行时设置错误。
#[derive(Debug, Error)]
pub enum RuntimeSettingsError {
    /// 设置补丁验证失败。
    #[error(transparent)]
    InvalidField(#[from] SettingsServiceError),
    /// 本地配置覆盖写入失败。
    #[error(transparent)]
    Persist(#[from] ConfigWriteError),
}

fn admin_settings_from_config(config: &AppConfig) -> AdminSettings {
    AdminSettings {
        default_model: config.model.default_model.clone(),
        default_reasoning_effort: config.model.default_reasoning_effort.clone(),
        service_tier: config.model.service_tier.clone(),
        model_aliases: config.model.aliases.clone(),
        refresh_enabled: config.auth.refresh_enabled,
        refresh_margin_seconds: config.auth.refresh_margin_seconds,
        refresh_concurrency: config.auth.refresh_concurrency,
        max_concurrent_per_account: config.auth.max_concurrent_per_account,
        request_interval_ms: config.auth.request_interval_ms,
        rotation_strategy: config.auth.rotation_strategy.clone(),
        tier_priority: config.auth.tier_priority.clone(),
        quota_refresh_interval_minutes: config.quota.refresh_interval_minutes,
        quota_warning_thresholds: AdminQuotaWarningThresholds {
            primary: config.quota.warning_thresholds.primary.clone(),
            secondary: config.quota.warning_thresholds.secondary.clone(),
        },
        quota_skip_exhausted: config.quota.skip_exhausted,
        logs_enabled: config.logging.enabled,
        logs_capacity: config.logging.capacity,
        logs_capture_body: config.logging.capture_body,
        usage_history_retention_days: config.usage_stats.history_retention_days,
    }
}

fn apply_admin_settings_to_config(config: &mut AppConfig, settings: AdminSettings) {
    config.model.default_model = settings.default_model;
    config.model.default_reasoning_effort = settings.default_reasoning_effort;
    config.model.service_tier = settings.service_tier;
    config.model.aliases = settings.model_aliases;
    config.auth.refresh_enabled = settings.refresh_enabled;
    config.auth.refresh_margin_seconds = settings.refresh_margin_seconds;
    config.auth.refresh_concurrency = settings.refresh_concurrency;
    config.auth.max_concurrent_per_account = settings.max_concurrent_per_account;
    config.auth.request_interval_ms = settings.request_interval_ms;
    config.auth.rotation_strategy = settings.rotation_strategy;
    config.auth.tier_priority = settings.tier_priority;
    config.quota.refresh_interval_minutes = settings.quota_refresh_interval_minutes;
    config.quota.warning_thresholds = QuotaWarningThresholds {
        primary: settings.quota_warning_thresholds.primary,
        secondary: settings.quota_warning_thresholds.secondary,
    };
    config.quota.skip_exhausted = settings.quota_skip_exhausted;
    config.logging.enabled = settings.logs_enabled;
    config.logging.capacity = settings.logs_capacity;
    config.logging.capture_body = settings.logs_capture_body;
    config.usage_stats.history_retention_days = settings.usage_history_retention_days;
}

/// 管理员会话服务。
#[derive(Clone)]
pub struct AdminSessionService {
    store: SqliteAdminSessionStore,
    auth: AdminAuthService,
    default_username: String,
    session_ttl_minutes: u64,
}

impl AdminSessionService {
    /// 构造管理员会话服务。
    pub fn new(
        store: SqliteAdminSessionStore,
        default_username: String,
        session_ttl_minutes: u64,
    ) -> Self {
        Self {
            store,
            auth: AdminAuthService::new(default_username.clone()),
            default_username,
            session_ttl_minutes,
        }
    }

    /// 校验管理员会话是否存在且未过期。
    pub async fn validate(&self, session_id: Option<&str>) -> Result<bool, AdminSessionError> {
        let Some(session_id) = session_id else {
            return Ok(false);
        };
        self.store
            .validate_session(session_id)
            .await
            .map_err(|_| AdminSessionError::Validate)
    }

    /// 如果还没有管理员用户，则根据配置密码创建默认管理员。
    pub async fn ensure_default_admin(&self, password: &str) -> Result<bool, AdminSessionError> {
        let password_hash =
            hash_admin_password(password).map_err(|_| AdminSessionError::HashPassword)?;
        self.store
            .ensure_default_admin(&password_hash)
            .await
            .map_err(|_| AdminSessionError::CreateAdmin)
    }

    /// 使用管理员用户名和密码创建会话。
    pub async fn login(
        &self,
        username: Option<&str>,
        password: &str,
    ) -> Result<Option<AdminLoginSession>, AdminSessionError> {
        let username = username.unwrap_or(&self.default_username);
        if !self.auth.username_matches(username) {
            return Ok(None);
        }

        let Some(admin) = self
            .store
            .load_first_admin()
            .await
            .map_err(|_| AdminSessionError::LoadAdmin)?
        else {
            return Ok(None);
        };
        let password_matches = verify_admin_password(password, &admin.password_hash)
            .map_err(|_| AdminSessionError::VerifyPassword)?;
        if !password_matches {
            return Ok(None);
        }

        let session_id = format!("sess_{}", uuid::Uuid::new_v4().simple());
        let ttl_minutes = self.session_ttl_minutes.min(i64::MAX as u64) as i64;
        let expires_at = Utc::now() + Duration::minutes(ttl_minutes);
        self.store
            .create_session(&session_id, &admin.id, expires_at)
            .await
            .map_err(|_| AdminSessionError::CreateSession)?;

        Ok(Some(AdminLoginSession {
            session_id,
            expires_at,
        }))
    }
}

/// 管理员登录成功后的会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLoginSession {
    /// 会话 ID。
    pub session_id: String,
    /// 过期时间。
    pub expires_at: DateTime<Utc>,
}

/// 管理员会话错误。
#[derive(Debug, Error)]
pub enum AdminSessionError {
    /// 会话校验失败。
    #[error("failed to validate admin session")]
    Validate,
    /// 密码哈希失败。
    #[error("failed to hash admin password")]
    HashPassword,
    /// 创建管理员失败。
    #[error("failed to create default admin user")]
    CreateAdmin,
    /// 读取管理员失败。
    #[error("failed to load admin user")]
    LoadAdmin,
    /// 密码校验失败。
    #[error("failed to verify admin password")]
    VerifyPassword,
    /// 创建会话失败。
    #[error("failed to create admin session")]
    CreateSession,
}

/// 管理端客户端 API Key 服务。
#[derive(Clone)]
pub struct AdminClientKeyService {
    store: SqliteClientKeyStore,
}

impl AdminClientKeyService {
    /// 构造管理端客户端 API Key 服务。
    pub fn new(store: SqliteClientKeyStore) -> Self {
        Self { store }
    }

    /// 创建新的客户端 API Key。
    pub async fn create(
        &self,
        name: &str,
    ) -> Result<AdminCreatedClientApiKey, AdminClientKeyError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AdminClientKeyError::EmptyName);
        }
        self.store
            .create(name)
            .await
            .map(AdminCreatedClientApiKey::from)
            .map_err(|_| AdminClientKeyError::Create)
    }

    /// 分页列出客户端 API Key。
    pub async fn list(
        &self,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Page<AdminStoredClientApiKey>, AdminClientKeyError> {
        let page = self
            .store
            .list(cursor, limit)
            .await
            .map_err(|_| AdminClientKeyError::List)?;
        Ok(Page {
            items: page
                .items
                .into_iter()
                .map(AdminStoredClientApiKey::from)
                .collect(),
            next_cursor: page.next_cursor,
        })
    }

    /// 更新客户端 API Key 标签。
    pub async fn update_label(
        &self,
        key_id: &str,
        label: Option<String>,
    ) -> Result<Option<AdminStoredClientApiKey>, AdminClientKeyError> {
        if label
            .as_ref()
            .is_some_and(|label| label.chars().count() > 64)
        {
            return Err(AdminClientKeyError::LabelTooLong);
        }
        self.store
            .set_label(key_id, label)
            .await
            .map(|key| key.map(AdminStoredClientApiKey::from))
            .map_err(|_| AdminClientKeyError::UpdateLabel)
    }

    /// 更新客户端 API Key 启用状态。
    pub async fn update_status(
        &self,
        key_id: &str,
        status: &str,
    ) -> Result<Option<UpdatedClientApiKeyStatus>, AdminClientKeyError> {
        let enabled = parse_client_key_status(status)?;
        match self.store.set_enabled(key_id, enabled).await {
            Ok(true) => Ok(Some(UpdatedClientApiKeyStatus {
                id: key_id.to_string(),
                enabled,
            })),
            Ok(false) => Ok(None),
            Err(_) => Err(AdminClientKeyError::UpdateStatus),
        }
    }

    /// 删除客户端 API Key。
    pub async fn delete(&self, key_id: &str) -> Result<bool, AdminClientKeyError> {
        self.store
            .delete(key_id)
            .await
            .map_err(|_| AdminClientKeyError::Delete)
    }

    /// 批量删除客户端 API Key。
    pub async fn batch_delete(
        &self,
        ids: Vec<String>,
    ) -> Result<BatchDeleteClientApiKeys, AdminClientKeyError> {
        if ids.is_empty() {
            return Err(AdminClientKeyError::EmptyIds);
        }

        let mut deleted = 0u32;
        let mut not_found = Vec::new();
        for id in ids {
            match self.store.delete(&id).await {
                Ok(true) => deleted += 1,
                Ok(false) => not_found.push(id),
                Err(_) => return Err(AdminClientKeyError::Delete),
            }
        }

        Ok(BatchDeleteClientApiKeys { deleted, not_found })
    }

    /// 导出客户端 API Key 元数据，不包含明文和哈希材料。
    pub async fn export(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<AdminStoredClientApiKey>, AdminClientKeyError> {
        if ids.is_empty() {
            let mut all_keys = Vec::new();
            let mut cursor = None;
            loop {
                let page = self
                    .store
                    .list(cursor, 200)
                    .await
                    .map_err(|_| AdminClientKeyError::Export)?;
                all_keys.extend(page.items.into_iter().map(AdminStoredClientApiKey::from));
                if page.next_cursor.is_none() {
                    return Ok(all_keys);
                }
                cursor = page.next_cursor;
            }
        }

        let mut keys = Vec::with_capacity(ids.len());
        for id in ids {
            match self.store.get(&id).await {
                Ok(Some(key)) => keys.push(AdminStoredClientApiKey::from(key)),
                Ok(None) => {}
                Err(_) => return Err(AdminClientKeyError::Export),
            }
        }
        Ok(keys)
    }

    /// 导入导出的客户端 API Key 元数据，并轮换为新的本地明文。
    pub async fn import(
        &self,
        payload: &Value,
    ) -> Result<ImportedClientApiKeys, AdminClientKeyError> {
        let entries = parse_client_key_import_payload(payload);
        if entries.is_empty() {
            return Err(AdminClientKeyError::NoImportableKeys);
        }

        let mut imported = 0u32;
        let mut skipped = 0u32;
        let mut keys = Vec::with_capacity(entries.len());
        for entry in entries {
            let name = entry.name.trim();
            if name.is_empty() {
                skipped += 1;
                continue;
            }
            if entry
                .label
                .as_ref()
                .is_some_and(|label| label.chars().count() > 64)
            {
                return Err(AdminClientKeyError::LabelTooLong);
            }

            let mut created = self.create(name).await?;
            if entry.label.is_some() || !entry.enabled {
                self.store
                    .set_label(&created.id, entry.label)
                    .await
                    .map_err(|_| AdminClientKeyError::Import)?;
                if !entry.enabled {
                    self.store
                        .set_enabled(&created.id, false)
                        .await
                        .map_err(|_| AdminClientKeyError::Import)?;
                }
                let Some(stored) = self
                    .store
                    .get(&created.id)
                    .await
                    .map_err(|_| AdminClientKeyError::Import)?
                else {
                    return Err(AdminClientKeyError::Import);
                };
                created.label = stored.label;
                created.enabled = stored.enabled;
            }
            imported += 1;
            keys.push(ImportedClientApiKey {
                source_id: entry.source_id,
                source_prefix: entry.source_prefix,
                key: created,
            });
        }

        Ok(ImportedClientApiKeys {
            imported,
            skipped,
            keys,
        })
    }
}

/// 管理端可见的客户端 API Key 元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminStoredClientApiKey {
    /// API Key 记录 ID。
    pub id: String,
    /// API Key 名称。
    pub name: String,
    /// 管理员可见标签。
    pub label: Option<String>,
    /// 明文 API Key 的短前缀。
    pub prefix: String,
    /// 是否允许用于 `/v1` 认证。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: String,
    /// 最近一次成功使用时间。
    pub last_used_at: Option<String>,
}

/// 新建客户端 API Key 后的一次性结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminCreatedClientApiKey {
    /// API Key 记录 ID。
    pub id: String,
    /// API Key 名称。
    pub name: String,
    /// 管理员可见标签。
    pub label: Option<String>,
    /// 明文 API Key 的短前缀。
    pub prefix: String,
    /// 是否允许用于 `/v1` 认证。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: String,
    /// 最近一次成功使用时间。
    pub last_used_at: Option<String>,
    /// 仅返回一次的明文 API Key。
    pub plaintext: String,
}

/// 客户端 API Key 状态更新结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedClientApiKeyStatus {
    /// API Key 记录 ID。
    pub id: String,
    /// 是否启用。
    pub enabled: bool,
}

/// 批量删除客户端 API Key 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDeleteClientApiKeys {
    /// 成功删除数量。
    pub deleted: u32,
    /// 未找到的 ID。
    pub not_found: Vec<String>,
}

/// 导入后的客户端 API Key。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedClientApiKey {
    /// 来源 ID。
    pub source_id: Option<String>,
    /// 来源短前缀。
    pub source_prefix: Option<String>,
    /// 新建的本地 API Key。
    pub key: AdminCreatedClientApiKey,
}

/// 客户端 API Key 导入结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedClientApiKeys {
    /// 成功导入数量。
    pub imported: u32,
    /// 跳过数量。
    pub skipped: u32,
    /// 新建 API Key 列表。
    pub keys: Vec<ImportedClientApiKey>,
}

/// 管理端客户端 API Key 错误。
#[derive(Debug, Error)]
pub enum AdminClientKeyError {
    /// 列表失败。
    #[error("failed to list client API keys")]
    List,
    /// 导出失败。
    #[error("failed to export client API keys")]
    Export,
    /// 导入失败。
    #[error("failed to import client API keys")]
    Import,
    /// 创建失败。
    #[error("failed to create client API key")]
    Create,
    /// 删除失败。
    #[error("failed to delete client API key")]
    Delete,
    /// 更新标签失败。
    #[error("failed to update client API key label")]
    UpdateLabel,
    /// 更新状态失败。
    #[error("failed to update client API key status")]
    UpdateStatus,
    /// 状态值无效。
    #[error("unsupported client API key status: {0}")]
    InvalidStatus(String),
    /// 名称为空。
    #[error("client API key name is required")]
    EmptyName,
    /// ID 列表为空。
    #[error("client API key ids are required")]
    EmptyIds,
    /// 标签过长。
    #[error("client API key label must be 64 characters or fewer")]
    LabelTooLong,
    /// 没有可导入的 API Key。
    #[error("no importable client API keys found")]
    NoImportableKeys,
}

#[derive(Debug, Clone)]
struct ClientApiKeyImportEntry {
    source_id: Option<String>,
    source_prefix: Option<String>,
    name: String,
    label: Option<String>,
    enabled: bool,
}

fn parse_client_key_status(status: &str) -> Result<bool, AdminClientKeyError> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(true),
        "disabled" => Ok(false),
        other => Err(AdminClientKeyError::InvalidStatus(other.to_string())),
    }
}

fn parse_account_status(
    status: &str,
) -> Result<codex_proxy_core::accounts::model::AccountStatus, AdminAccountError> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(codex_proxy_core::accounts::model::AccountStatus::Active),
        "disabled" => Ok(codex_proxy_core::accounts::model::AccountStatus::Disabled),
        "expired" => Ok(codex_proxy_core::accounts::model::AccountStatus::Expired),
        "quota_exhausted" => Ok(codex_proxy_core::accounts::model::AccountStatus::QuotaExhausted),
        "refreshing" => Ok(codex_proxy_core::accounts::model::AccountStatus::Refreshing),
        "banned" => Ok(codex_proxy_core::accounts::model::AccountStatus::Banned),
        other => Err(AdminAccountError::InvalidStatus(other.to_string())),
    }
}

fn parse_batch_account_status(
    status: &str,
) -> Result<codex_proxy_core::accounts::model::AccountStatus, AdminAccountError> {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" => Ok(codex_proxy_core::accounts::model::AccountStatus::Active),
        "disabled" => Ok(codex_proxy_core::accounts::model::AccountStatus::Disabled),
        other => Err(AdminAccountError::InvalidStatus(other.to_string())),
    }
}

fn refresh_failure_status(
    failure: RefreshFailure,
) -> codex_proxy_core::accounts::model::AccountStatus {
    match failure {
        RefreshFailure::InvalidGrant => codex_proxy_core::accounts::model::AccountStatus::Expired,
        RefreshFailure::QuotaExhausted => {
            codex_proxy_core::accounts::model::AccountStatus::QuotaExhausted
        }
        RefreshFailure::Banned => codex_proxy_core::accounts::model::AccountStatus::Banned,
        RefreshFailure::Disabled => codex_proxy_core::accounts::model::AccountStatus::Disabled,
        RefreshFailure::Transport => codex_proxy_core::accounts::model::AccountStatus::Active,
    }
}

const ACCOUNT_IMPORT_ENVELOPE_KEYS: &[&str] =
    &["code", "message", "data", "requestId", "request_id"];
const ACCOUNT_IMPORT_CONTAINER_KEYS: &[&str] = &["sourceFormat", "source_format", "accounts"];
const ACCOUNT_IMPORT_ACCOUNT_KEYS: &[&str] = &[
    "id",
    "email",
    "accountId",
    "account_id",
    "userId",
    "user_id",
    "label",
    "planType",
    "plan_type",
    "token",
    "accessToken",
    "access_token",
    "refreshToken",
    "refresh_token",
    "accessTokenExpiresAt",
    "access_token_expires_at",
    "status",
    "addedAt",
    "added_at",
    "updatedAt",
    "updated_at",
];

fn parse_account_import_payload(
    payload: &Value,
) -> Result<Vec<AccountImportEntry>, AdminAccountError> {
    let payload = payload
        .get("data")
        .filter(|data| data.get("accounts").is_some())
        .map(|data| {
            ensure_account_import_keys(payload, ACCOUNT_IMPORT_ENVELOPE_KEYS)?;
            Ok(data)
        })
        .transpose()?
        .unwrap_or(payload);

    if let Some(accounts) = payload.get("accounts") {
        ensure_account_import_keys(payload, ACCOUNT_IMPORT_CONTAINER_KEYS)?;
        ensure_native_account_import_source(payload)?;
        let accounts = accounts
            .as_array()
            .ok_or(AdminAccountError::NoImportableAccounts)?;
        return parse_account_import_entries(accounts);
    }
    if let Some(accounts) = payload.as_array() {
        return parse_account_import_entries(accounts);
    }

    Ok(account_import_entry_from_value(payload)?
        .into_iter()
        .collect())
}

fn parse_account_import_entries(
    accounts: &[Value],
) -> Result<Vec<AccountImportEntry>, AdminAccountError> {
    let mut entries = Vec::new();
    for account in accounts {
        if let Some(entry) = account_import_entry_from_value(account)? {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn account_import_entry_from_value(
    value: &Value,
) -> Result<Option<AccountImportEntry>, AdminAccountError> {
    let Some(account) = value.as_object() else {
        return Ok(None);
    };
    if account
        .keys()
        .any(|key| !ACCOUNT_IMPORT_ACCOUNT_KEYS.contains(&key.as_str()))
    {
        return Err(AdminAccountError::NoImportableAccounts);
    }

    let token = first_string(value, &["token", "accessToken", "access_token"]);
    let refresh_token = first_string(value, &["refreshToken", "refresh_token"]);
    if token.is_none() && refresh_token.is_none() {
        return Ok(None);
    }

    Ok(Some(AccountImportEntry {
        id: first_string(value, &["id"]),
        email: first_string(value, &["email"]),
        account_id: first_string(value, &["accountId", "account_id"]),
        user_id: first_string(value, &["userId", "user_id"]),
        label: first_string(value, &["label"]),
        plan_type: first_string(value, &["planType", "plan_type"]),
        token,
        refresh_token,
        access_token_expires_at: first_string(
            value,
            &["accessTokenExpiresAt", "access_token_expires_at"],
        ),
        status: first_string(value, &["status"]),
    }))
}

fn ensure_account_import_keys(
    value: &Value,
    allowed_keys: &[&str],
) -> Result<(), AdminAccountError> {
    let Some(object) = value.as_object() else {
        return Err(AdminAccountError::NoImportableAccounts);
    };
    if object
        .keys()
        .all(|key| allowed_keys.contains(&key.as_str()))
    {
        Ok(())
    } else {
        Err(AdminAccountError::NoImportableAccounts)
    }
}

fn ensure_native_account_import_source(value: &Value) -> Result<(), AdminAccountError> {
    if let Some(source_format) = first_string(value, &["sourceFormat", "source_format"]) {
        if !source_format.eq_ignore_ascii_case("native") {
            return Err(AdminAccountError::NoImportableAccounts);
        }
    }
    Ok(())
}

fn parse_account_import_status(
    status: Option<&str>,
) -> Result<codex_proxy_core::accounts::model::AccountStatus, AdminAccountError> {
    parse_account_status(status.unwrap_or("active"))
}

fn parse_account_import_datetime(
    value: &str,
) -> Result<chrono::DateTime<chrono::Utc>, AdminAccountError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&chrono::Utc))
        .map_err(|_| AdminAccountError::InvalidAccessTokenExpiresAt)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManualAccountClaims {
    account_id: String,
    user_id: Option<String>,
    email: Option<String>,
    plan_type: Option<String>,
    expires_at: chrono::DateTime<chrono::Utc>,
}

fn manual_account_claims(
    token: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<ManualAccountClaims, &'static str> {
    let payload = decode_jwt_payload(token).ok_or("Invalid JWT format")?;
    let exp = payload
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or("Token is expired")?;
    if now.timestamp() >= exp {
        return Err("Token is expired");
    }
    let expires_at =
        chrono::DateTime::<chrono::Utc>::from_timestamp(exp, 0).ok_or("Invalid JWT exp claim")?;
    let auth = payload
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object)
        .ok_or("Token missing chatgpt_account_id claim")?;
    let account_id =
        string_claim(auth, "chatgpt_account_id").ok_or("Token missing chatgpt_account_id claim")?;
    let profile = payload
        .get("https://api.openai.com/profile")
        .and_then(Value::as_object);
    let user_id = string_claim(auth, "chatgpt_user_id")
        .or_else(|| profile.and_then(|profile| string_claim(profile, "chatgpt_user_id")));
    let plan_type = string_claim(auth, "chatgpt_plan_type")
        .or_else(|| profile.and_then(|profile| string_claim(profile, "chatgpt_plan_type")));
    let email = profile.and_then(|profile| string_claim(profile, "email"));

    Ok(ManualAccountClaims {
        account_id,
        user_id,
        email,
        plan_type,
        expires_at,
    })
}

fn decode_jwt_payload(token: &str) -> Option<Map<String, Value>> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    if payload.is_empty() {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<Value>(&bytes)
        .ok()?
        .as_object()
        .cloned()
}

fn string_claim(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn normalize_bearer_token(value: String) -> String {
    value
        .trim()
        .strip_prefix("Bearer ")
        .or_else(|| value.trim().strip_prefix("bearer "))
        .unwrap_or(value.trim())
        .trim()
        .to_string()
}

fn normalized_account_id(id: Option<String>) -> String {
    normalize_nonempty(id).unwrap_or_else(|| format!("acct_{}", uuid::Uuid::new_v4().simple()))
}

fn normalize_label(value: Option<String>) -> Option<String> {
    normalize_nonempty(value)
}

fn normalize_nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_client_key_import_payload(payload: &Value) -> Vec<ClientApiKeyImportEntry> {
    let payload = payload
        .get("data")
        .filter(|data| data.get("apiKeys").is_some() || data.get("keys").is_some())
        .unwrap_or(payload);

    if let Some(keys) = payload.get("apiKeys").and_then(Value::as_array) {
        return keys
            .iter()
            .filter_map(client_key_import_entry_from_value)
            .collect();
    }
    if let Some(keys) = payload.get("keys").and_then(Value::as_array) {
        return keys
            .iter()
            .filter_map(client_key_import_entry_from_value)
            .collect();
    }
    if let Some(keys) = payload.as_array() {
        return keys
            .iter()
            .filter_map(client_key_import_entry_from_value)
            .collect();
    }

    client_key_import_entry_from_value(payload)
        .into_iter()
        .collect()
}

fn client_key_import_entry_from_value(value: &Value) -> Option<ClientApiKeyImportEntry> {
    value.as_object()?;
    let name = first_string(value, &["name"])?;
    Some(ClientApiKeyImportEntry {
        source_id: first_string(value, &["id", "sourceId"]),
        source_prefix: first_string(value, &["prefix", "sourcePrefix"]),
        name,
        label: first_string(value, &["label"]),
        enabled: client_key_import_enabled(value),
    })
}

fn client_key_import_enabled(value: &Value) -> bool {
    if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
        return enabled;
    }
    !first_string(value, &["status"])
        .unwrap_or_else(|| "active".to_string())
        .trim()
        .eq_ignore_ascii_case("disabled")
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn quota_warnings_from_snapshots(
    snapshots: Vec<AccountQuotaSnapshot>,
    thresholds: &QuotaWarningThresholds,
) -> AdminAccountQuotaWarnings {
    let primary_thresholds = sorted_thresholds(&thresholds.primary);
    let secondary_thresholds = sorted_thresholds(&thresholds.secondary);
    let mut warnings = Vec::new();
    let mut updated_at = None;

    for snapshot in snapshots {
        let Ok(quota) = serde_json::from_str::<Value>(&snapshot.quota_json) else {
            continue;
        };
        let before_len = warnings.len();
        if let Some(warning) = warning_from_quota_window(
            &snapshot.account_id,
            snapshot.email.as_deref(),
            &quota,
            "rate_limit",
            AdminQuotaWarningWindow::Primary,
            &primary_thresholds,
        ) {
            warnings.push(warning);
        }
        if let Some(warning) = warning_from_quota_window(
            &snapshot.account_id,
            snapshot.email.as_deref(),
            &quota,
            "secondary_rate_limit",
            AdminQuotaWarningWindow::Secondary,
            &secondary_thresholds,
        ) {
            warnings.push(warning);
        }
        if warnings.len() > before_len {
            updated_at = max_optional_datetime(updated_at, snapshot.quota_fetched_at);
        }
    }

    AdminAccountQuotaWarnings {
        warnings,
        updated_at,
    }
}

fn warning_from_quota_window(
    account_id: &str,
    email: Option<&str>,
    quota: &Value,
    field: &str,
    window: AdminQuotaWarningWindow,
    thresholds: &[u8],
) -> Option<AdminAccountQuotaWarning> {
    let quota_window = quota.get(field).filter(|value| !value.is_null())?;
    let used_percent = quota_window
        .get("used_percent")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())?;
    let level = warning_level(used_percent, thresholds)?;

    Some(AdminAccountQuotaWarning {
        account_id: account_id.to_string(),
        email: email.map(ToString::to_string),
        window,
        level,
        used_percent,
        reset_at: quota_window.get("reset_at").and_then(Value::as_i64),
    })
}

fn warning_level(used_percent: f64, thresholds: &[u8]) -> Option<AdminQuotaWarningLevel> {
    let matched_index = thresholds
        .iter()
        .rposition(|threshold| quota_reached(used_percent, f64::from(*threshold)))?;
    if matched_index + 1 == thresholds.len() {
        Some(AdminQuotaWarningLevel::Critical)
    } else {
        Some(AdminQuotaWarningLevel::Warning)
    }
}

fn sorted_thresholds(thresholds: &[u8]) -> Vec<u8> {
    let mut thresholds = thresholds.to_vec();
    thresholds.sort_unstable();
    thresholds.dedup();
    thresholds
}

fn max_optional_datetime(
    current: Option<chrono::DateTime<chrono::Utc>>,
    candidate: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.max(candidate)),
        (Some(current), None) => Some(current),
        (None, Some(candidate)) => Some(candidate),
        (None, None) => None,
    }
}

impl From<StoredAccount> for AdminStoredAccount {
    fn from(account: StoredAccount) -> Self {
        Self {
            id: account.id,
            email: account.email,
            account_id: account.account_id,
            user_id: account.user_id,
            label: account.label,
            plan_type: account.plan_type,
            access_token: account.access_token.expose_secret().to_string(),
            refresh_token: account
                .refresh_token
                .map(|token| token.expose_secret().to_string()),
            access_token_expires_at: account.access_token_expires_at,
            status: account.status,
            added_at: account.added_at,
            updated_at: account.updated_at,
        }
    }
}

impl From<StoredAccount> for AdminAccountMetadata {
    fn from(account: StoredAccount) -> Self {
        Self {
            id: account.id,
            email: account.email,
            account_id: account.account_id,
            user_id: account.user_id,
            label: account.label,
            plan_type: account.plan_type,
            access_token_expires_at: account.access_token_expires_at,
            status: account.status,
            added_at: account.added_at,
            updated_at: account.updated_at,
        }
    }
}

impl From<StoredClientApiKey> for AdminStoredClientApiKey {
    fn from(key: StoredClientApiKey) -> Self {
        Self {
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
        }
    }
}

impl From<CreatedClientApiKey> for AdminCreatedClientApiKey {
    fn from(key: CreatedClientApiKey) -> Self {
        Self {
            id: key.id,
            name: key.name,
            label: key.label,
            prefix: key.prefix,
            enabled: key.enabled,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
            plaintext: key.plaintext,
        }
    }
}

impl From<StoredAccountMetadata> for AdminAccountMetadata {
    fn from(account: StoredAccountMetadata) -> Self {
        Self {
            id: account.id,
            email: account.email,
            account_id: account.account_id,
            user_id: account.user_id,
            label: account.label,
            plan_type: account.plan_type,
            access_token_expires_at: account.access_token_expires_at,
            status: account.status,
            added_at: account.added_at,
            updated_at: account.updated_at,
        }
    }
}

impl From<AccountUsageListRecord> for AdminUsageRecord {
    fn from(usage: AccountUsageListRecord) -> Self {
        Self {
            account_id: usage.account_id,
            email: usage.email,
            label: usage.label,
            plan_type: usage.plan_type,
            request_count: usage.request_count,
            empty_response_count: usage.empty_response_count,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cached_tokens,
            image_input_tokens: usage.image_input_tokens,
            image_output_tokens: usage.image_output_tokens,
            image_request_count: usage.image_request_count,
            image_request_failed_count: usage.image_request_failed_count,
            last_used_at: usage.last_used_at,
        }
    }
}

impl From<AccountUsageSummary> for AdminUsageSummary {
    fn from(summary: AccountUsageSummary) -> Self {
        Self {
            account_count: summary.account_count,
            request_count: summary.request_count,
            empty_response_count: summary.empty_response_count,
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            cached_tokens: summary.cached_tokens,
            image_input_tokens: summary.image_input_tokens,
            image_output_tokens: summary.image_output_tokens,
            image_request_count: summary.image_request_count,
            image_request_failed_count: summary.image_request_failed_count,
        }
    }
}

impl From<AdminLogFilter> for EventLogFilter {
    fn from(filter: AdminLogFilter) -> Self {
        Self {
            kind: filter.kind,
            level: filter.level,
            request_id: filter.request_id,
            account_id: filter.account_id,
            route: filter.route,
            model: filter.model,
            status_code: filter.status_code,
            search: filter.search,
        }
    }
}
