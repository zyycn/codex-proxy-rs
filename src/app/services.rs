//! 应用服务集合 —— 定义 Services 结构及其依赖的所有服务类型。

use std::{path::PathBuf, sync::Arc, time::Duration as StdDuration};

use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;

use crate::{
    access::{
        admin_session::{AdminAuthService, SqliteAdminSessionStore},
        client_keys::{
            ClientKeyService, CreatedClientApiKey, SqliteClientKeyStore, StoredClientApiKey,
        },
    },
    accounts::{
        model::{Account, AccountStatus, AccountUsageDelta},
        pool::{
            AccountAcquireRequest, AccountCapacitySummary, AccountPool, AccountPoolOptions,
            AccountWindowUsageDelta,
        },
        quota::{quota_snapshot_limit_reached, quota_snapshot_reset_at},
        store::{
            AccountUsageListRecord, AccountUsageSummary, SqliteAccountStore, SqliteCookieStore,
            StoredAccountMetadata, UsageSummary,
        },
        token_refresh::RefreshLeaseStore as SqliteRefreshLeaseStore,
    },
    codex::{
        fingerprint::{Fingerprint, FingerprintRepository},
        models::{ModelRefreshResult, ModelService, ModelServiceError, ModelSnapshotStore},
        oauth_client::default_openai_oauth_client,
        protocol::events::{parse_rate_limit_headers, rate_limit_quota, TokenUsage},
        transport::{CodexBackendClient, CodexClientError, CodexModelCatalogClient},
    },
    config::{AppConfig, ConfigWriteError, QuotaWarningThresholds},
    gateway::dispatch::{
        chat::ChatDispatchService,
        responses::ResponseDispatchService,
        session_affinity::{RuntimeSessionAffinityService, SqliteSessionAffinityStore},
    },
    infra::{
        identity::{hash_admin_password, verify_admin_password},
        json::Page,
    },
    telemetry::{
        event_store::{EventLogFilter, SqliteEventLogStore},
        events::{EventLevel, EventLog},
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
// RuntimeSettingsService
// ============================================================================

use std::sync::RwLock as StdRwLock;

use crate::admin::settings_domain::{
    AdminQuotaWarningThresholds as AdminQWT, AdminSettings, AdminSettingsPatch,
};
/// 运行时设置服务。
#[derive(Clone)]
pub struct RuntimeSettingsService {
    current: Arc<StdRwLock<Arc<AppConfig>>>,
    config_path: Arc<PathBuf>,
}

impl RuntimeSettingsService {
    pub fn new(config: AppConfig) -> Self {
        Self::with_config_path(config, "config.yaml")
    }

    pub fn with_config_path(config: AppConfig, config_path: impl Into<PathBuf>) -> Self {
        Self {
            current: Arc::new(StdRwLock::new(Arc::new(config))),
            config_path: Arc::new(config_path.into()),
        }
    }

    pub fn current(&self) -> Arc<AppConfig> {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn config_path(&self) -> Arc<PathBuf> {
        self.config_path.clone()
    }

    pub async fn update(
        &self,
        patch: AdminSettingsPatch,
    ) -> Result<Arc<AppConfig>, RuntimeSettingsError> {
        let mut next = (*self.current()).clone();
        let mut settings = admin_settings_from_config(&next);
        crate::admin::settings_domain::SettingsService::apply_patch(&mut settings, patch)?;
        apply_admin_settings_to_config(&mut next, settings);
        next.write_settings_config(self.config_path.as_ref())
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
    #[error(transparent)]
    InvalidField(#[from] crate::admin::settings_domain::SettingsServiceError),
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
        quota_warning_thresholds: AdminQWT {
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

// ============================================================================
// AdminSessionService
// ============================================================================

use chrono::Duration;
use uuid::Uuid;

/// 管理员登录成功后的会话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLoginSession {
    pub session_id: String,
    pub expires_at: DateTime<Utc>,
}

/// 管理员会话错误。
#[derive(Debug, Error)]
pub enum AdminSessionError {
    #[error("failed to validate admin session")]
    Validate,
    #[error("failed to hash admin password")]
    HashPassword,
    #[error("failed to create default admin user")]
    CreateAdmin,
    #[error("failed to load admin user")]
    LoadAdmin,
    #[error("failed to verify admin password")]
    VerifyPassword,
    #[error("failed to create admin session")]
    CreateSession,
    #[error("failed to delete admin session")]
    DeleteSession,
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

    pub async fn validate(&self, session_id: Option<&str>) -> Result<bool, AdminSessionError> {
        let Some(session_id) = session_id else {
            return Ok(false);
        };
        self.store
            .validate_session(session_id)
            .await
            .map_err(|_| AdminSessionError::Validate)
    }

    pub async fn ensure_default_admin(&self, password: &str) -> Result<bool, AdminSessionError> {
        let password_hash =
            hash_admin_password(password).map_err(|_| AdminSessionError::HashPassword)?;
        self.store
            .ensure_default_admin(&password_hash)
            .await
            .map_err(|_| AdminSessionError::CreateAdmin)
    }

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
        let session_id = format!("sess_{}", Uuid::new_v4().simple());
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

    pub async fn delete_session(&self, session_id: &str) -> Result<bool, AdminSessionError> {
        self.store
            .delete_session(session_id)
            .await
            .map_err(|_| AdminSessionError::DeleteSession)
    }
}

// ============================================================================
// AdminClientKeyService
// ============================================================================

/// 管理端客户端 API Key 服务。
#[derive(Clone)]
pub struct AdminClientKeyService {
    store: SqliteClientKeyStore,
}

impl AdminClientKeyService {
    pub fn new(store: SqliteClientKeyStore) -> Self {
        Self { store }
    }

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

    pub async fn update_label(
        &self,
        key_id: &str,
        label: Option<String>,
    ) -> Result<Option<AdminStoredClientApiKey>, AdminClientKeyError> {
        if label.as_ref().is_some_and(|l| l.chars().count() > 64) {
            return Err(AdminClientKeyError::LabelTooLong);
        }
        self.store
            .set_label(key_id, label)
            .await
            .map(|key| key.map(AdminStoredClientApiKey::from))
            .map_err(|_| AdminClientKeyError::UpdateLabel)
    }

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

    pub async fn delete(&self, key_id: &str) -> Result<bool, AdminClientKeyError> {
        self.store
            .delete(key_id)
            .await
            .map_err(|_| AdminClientKeyError::Delete)
    }

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

    pub async fn import(
        &self,
        payload: &serde_json::Value,
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
            if entry.label.as_ref().is_some_and(|l| l.chars().count() > 64) {
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

// AdminClientKeyService types

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminStoredClientApiKey {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminCreatedClientApiKey {
    pub id: String,
    pub name: String,
    pub label: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub plaintext: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatedClientApiKeyStatus {
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDeleteClientApiKeys {
    pub deleted: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedClientApiKey {
    pub source_id: Option<String>,
    pub source_prefix: Option<String>,
    pub key: AdminCreatedClientApiKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedClientApiKeys {
    pub imported: u32,
    pub skipped: u32,
    pub keys: Vec<ImportedClientApiKey>,
}

#[derive(Debug, Error)]
pub enum AdminClientKeyError {
    #[error("failed to list client API keys")]
    List,
    #[error("failed to export client API keys")]
    Export,
    #[error("failed to import client API keys")]
    Import,
    #[error("failed to create client API key")]
    Create,
    #[error("failed to delete client API key")]
    Delete,
    #[error("failed to update client API key label")]
    UpdateLabel,
    #[error("failed to update client API key status")]
    UpdateStatus,
    #[error("unsupported client API key status: {0}")]
    InvalidStatus(String),
    #[error("client API key name is required")]
    EmptyName,
    #[error("client API key ids are required")]
    EmptyIds,
    #[error("client API key label must be 64 characters or fewer")]
    LabelTooLong,
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

fn parse_client_key_import_payload(payload: &serde_json::Value) -> Vec<ClientApiKeyImportEntry> {
    use serde_json::Value;
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

fn client_key_import_entry_from_value(
    value: &serde_json::Value,
) -> Option<ClientApiKeyImportEntry> {
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

fn client_key_import_enabled(value: &serde_json::Value) -> bool {
    if let Some(enabled) = value.get("enabled").and_then(|v| v.as_bool()) {
        return enabled;
    }
    !first_string(value, &["status"])
        .unwrap_or_else(|| "active".to_string())
        .trim()
        .eq_ignore_ascii_case("disabled")
}

fn first_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
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

// ============================================================================
// AdminModelService
// ============================================================================

use crate::accounts::store::AccountStore;
use std::sync::Arc as StdArc;

/// 管理端模型服务。
#[derive(Clone)]
pub struct AdminModelService {
    models: StdArc<ModelService>,
    accounts: StdArc<dyn AccountStore>,
    installation_id: Option<String>,
}

impl AdminModelService {
    pub fn new(
        models: StdArc<ModelService>,
        accounts: StdArc<dyn AccountStore>,
        installation_id: Option<String>,
    ) -> Self {
        Self {
            models,
            accounts,
            installation_id,
        }
    }

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

#[derive(Debug, Error)]
pub enum AdminModelError {
    #[error("failed to list accounts")]
    ListAccounts,
    #[error("no active accounts available for model refresh")]
    NoAccounts,
    #[error("model snapshot store is unavailable")]
    SnapshotStoreUnavailable,
    #[error("model upstream client is unavailable")]
    UpstreamClientUnavailable,
    #[error("failed to store model snapshot")]
    StoreSnapshot,
    #[error("failed to load model snapshots")]
    LoadSnapshots,
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

// ============================================================================
// AdminLogService
// ============================================================================

use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy)]
struct AdminLogSettings {
    enabled: bool,
    capacity: u32,
    capture_body: bool,
}

/// 管理端日志服务。
#[derive(Clone)]
pub struct AdminLogService {
    store: SqliteEventLogStore,
    settings: StdArc<RwLock<AdminLogSettings>>,
}

impl AdminLogService {
    pub fn new(
        store: SqliteEventLogStore,
        enabled: bool,
        capacity: u32,
        capture_body: bool,
    ) -> Self {
        Self {
            store,
            settings: StdArc::new(RwLock::new(AdminLogSettings {
                enabled,
                capacity,
                capture_body,
            })),
        }
    }

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

    pub async fn get(&self, id: &str) -> Result<Option<EventLog>, AdminLogError> {
        self.store.get(id).await.map_err(|_| AdminLogError::Get)
    }

    pub async fn state(&self) -> Result<AdminLogState, AdminLogError> {
        let settings = *self.settings.read().await;
        Ok(AdminLogState {
            enabled: settings.enabled,
            capacity: settings.capacity,
            capture_body: settings.capture_body,
            stored_count: self.store.count().await.map_err(|_| AdminLogError::Count)?,
        })
    }

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

    pub async fn clear(&self) -> Result<AdminClearLogs, AdminLogError> {
        self.store
            .clear()
            .await
            .map(|cleared| AdminClearLogs { cleared })
            .map_err(|_| AdminLogError::Clear)
    }

    pub async fn record(&self, mut event: EventLog) -> Result<(), AdminLogError> {
        let settings = *self.settings.read().await;
        let policy = crate::telemetry::events::EventLogService::new(settings.enabled);
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminLogFilter {
    pub kind: Option<String>,
    pub level: Option<EventLevel>,
    pub request_id: Option<String>,
    pub account_id: Option<String>,
    pub route: Option<String>,
    pub model: Option<String>,
    pub status_code: Option<i64>,
    pub transport: Option<String>,
    pub attempt_index: Option<i64>,
    pub upstream_status_code: Option<i64>,
    pub failure_class: Option<String>,
    pub response_id: Option<String>,
    pub upstream_request_id: Option<String>,
    pub search: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLogState {
    pub enabled: bool,
    pub capacity: u32,
    pub capture_body: bool,
    pub stored_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminLogStateUpdate {
    pub enabled: Option<bool>,
    pub capacity: Option<u32>,
    pub capture_body: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminClearLogs {
    pub cleared: u64,
}

#[derive(Debug, Error)]
pub enum AdminLogError {
    #[error("failed to list event logs")]
    List,
    #[error("failed to get event log")]
    Get,
    #[error("failed to count event logs")]
    Count,
    #[error("failed to clear event logs")]
    Clear,
    #[error("failed to append event log")]
    Append,
    #[error("failed to trim event logs")]
    Trim,
    #[error("log capacity must be greater than zero")]
    InvalidCapacity,
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
            transport: filter.transport,
            attempt_index: filter.attempt_index,
            upstream_status_code: filter.upstream_status_code,
            failure_class: filter.failure_class,
            response_id: filter.response_id,
            upstream_request_id: filter.upstream_request_id,
            search: filter.search,
        }
    }
}

// ============================================================================
// AdminUsageService
// ============================================================================

#[derive(Clone)]
pub struct AdminUsageService {
    store: SqliteAccountStore,
}

impl AdminUsageService {
    pub fn new(store: SqliteAccountStore) -> Self {
        Self { store }
    }

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

    pub async fn summary(&self) -> Result<AdminUsageSummary, AdminUsageError> {
        self.store
            .usage_summary()
            .await
            .map(AdminUsageSummary::from)
            .map_err(|_| AdminUsageError::Summary)
    }
}

#[derive(Debug, Error)]
pub enum AdminUsageError {
    #[error("failed to list account usage")]
    List,
    #[error("failed to summarize account usage")]
    Summary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminUsageRecord {
    pub account_id: String,
    pub email: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub request_count: i64,
    pub empty_response_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
    pub image_input_tokens: i64,
    pub image_output_tokens: i64,
    pub image_request_count: i64,
    pub image_request_failed_count: i64,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminUsageSummary {
    pub account_count: i64,
    pub request_count: i64,
    pub empty_response_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
    pub image_input_tokens: i64,
    pub image_output_tokens: i64,
    pub image_request_count: i64,
    pub image_request_failed_count: i64,
}

impl From<UsageSummary> for AdminUsageSummary {
    fn from(s: UsageSummary) -> Self {
        Self {
            account_count: s.account_count,
            request_count: s.request_count,
            empty_response_count: s.empty_response_count,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            cached_tokens: s.cached_tokens,
            reasoning_tokens: s.reasoning_tokens,
            total_tokens: s.total_tokens,
            image_input_tokens: s.image_input_tokens,
            image_output_tokens: s.image_output_tokens,
            image_request_count: s.image_request_count,
            image_request_failed_count: s.image_request_failed_count,
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
            reasoning_tokens: usage.reasoning_tokens,
            total_tokens: usage.total_tokens,
            image_input_tokens: usage.image_input_tokens,
            image_output_tokens: usage.image_output_tokens,
            image_request_count: usage.image_request_count,
            image_request_failed_count: usage.image_request_failed_count,
            last_used_at: usage.last_used_at,
        }
    }
}

impl From<AccountUsageSummary> for AdminUsageSummary {
    fn from(s: AccountUsageSummary) -> Self {
        Self {
            account_count: s.account_count,
            request_count: s.request_count,
            empty_response_count: s.empty_response_count,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            cached_tokens: s.cached_tokens,
            reasoning_tokens: s.reasoning_tokens,
            total_tokens: s.total_tokens,
            image_input_tokens: s.image_input_tokens,
            image_output_tokens: s.image_output_tokens,
            image_request_count: s.image_request_count,
            image_request_failed_count: s.image_request_failed_count,
        }
    }
}

// ============================================================================
// AdminAccountService
// ============================================================================

use crate::accounts::{
    model::AccountStatus as AcctStatus, store::AccountStore as AccountStoreTrait,
};

#[derive(Clone)]
pub struct AdminAccountService {
    pub store: SqliteAccountStore,
    pub(crate) cookies: SqliteCookieStore,
    pub(crate) quota_thresholds: QuotaWarningThresholds,
    pub(crate) codex: StdArc<CodexBackendClient>,
    pub(crate) account_pool: StdArc<RuntimeAccountPoolService>,
    pub(crate) token_refresher: StdArc<dyn crate::accounts::oauth::TokenRefresher>,
    pub(crate) refresh_margin_seconds: u64,
    pub(crate) installation_id: Option<String>,
}

impl AdminAccountService {
    #[expect(
        clippy::too_many_arguments,
        reason = "service constructor wires independent stores and runtime collaborators"
    )]
    pub fn new(
        store: SqliteAccountStore,
        cookies: SqliteCookieStore,
        quota_thresholds: QuotaWarningThresholds,
        codex: StdArc<CodexBackendClient>,
        account_pool: StdArc<RuntimeAccountPoolService>,
        token_refresher: StdArc<dyn crate::accounts::oauth::TokenRefresher>,
        refresh_margin_seconds: u64,
        installation_id: Option<String>,
    ) -> Self {
        Self {
            store,
            cookies,
            quota_thresholds,
            codex,
            account_pool,
            token_refresher,
            refresh_margin_seconds,
            installation_id,
        }
    }

    fn next_refresh_at_for_expires_at(&self, expires_at: DateTime<Utc>) -> DateTime<Utc> {
        let margin_seconds = self.refresh_margin_seconds.min(i64::MAX as u64) as i64;
        expires_at - Duration::seconds(margin_seconds)
    }

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
                if user.is_none() && account.status == AcctStatus::Active {
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

    pub async fn create(
        &self,
        token: Option<String>,
        refresh_token: Option<String>,
    ) -> Result<AdminAccountMetadata, AdminAccountError> {
        let provided_refresh_token =
            crate::accounts::import_export::normalize_nonempty(refresh_token);
        let tokens = if let Some(access_token) = crate::accounts::import_export::normalize_nonempty(
            token.map(crate::accounts::import_export::normalize_bearer_token),
        ) {
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
            let access_token = crate::accounts::import_export::normalize_nonempty(Some(
                crate::accounts::import_export::normalize_bearer_token(token_pair.access_token),
            ))
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

        let claims =
            crate::accounts::token_refresh::manual_account_claims(&tokens.access_token, Utc::now())
                .map_err(AdminAccountError::InvalidToken)?;
        let existing = if let Some(account_id) = claims.account_id.as_deref() {
            self.store
                .find_by_chatgpt_identity(account_id, claims.user_id.as_deref())
                .await
                .map_err(|_| AdminAccountError::Inspect)?
        } else {
            None
        };

        let account_id = if let Some(existing) = existing {
            let updated = self
                .store
                .update_from_claims(
                    &existing.id,
                    crate::accounts::store::AccountClaimsUpdate {
                        email: claims.email.clone(),
                        account_id: claims.account_id.clone(),
                        user_id: claims.user_id.clone(),
                        plan_type: claims.plan_type.clone(),
                        access_token: SecretString::new(tokens.access_token.into()),
                        refresh_token: tokens
                            .refresh_token_for_existing
                            .map(|token| SecretString::new(token.into())),
                        access_token_expires_at: Some(claims.expires_at),
                        next_refresh_at: Some(
                            self.next_refresh_at_for_expires_at(claims.expires_at),
                        ),
                        status: crate::accounts::model::AccountStatus::Active,
                    },
                )
                .await
                .map_err(|_| AdminAccountError::UpdateClaims)?;
            if !updated {
                return Err(AdminAccountError::NotFound);
            }
            existing.id
        } else {
            let id = crate::accounts::import_export::normalized_account_id(None);
            self.store
                .insert(crate::accounts::store::NewAccount {
                    id: id.clone(),
                    email: claims.email.clone(),
                    account_id: claims.account_id.clone(),
                    user_id: claims.user_id.clone(),
                    label: None,
                    plan_type: claims.plan_type.clone(),
                    access_token: SecretString::new(tokens.access_token.into()),
                    refresh_token: tokens
                        .refresh_token_for_new
                        .map(|token| SecretString::new(token.into())),
                    access_token_expires_at: Some(claims.expires_at),
                    status: crate::accounts::model::AccountStatus::Active,
                    added_at: None,
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
            .map(stored_to_admin_metadata)
            .ok_or(AdminAccountError::NotFound)
    }

    pub async fn update_label(
        &self,
        account_id: &str,
        label: Option<String>,
    ) -> Result<bool, AdminAccountError> {
        if label.as_ref().is_some_and(|l| l.chars().count() > 64) {
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

    pub async fn refresh_account(
        &self,
        account_id: &str,
    ) -> Result<AdminAccountMetadata, AdminAccountError> {
        let account = match self.store.get(account_id).await {
            Ok(Some(account)) => account,
            Ok(None) => return Err(AdminAccountError::NotFound),
            Err(_) => return Err(AdminAccountError::Inspect),
        };
        let Some(refresh_token) = account.refresh_token.as_ref() else {
            return Err(AdminAccountError::TokenRequired);
        };

        match self
            .token_refresher
            .refresh(refresh_token.expose_secret())
            .await
        {
            Ok(tokens) => {
                let access_token = crate::accounts::import_export::normalize_nonempty(Some(
                    crate::accounts::import_export::normalize_bearer_token(tokens.access_token),
                ))
                .ok_or(AdminAccountError::TokenRequired)?;
                let claims = crate::accounts::token_refresh::manual_account_claims(
                    &access_token,
                    Utc::now(),
                )
                .map_err(AdminAccountError::InvalidToken)?;
                let updated = self
                    .store
                    .update_from_claims(
                        account_id,
                        crate::accounts::store::AccountClaimsUpdate {
                            email: claims.email,
                            account_id: claims.account_id.or(account.account_id),
                            user_id: claims.user_id,
                            plan_type: claims.plan_type,
                            access_token: SecretString::new(access_token.into()),
                            refresh_token: tokens
                                .refresh_token
                                .map(|token| SecretString::new(token.into())),
                            access_token_expires_at: Some(claims.expires_at),
                            next_refresh_at: Some(
                                self.next_refresh_at_for_expires_at(claims.expires_at),
                            ),
                            status: crate::accounts::model::AccountStatus::Active,
                        },
                    )
                    .await
                    .map_err(|_| AdminAccountError::UpdateClaims)?;
                if !updated {
                    return Err(AdminAccountError::NotFound);
                }
                self.sync_account_pool(account_id).await?;
            }
            Err(failure) => {
                let status = crate::accounts::import_export::refresh_failure_status(&failure);
                let updated = self
                    .store
                    .set_status(account_id, status)
                    .await
                    .map_err(|_| AdminAccountError::UpdateStatus)?;
                if !updated {
                    return Err(AdminAccountError::NotFound);
                }
                if crate::accounts::import_export::refresh_failure_status_clears_next_refresh_at(
                    status,
                ) {
                    let cleared = self
                        .store
                        .set_next_refresh_at(account_id, None)
                        .await
                        .map_err(|_| AdminAccountError::UpdateStatus)?;
                    if !cleared {
                        return Err(AdminAccountError::NotFound);
                    }
                }
                self.sync_account_pool_best_effort(account_id, "account refresh failure")
                    .await;
                return Err(AdminAccountError::RefreshTokenExchange(failure));
            }
        }

        self.store
            .get(account_id)
            .await
            .map_err(|_| AdminAccountError::Inspect)?
            .map(stored_to_admin_metadata)
            .ok_or(AdminAccountError::NotFound)
    }

    pub async fn reset_usage(
        &self,
        account_id: &str,
    ) -> Result<AdminAccountMetadata, AdminAccountError> {
        self.store
            .reset_usage(account_id)
            .await
            .map_err(|_| AdminAccountError::ResetUsage)?;
        self.sync_account_pool(account_id).await?;
        self.store
            .get(account_id)
            .await
            .map_err(|_| AdminAccountError::NotFound)?
            .map(stored_to_admin_metadata)
            .ok_or(AdminAccountError::NotFound)
    }

    pub async fn cookies(&self, account_id: &str) -> Result<Option<String>, AdminAccountError> {
        self.ensure_cookie_account_exists(account_id).await?;
        self.cookies
            .cookie_header(account_id, "chatgpt.com")
            .await
            .map_err(|_| AdminAccountError::LoadCookies)
    }

    pub async fn set_cookies(
        &self,
        account_id: &str,
        cookies: serde_json::Value,
    ) -> Result<Option<String>, AdminAccountError> {
        let cookie_header = match cookies {
            serde_json::Value::String(ref s) => s.trim().to_string(),
            serde_json::Value::Object(ref obj) => {
                let pairs: Vec<String> = obj
                    .iter()
                    .filter_map(|(name, val)| {
                        let v = val.as_str()?.trim();
                        if name.trim().is_empty() || v.is_empty() {
                            return None;
                        }
                        Some(format!("{}={}", name.trim(), v))
                    })
                    .collect();
                if pairs.is_empty() {
                    return Err(AdminAccountError::NoValidCookies);
                }
                pairs.join("; ")
            }
            _ => return Err(AdminAccountError::NoValidCookies),
        };
        self.ensure_cookie_account_exists(account_id).await?;
        match self
            .cookies
            .set_cookie_header(account_id, &cookie_header)
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

    pub async fn delete_cookies(&self, account_id: &str) -> Result<(), AdminAccountError> {
        self.ensure_cookie_account_exists(account_id).await?;
        self.cookies
            .delete_account_cookies(account_id)
            .await
            .map(|_| ())
            .map_err(|_| AdminAccountError::DeleteCookies)
    }

    pub async fn account_quota(
        &self,
        account_id: &str,
    ) -> Result<serde_json::Value, AdminAccountError> {
        let stored = self
            .store
            .get(account_id)
            .await
            .map_err(|_| AdminAccountError::NotFound)?
            .ok_or(AdminAccountError::NotFound)?;

        let request_id = uuid::Uuid::new_v4().to_string();
        let token = stored.access_token.expose_secret().to_string();
        let context = crate::codex::transport::CodexRequestContext {
            access_token: &token,
            account_id: stored.account_id.as_deref(),
            request_id: &request_id,
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
        };
        let raw = self
            .codex
            .fetch_usage(context)
            .await
            .map_err(|e| AdminAccountError::FetchQuota(e.to_string()))?;
        let normalized = crate::accounts::quota::quota_from_usage(&raw);
        if let Ok(json_str) = serde_json::to_string(&normalized) {
            if matches!(
                self.store.update_quota_json(account_id, &json_str).await,
                Ok(true)
            ) {
                self.sync_account_pool_best_effort(account_id, "account quota refresh")
                    .await;
            }
        }
        Ok(serde_json::json!({ "quota": normalized, "raw": raw }))
    }

    pub async fn quota_warnings(&self) -> Result<serde_json::Value, AdminAccountError> {
        let snapshots = self
            .store
            .list_quota_snapshots()
            .await
            .map_err(|_| AdminAccountError::QuotaWarnings)?;

        let mut warnings = Vec::new();
        for snap in &snapshots {
            let quota: serde_json::Value =
                serde_json::from_str(&snap.quota_json).unwrap_or(serde_json::Value::Null);
            let used = crate::accounts::quota::quota_snapshot_limit_reached(&quota);
            if used {
                warnings.push(serde_json::json!({
                    "accountId": snap.account_id,
                    "email": snap.email,
                    "level": "exhausted"
                }));
            } else {
                // Check used_percent against thresholds
                let mut check_threshold = |quota_key: &str, thresholds: &[u8]| {
                    let used_percent = quota
                        .get(quota_key)
                        .and_then(|v| v.get("used_percent"))
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    for threshold in thresholds.iter() {
                        if used_percent >= *threshold as u64 {
                            warnings.push(serde_json::json!({
                                "accountId": snap.account_id,
                                "email": snap.email,
                                "level": "warning",
                                "threshold": threshold,
                                "usedPercent": used_percent,
                                "quotaKey": quota_key,
                            }));
                            break;
                        }
                    }
                };
                check_threshold("rate_limit", &self.quota_thresholds.primary);
                check_threshold("secondary_rate_limit", &self.quota_thresholds.secondary);
            }
        }

        Ok(serde_json::json!({
            "warnings": warnings,
            "updatedAt": Utc::now().to_rfc3339()
        }))
    }

    pub async fn health_check_accounts(
        &self,
        req: serde_json::Value,
    ) -> Result<serde_json::Value, AdminAccountError> {
        use crate::accounts::store::StoredAccount;

        let ids = req
            .get("ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut results = Vec::new();
        let accounts = if ids.is_empty() {
            let mut cursor = None;
            let mut all: Vec<StoredAccount> = Vec::new();
            loop {
                let page = self
                    .store
                    .list(cursor, 200)
                    .await
                    .map_err(|_| AdminAccountError::HealthCheck)?;
                all.extend(page.items);
                if page.next_cursor.is_none() {
                    break;
                }
                cursor = page.next_cursor;
            }
            all
        } else {
            let mut list = Vec::with_capacity(ids.len());
            for id in ids {
                if let Ok(Some(acct)) = self.store.get(&id).await {
                    list.push(acct);
                }
            }
            list
        };

        let request_id = uuid::Uuid::new_v4().to_string();
        for account in &accounts {
            let token = account.access_token.expose_secret().to_string();
            let start = std::time::Instant::now();
            let context = crate::codex::transport::CodexRequestContext {
                access_token: &token,
                account_id: account.account_id.as_deref(),
                request_id: &request_id,
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
            };
            match self.codex.fetch_usage(context).await {
                Ok(_) => {
                    let duration = start.elapsed().as_millis();
                    results.push(serde_json::json!({
                        "id": account.id,
                        "email": account.email,
                        "result": "alive",
                        "durationMs": duration
                    }));
                }
                Err(e) => {
                    let duration = start.elapsed().as_millis();
                    results.push(serde_json::json!({
                        "id": account.id,
                        "email": account.email,
                        "result": "dead",
                        "error": e.to_string(),
                        "durationMs": duration
                    }));
                }
            }
        }

        let total = results.len();
        let alive = results
            .iter()
            .filter(|r| r.get("result") == Some(&serde_json::json!("alive")))
            .count();
        let dead = total - alive;

        Ok(serde_json::json!({
            "summary": { "total": total, "alive": alive, "dead": dead, "skipped": 0 },
            "results": results
        }))
    }

    pub async fn export(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<AdminAccountMetadata>, AdminAccountError> {
        if ids.is_empty() {
            let mut all = Vec::new();
            let mut cursor = None;
            loop {
                let page = self
                    .store
                    .list_metadata(cursor, 200)
                    .await
                    .map_err(|_| AdminAccountError::Export)?;
                all.extend(page.items.into_iter().map(AdminAccountMetadata::from));
                if page.next_cursor.is_none() {
                    break;
                }
                cursor = page.next_cursor;
            }
            Ok(all)
        } else {
            let mut accounts = Vec::with_capacity(ids.len());
            for id in ids {
                if let Ok(Some(stored)) = self.store.get(&id).await {
                    accounts.push(stored_to_admin_metadata(stored));
                }
            }
            Ok(accounts)
        }
    }

    pub async fn export_with_tokens(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<crate::accounts::store::StoredAccount>, AdminAccountError> {
        if ids.is_empty() {
            let mut all = Vec::new();
            let mut cursor = None;
            loop {
                let page = self
                    .store
                    .list(cursor, 200)
                    .await
                    .map_err(|_| AdminAccountError::Export)?;
                all.extend(page.items);
                if page.next_cursor.is_none() {
                    break;
                }
                cursor = page.next_cursor;
            }
            Ok(all)
        } else {
            let mut accounts = Vec::with_capacity(ids.len());
            for id in ids {
                if let Ok(Some(stored)) = self.store.get(&id).await {
                    accounts.push(stored);
                }
            }
            Ok(accounts)
        }
    }

    pub async fn import(
        &self,
        data: serde_json::Value,
    ) -> Result<ImportedAccounts, AdminAccountError> {
        let parsed = crate::accounts::import_export::parse_account_import_payload(&data)
            .map_err(|_| AdminAccountError::NoImportableAccounts)?;
        let source_format = parsed.source.as_str();
        let entries = parsed.entries;
        if entries.is_empty() {
            return Err(AdminAccountError::NoImportableAccounts);
        }

        let mut imported = 0u32;
        let mut skipped = 0u32;
        for entry in entries {
            match self.import_entry(entry, parsed.source).await? {
                ImportedAccountState::Imported(account_id) => {
                    imported += 1;
                    self.sync_account_pool(&account_id).await?;
                }
                ImportedAccountState::Skipped => skipped += 1,
            }
        }

        Ok(ImportedAccounts {
            imported,
            skipped,
            source_format,
        })
    }

    pub async fn import_codex_cli_auth(
        &self,
        data: serde_json::Value,
    ) -> Result<AdminAccountMetadata, AdminAccountError> {
        let token = crate::accounts::import_export::first_string(
            &data,
            &["access_token", "accessToken", "token"],
        );
        let refresh_token =
            crate::accounts::import_export::first_string(&data, &["refresh_token", "refreshToken"]);
        if token.is_none() && refresh_token.is_none() {
            return Err(AdminAccountError::NoImportableAccounts);
        }
        self.create(token, refresh_token).await
    }

    async fn import_entry(
        &self,
        entry: crate::accounts::import_export::AccountImportEntry,
        source: crate::accounts::import_export::AccountImportSource,
    ) -> Result<ImportedAccountState, AdminAccountError> {
        let Some(resolved_tokens) = self
            .resolve_import_tokens(entry.token, entry.refresh_token)
            .await?
        else {
            return Ok(ImportedAccountState::Skipped);
        };
        let label = crate::accounts::import_export::normalize_label(entry.label);
        if label
            .as_ref()
            .is_some_and(|label| label.chars().count() > 64)
        {
            return Err(AdminAccountError::LabelTooLong);
        }

        let access_token_expires_at = entry
            .access_token_expires_at
            .as_deref()
            .map(crate::accounts::import_export::parse_account_import_datetime)
            .transpose()
            .map_err(|_| AdminAccountError::InvalidAccessTokenExpiresAt)?;
        let quota_fetched_at = entry
            .quota_fetched_at
            .as_deref()
            .map(crate::accounts::import_export::parse_account_import_datetime)
            .transpose()
            .map_err(|_| AdminAccountError::InvalidAccessTokenExpiresAt)?;
        let mut quota_json = entry
            .cached_quota
            .as_ref()
            .map(serde_json::Value::to_string);
        let mut quota_fetched_at = quota_fetched_at;
        let quota_verify_required = entry.quota_verify_required.unwrap_or(false);
        let parsed_status =
            crate::accounts::import_export::parse_account_import_status(entry.status.as_deref())
                .map_err(|error| AdminAccountError::InvalidStatus(error.to_string()))?;
        let mut status = crate::accounts::import_export::normalized_imported_account_status(
            parsed_status,
            source,
            &resolved_tokens.access_token,
        );
        let access_token_expires_at = resolved_tokens
            .claims
            .as_ref()
            .map(|claims| claims.expires_at)
            .or(access_token_expires_at);
        let next_refresh_at = access_token_expires_at
            .map(|expires_at| self.next_refresh_at_for_expires_at(expires_at));
        let claims = resolved_tokens.claims.as_ref();
        let mut plan_type = claims
            .and_then(|claims| claims.plan_type.clone())
            .or_else(|| crate::accounts::import_export::normalize_nonempty(entry.plan_type));
        if plan_type.is_none() {
            plan_type = entry.cached_quota.as_ref().and_then(import_quota_plan_type);
        }
        let email = claims
            .and_then(|claims| claims.email.clone())
            .or_else(|| crate::accounts::import_export::normalize_nonempty(entry.email.clone()));
        let chatgpt_account_id = claims
            .and_then(|claims| claims.account_id.as_deref())
            .or_else(|| {
                crate::accounts::import_export::normalize_nonempty_str(entry.account_id.as_deref())
            });
        let chatgpt_user_id = claims
            .and_then(|claims| claims.user_id.as_deref())
            .or_else(|| {
                crate::accounts::import_export::normalize_nonempty_str(entry.user_id.as_deref())
            });
        let supplemental = self
            .import_supplemental_account_info(
                &resolved_tokens.access_token,
                chatgpt_account_id,
                ImportSupplementalNeeds {
                    account_id: chatgpt_account_id.is_none(),
                    user_id: chatgpt_user_id.is_none(),
                    email: email.is_none(),
                    plan_type: plan_type.is_none(),
                    quota: quota_json.is_none(),
                },
            )
            .await;
        let chatgpt_account_id = supplemental
            .account_id
            .or_else(|| chatgpt_account_id.map(ToString::to_string));
        let chatgpt_user_id = chatgpt_user_id
            .map(ToString::to_string)
            .or(supplemental.user_id);
        let email = email.or(supplemental.email);
        let account_id = self
            .import_target_account_id(
                entry.id.as_deref(),
                chatgpt_account_id.as_deref(),
                chatgpt_user_id.as_deref(),
            )
            .await?;
        if plan_type.is_none() {
            plan_type = supplemental.plan_type;
        }
        if quota_json.is_none() {
            quota_json = supplemental.quota_json;
            quota_fetched_at = supplemental.quota_fetched_at.or(quota_fetched_at);
        }
        if let Some(supplemental_status) = supplemental.status {
            status = supplemental_status;
        }
        let account = crate::accounts::store::NewAccount {
            id: account_id.clone(),
            email,
            account_id: chatgpt_account_id,
            user_id: chatgpt_user_id,
            label,
            plan_type,
            access_token: SecretString::new(resolved_tokens.access_token.into()),
            refresh_token: resolved_tokens
                .refresh_token
                .map(|token| SecretString::new(token.into())),
            access_token_expires_at,
            status,
            added_at: None,
        };

        match self.store.get(&account_id).await {
            Ok(Some(_)) => {
                let updated = self
                    .store
                    .update_from_import(crate::accounts::store::ImportedAccountUpdate {
                        account,
                        quota_json,
                        quota_fetched_at,
                        quota_verify_required,
                    })
                    .await
                    .map_err(|_| AdminAccountError::Import)?;
                if !updated {
                    return Err(AdminAccountError::NotFound);
                }
                self.store
                    .set_next_refresh_at(&account_id, next_refresh_at)
                    .await
                    .map_err(|_| AdminAccountError::Import)?;
            }
            Ok(None) => {
                self.store
                    .insert(account)
                    .await
                    .map_err(|_| AdminAccountError::Import)?;

                self.store
                    .set_next_refresh_at(&account_id, next_refresh_at)
                    .await
                    .map_err(|_| AdminAccountError::Import)?;

                if quota_json.is_some() || quota_fetched_at.is_some() || quota_verify_required {
                    self.store
                        .apply_imported_quota_state(
                            &account_id,
                            quota_json.as_deref(),
                            quota_fetched_at,
                            quota_verify_required,
                        )
                        .await
                        .map_err(|_| AdminAccountError::Import)?;
                }
            }
            Err(_) => return Err(AdminAccountError::Inspect),
        }

        Ok(ImportedAccountState::Imported(account_id))
    }

    async fn import_supplemental_account_info(
        &self,
        access_token: &str,
        account_id: Option<&str>,
        needs: ImportSupplementalNeeds,
    ) -> ImportSupplementalAccountInfo {
        if !needs.any() {
            return ImportSupplementalAccountInfo::default();
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let context = crate::codex::transport::CodexRequestContext {
            access_token,
            account_id,
            request_id: &request_id,
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
        };

        match self.codex.fetch_usage(context).await {
            Ok(raw) => {
                let normalized = crate::accounts::quota::quota_from_usage(&raw);
                ImportSupplementalAccountInfo {
                    account_id: import_usage_string(&raw, "account_id"),
                    user_id: import_usage_string(&raw, "user_id"),
                    email: import_usage_string(&raw, "email"),
                    plan_type: import_usage_plan_type(&raw),
                    quota_json: serde_json::to_string(&normalized).ok(),
                    quota_fetched_at: Some(Utc::now()),
                    status: None,
                }
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "failed to fetch supplemental account information during import"
                );
                ImportSupplementalAccountInfo {
                    status: import_status_from_usage_error(&error),
                    ..ImportSupplementalAccountInfo::default()
                }
            }
        }
    }

    async fn resolve_import_tokens(
        &self,
        token: Option<String>,
        refresh_token: Option<String>,
    ) -> Result<Option<ResolvedImportTokens>, AdminAccountError> {
        let mut refresh_token = crate::accounts::import_export::normalize_nonempty(refresh_token);
        let Some(access_token) = crate::accounts::import_export::normalize_nonempty(
            token.map(crate::accounts::import_export::normalize_bearer_token),
        ) else {
            let Some(existing_refresh_token) = refresh_token else {
                return Ok(None);
            };
            let refreshed = self
                .token_refresher
                .refresh(&existing_refresh_token)
                .await
                .map_err(AdminAccountError::RefreshTokenExchange)?;
            let access_token = crate::accounts::import_export::normalize_nonempty(Some(
                crate::accounts::import_export::normalize_bearer_token(refreshed.access_token),
            ))
            .ok_or(AdminAccountError::TokenRequired)?;
            refresh_token = refreshed.refresh_token.or(Some(existing_refresh_token));
            let claims =
                crate::accounts::token_refresh::manual_account_claims(&access_token, Utc::now())
                    .map_err(AdminAccountError::InvalidToken)?;
            return Ok(Some(ResolvedImportTokens {
                access_token,
                refresh_token,
                claims: Some(claims),
            }));
        };

        if let Ok(claims) =
            crate::accounts::token_refresh::manual_account_claims(&access_token, Utc::now())
        {
            return Ok(Some(ResolvedImportTokens {
                access_token,
                refresh_token,
                claims: Some(claims),
            }));
        }

        let Some(existing_refresh_token) = refresh_token else {
            return Ok(Some(ResolvedImportTokens {
                access_token,
                refresh_token: None,
                claims: None,
            }));
        };
        let refreshed = self
            .token_refresher
            .refresh(&existing_refresh_token)
            .await
            .map_err(AdminAccountError::RefreshTokenExchange)?;
        let access_token = crate::accounts::import_export::normalize_nonempty(Some(
            crate::accounts::import_export::normalize_bearer_token(refreshed.access_token),
        ))
        .ok_or(AdminAccountError::TokenRequired)?;
        refresh_token = refreshed.refresh_token.or(Some(existing_refresh_token));
        let claims =
            crate::accounts::token_refresh::manual_account_claims(&access_token, Utc::now())
                .map_err(AdminAccountError::InvalidToken)?;
        Ok(Some(ResolvedImportTokens {
            access_token,
            refresh_token,
            claims: Some(claims),
        }))
    }

    async fn import_target_account_id(
        &self,
        id: Option<&str>,
        account_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<String, AdminAccountError> {
        let provided_id =
            crate::accounts::import_export::normalize_nonempty_str(id).map(ToString::to_string);
        if let Some(id) = provided_id.as_deref() {
            match self.store.get(id).await {
                Ok(Some(_)) => return Ok(id.to_string()),
                Ok(None) => {}
                Err(_) => return Err(AdminAccountError::Inspect),
            }
        }

        let chatgpt_account_id = crate::accounts::import_export::normalize_nonempty_str(account_id);
        let chatgpt_user_id = crate::accounts::import_export::normalize_nonempty_str(user_id);
        if let Some(chatgpt_account_id) = chatgpt_account_id {
            if let Some(existing) = self
                .store
                .find_by_chatgpt_identity(chatgpt_account_id, chatgpt_user_id)
                .await
                .map_err(|_| AdminAccountError::Inspect)?
            {
                return Ok(existing.id);
            }
        }

        Ok(provided_id
            .unwrap_or_else(|| crate::accounts::import_export::normalized_account_id(None)))
    }

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

// Account types
#[derive(Debug, Clone, serde::Serialize)]
pub struct AdminAccountMetadata {
    pub id: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub status: AcctStatus,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub added_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<StoredAccountMetadata> for AdminAccountMetadata {
    fn from(m: StoredAccountMetadata) -> Self {
        let added_at = m
            .added_at
            .parse::<DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now());
        let updated_at = m
            .updated_at
            .parse::<DateTime<Utc>>()
            .unwrap_or_else(|_| Utc::now());
        Self {
            id: m.id,
            email: m.email,
            account_id: m.account_id,
            user_id: m.user_id,
            label: m.label,
            plan_type: m.plan_type,
            status: m.status,
            access_token_expires_at: m.access_token_expires_at,
            added_at,
            updated_at,
        }
    }
}

#[derive(Debug, Error)]
pub enum AdminAccountError {
    #[error("failed to list accounts")]
    List,
    #[error("failed to export accounts")]
    Export,
    #[error("failed to import accounts")]
    Import,
    #[error("failed to inspect account")]
    Inspect,
    #[error("account not found")]
    NotFound,
    #[error("failed to update label")]
    UpdateLabel,
    #[error("failed to update status")]
    UpdateStatus,
    #[error("failed to delete account")]
    Delete,
    #[error("failed to load cookies")]
    LoadCookies,
    #[error("failed to store cookies")]
    StoreCookies,
    #[error("failed to delete cookies")]
    DeleteCookies,
    #[error("failed to update claims")]
    UpdateClaims,
    #[error("failed to reset usage")]
    ResetUsage,
    #[error("failed to sync account pool")]
    SyncAccountPool,
    #[error("failed to get quota warnings")]
    QuotaWarnings,
    #[error("failed to store quota")]
    StoreQuota,
    #[error("failed to fetch quota: {0}")]
    FetchQuota(String),
    #[error("health check failed")]
    HealthCheck,
    #[error("invalid status: {0}")]
    InvalidStatus(String),
    #[error("label must be 64 characters or fewer")]
    LabelTooLong,
    #[error("account ids are required")]
    EmptyIds,
    #[error("no importable accounts found")]
    NoImportableAccounts,
    #[error("invalid access token expires at")]
    InvalidAccessTokenExpiresAt,
    #[error("token is required")]
    TokenRequired,
    #[error("invalid token: {0}")]
    InvalidToken(&'static str),
    #[error("token refresh exchange failed: {0}")]
    RefreshTokenExchange(crate::accounts::oauth::RefreshFailure),
    #[error("no valid cookies provided")]
    NoValidCookies,
    #[error("account is {0}")]
    Inactive(AcctStatus),
}

#[derive(Debug, Clone)]
pub struct AdminAuthStatus {
    pub authenticated: bool,
    pub user: Option<AdminAccountMetadata>,
    pub pool: AdminAuthPoolStatus,
}

#[derive(Debug, Clone, Default)]
pub struct AdminAuthPoolStatus {
    pub total: u32,
    pub active: u32,
    pub expired: u32,
    pub quota_exhausted: u32,
    pub refreshing: u32,
    pub disabled: u32,
    pub banned: u32,
}

impl AdminAuthPoolStatus {
    fn record(&mut self, status: AcctStatus) {
        self.total += 1;
        match status {
            AcctStatus::Active => self.active += 1,
            AcctStatus::Expired => self.expired += 1,
            AcctStatus::QuotaExhausted => self.quota_exhausted += 1,
            AcctStatus::Refreshing => self.refreshing += 1,
            AcctStatus::Disabled => self.disabled += 1,
            AcctStatus::Banned => self.banned += 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AdminAuthLogout {
    pub success: bool,
    pub deleted: u64,
}

#[derive(Debug, Clone)]
pub struct UpdatedAccountStatus {
    pub id: String,
    pub status: AcctStatus,
}

#[derive(Debug, Clone)]
pub struct BatchDeleteAccounts {
    pub deleted: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BatchUpdateAccountStatus {
    pub updated: u32,
    pub not_found: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ImportedAccounts {
    pub imported: u32,
    pub skipped: u32,
    pub source_format: &'static str,
}

#[derive(Debug, Clone)]
struct ManualCreateTokens {
    access_token: String,
    refresh_token_for_new: Option<String>,
    refresh_token_for_existing: Option<String>,
}

#[derive(Debug, Clone)]
struct ResolvedImportTokens {
    access_token: String,
    refresh_token: Option<String>,
    claims: Option<crate::accounts::token_refresh::ManualAccountClaims>,
}

#[derive(Debug, Clone, Default)]
struct ImportSupplementalAccountInfo {
    account_id: Option<String>,
    user_id: Option<String>,
    email: Option<String>,
    plan_type: Option<String>,
    quota_json: Option<String>,
    quota_fetched_at: Option<DateTime<Utc>>,
    status: Option<AccountStatus>,
}

#[derive(Debug, Clone, Copy)]
struct ImportSupplementalNeeds {
    account_id: bool,
    user_id: bool,
    email: bool,
    plan_type: bool,
    quota: bool,
}

impl ImportSupplementalNeeds {
    fn any(self) -> bool {
        self.account_id || self.user_id || self.email || self.plan_type || self.quota
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ImportedAccountState {
    Imported(String),
    Skipped,
}

// Helper: convert StoredAccount -> AdminAccountMetadata
fn stored_to_admin_metadata(s: crate::accounts::store::StoredAccount) -> AdminAccountMetadata {
    AdminAccountMetadata::from(crate::accounts::store::StoredAccountMetadata {
        id: s.id,
        email: s.email,
        account_id: s.account_id,
        user_id: s.user_id,
        label: s.label,
        plan_type: s.plan_type,
        access_token_expires_at: s.access_token_expires_at,
        status: s.status,
        added_at: s.added_at,
        updated_at: s.updated_at,
    })
}

fn import_usage_plan_type(usage: &serde_json::Value) -> Option<String> {
    usage
        .get("plan_type")
        .and_then(serde_json::Value::as_str)
        .and_then(normalized_plan_type)
}

fn import_usage_string(usage: &serde_json::Value, key: &str) -> Option<String> {
    usage
        .get(key)
        .and_then(serde_json::Value::as_str)
        .and_then(|value| crate::accounts::import_export::normalize_nonempty_str(Some(value)))
        .map(ToString::to_string)
}

fn import_quota_plan_type(quota: &serde_json::Value) -> Option<String> {
    quota
        .get("plan_type")
        .and_then(serde_json::Value::as_str)
        .and_then(normalized_plan_type)
}

fn normalized_plan_type(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    (!value.is_empty() && !matches!(value.as_str(), "unknown" | "null")).then_some(value)
}

fn import_status_from_usage_error(error: &CodexClientError) -> Option<AccountStatus> {
    if crate::gateway::dispatch::responses::is_banned_upstream_error(error) {
        Some(AccountStatus::Banned)
    } else {
        None
    }
}

// Helper functions
fn parse_account_status(status: &str) -> Result<AcctStatus, AdminAccountError> {
    crate::accounts::import_export::parse_account_status(status)
        .map_err(|_| AdminAccountError::InvalidStatus(status.trim().to_ascii_lowercase()))
}

fn parse_batch_account_status(status: &str) -> Result<AcctStatus, AdminAccountError> {
    crate::accounts::import_export::parse_batch_account_status(status)
        .map_err(|_| AdminAccountError::InvalidStatus(status.trim().to_ascii_lowercase()))
}

// ============================================================================
// AdminOAuthService
// ============================================================================

use crate::accounts::oauth::{
    DeviceCode, OAuthClient, OAuthConfig, OAuthError, PkceLogin, PkceSessionStore, TokenPair,
};

#[derive(Clone)]
pub struct AdminOAuthService {
    config: OAuthConfig,
    client: StdArc<dyn OAuthClient>,
    sessions: StdArc<tokio::sync::Mutex<PkceSessionStore>>,
}

impl AdminOAuthService {
    pub async fn login_start(&self) -> Result<serde_json::Value, AdminAccountError> {
        Ok(serde_json::json!({"url": ""}))
    }
    pub async fn device_login(
        &self,
        _data: serde_json::Value,
    ) -> Result<serde_json::Value, AdminAccountError> {
        Ok(serde_json::json!({"device_code": ""}))
    }
    pub async fn device_poll(&self, _code: &str) -> Result<serde_json::Value, AdminAccountError> {
        Ok(serde_json::json!({"status": "pending"}))
    }
    pub async fn code_relay(
        &self,
        _data: serde_json::Value,
    ) -> Result<serde_json::Value, AdminAccountError> {
        Ok(serde_json::json!({"status": "ok"}))
    }
    pub async fn callback(
        &self,
        _params: std::collections::HashMap<String, String>,
    ) -> Result<serde_json::Value, AdminAccountError> {
        Ok(serde_json::json!({"status": "ok"}))
    }
    pub fn new(config: OAuthConfig, client: StdArc<dyn OAuthClient>) -> Self {
        Self {
            config,
            client,
            sessions: StdArc::new(tokio::sync::Mutex::new(PkceSessionStore::default())),
        }
    }

    pub async fn start_pkce_login(&self, return_host: &str) -> PkceLogin {
        self.sessions
            .lock()
            .await
            .start_login(return_host, &self.config)
    }

    pub async fn request_device_code(&self) -> Result<DeviceCode, AdminOAuthError> {
        self.client
            .request_device_code()
            .await
            .map_err(AdminOAuthError::OAuth)
    }

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

#[derive(Debug, Clone)]
pub enum AdminDevicePoll {
    Pending { code: &'static str },
    Authorized(TokenPair),
}

#[derive(Debug, Clone)]
pub struct AdminOAuthCallback {
    pub tokens: TokenPair,
    pub return_host: String,
}

#[derive(Debug, Error)]
pub enum AdminOAuthError {
    #[error("invalid OAuth callback")]
    InvalidCallback,
    #[error("invalid OAuth state")]
    InvalidState,
    #[error("{0}")]
    OAuth(OAuthError),
}

// ============================================================================
// RuntimeAccountPoolService
// ============================================================================

use crate::accounts::pool::AcquiredAccount;

#[derive(Clone)]
pub struct RuntimeAccountPoolService {
    pool: StdArc<tokio::sync::Mutex<AccountPool>>,
    store: StdArc<dyn AccountStoreTrait>,
    request_interval: StdDuration,
}

impl RuntimeAccountPoolService {
    pub fn new(
        store: StdArc<dyn AccountStoreTrait>,
        options: AccountPoolOptions,
        request_interval_ms: u64,
    ) -> Self {
        Self {
            pool: StdArc::new(tokio::sync::Mutex::new(AccountPool::with_options(options))),
            store,
            request_interval: StdDuration::from_millis(request_interval_ms),
        }
    }

    pub async fn restore_from_repository(&self) -> Result<usize, RuntimeAccountPoolError> {
        let accounts = self
            .store
            .list_pool_accounts()
            .await
            .map_err(|_| RuntimeAccountPoolError::Generic)?;
        let count = accounts.len();
        let mut pool = self.pool.lock().await;
        pool.clear();
        for account in accounts {
            pool.insert(account);
        }
        Ok(count)
    }

    pub async fn clear(&self) {
        self.pool.lock().await.clear();
    }

    pub async fn capacity_summary(&self, now: DateTime<Utc>) -> AccountCapacitySummary {
        self.pool.lock().await.capacity_summary(now)
    }

    pub async fn capacity_summary_now(&self) -> AccountCapacitySummary {
        self.capacity_summary(Utc::now()).await
    }

    pub async fn release(&self, account_id: &str) {
        self.pool.lock().await.release(account_id);
    }

    pub async fn set_status(&self, account_id: &str, status: AccountStatus) -> bool {
        let persisted = match self.store.set_status(account_id, status).await {
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

    pub async fn mark_quota_limited_until(&self, account_id: &str, until: DateTime<Utc>) -> bool {
        let persisted = match self.store.mark_quota_limited_until(account_id, until).await {
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
            .mark_quota_limited_until(account_id, until);
        persisted || in_memory
    }

    pub async fn acquire_with(&self, request: AccountAcquireRequest) -> Option<AcquiredAccount> {
        let acquired = self.pool.lock().await.acquire_with(request)?;
        if let Err(error) = self.store.record_request(&acquired.account.id).await {
            tracing::warn!(
                account_id = acquired.account.id,
                error = %error,
                "failed to persist account request usage"
            );
        }
        Some(acquired)
    }

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
            tokio::time::sleep(self.request_interval - elapsed).await;
        }
    }

    pub async fn record_token_usage(&self, account_id: &str, usage: &TokenUsage) {
        self.record_response_usage(account_id, *usage, false).await;
    }

    pub async fn record_response_usage(
        &self,
        account_id: &str,
        usage: TokenUsage,
        image_generation_requested: bool,
    ) {
        let image_request_succeeded = image_generation_requested && usage.image_output_tokens > 0;
        let image_request_failed = image_generation_requested && !image_request_succeeded;
        let persisted_usage = AccountUsageDelta {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cached_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            total_tokens: usage.total_tokens,
            image_input_tokens: usage.image_input_tokens,
            image_output_tokens: usage.image_output_tokens,
            image_requests: u64::from(image_request_succeeded),
            image_request_failures: u64::from(image_request_failed),
            ..AccountUsageDelta::default()
        };
        if let Err(error) = self
            .store
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

    pub async fn record_empty_response_attempt(
        &self,
        account_id: &str,
        image_generation_requested: bool,
    ) {
        let usage = AccountUsageDelta {
            empty_responses: 1,
            image_request_failures: u64::from(image_generation_requested),
            ..AccountUsageDelta::default()
        };
        if let Err(error) = self.store.record_usage_delta(account_id, usage).await {
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

    pub async fn sync_passive_rate_limit_headers(
        &self,
        account: &Account,
        headers: &[(String, String)],
    ) {
        let Some(rate_limits) = parse_rate_limit_headers(headers) else {
            return;
        };
        let existing_quota = match self.store.get_quota_json(&account.id).await {
            Ok(Some(quota_json)) => serde_json::from_str::<serde_json::Value>(&quota_json).ok(),
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
        self.apply_quota_snapshot(&account.id, &quota).await;
    }

    pub async fn account_snapshot(&self, account_id: &str) -> Option<Account> {
        self.pool.lock().await.get(account_id)
    }

    pub async fn apply_quota_snapshot(&self, account_id: &str, quota: &serde_json::Value) -> bool {
        let limit_reached = quota_snapshot_limit_reached(quota);
        let reset_at = quota_snapshot_reset_at(quota);
        let cooldown_until = limit_reached.then_some(reset_at).flatten();
        let quota_json = quota.to_string();
        let persisted = match self
            .store
            .apply_quota_snapshot(account_id, &quota_json, limit_reached, cooldown_until)
            .await
        {
            Ok(persisted) => persisted,
            Err(error) => {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist verified quota snapshot"
                );
                false
            }
        };
        let in_memory =
            self.pool
                .lock()
                .await
                .apply_quota_state(account_id, limit_reached, cooldown_until);

        if let Some(reset_at) = reset_at {
            let limit_window_seconds =
                crate::accounts::quota::quota_snapshot_limit_window_seconds(quota);
            if let Err(error) = self
                .store
                .sync_rate_limit_window(account_id, reset_at, limit_window_seconds)
                .await
            {
                tracing::warn!(
                    account_id,
                    error = %error,
                    "failed to persist verified quota window"
                );
            }
            self.pool.lock().await.sync_rate_limit_window(
                account_id,
                reset_at,
                limit_window_seconds,
            );
        }

        persisted || in_memory
    }

    pub async fn remove_account(&self, account_id: &str) -> bool {
        self.pool.lock().await.remove(account_id)
    }

    pub async fn delete_account(&self, account_id: &str) -> bool {
        self.remove_account(account_id).await
    }

    pub async fn sync_account_from_repository(
        &self,
        account_id: &str,
    ) -> Result<bool, RuntimeAccountPoolError> {
        let account = self
            .store
            .get_pool_account(account_id)
            .await
            .map_err(|_| RuntimeAccountPoolError::Generic)?;
        let mut pool = self.pool.lock().await;
        if let Some(account) = account {
            pool.insert(account);
            return Ok(true);
        }
        Ok(pool.remove(account_id))
    }

    pub async fn reset_usage(&self, account_id: &str) -> bool {
        self.pool.lock().await.reset_usage(account_id)
    }

    pub async fn set_cloudflare_cooldown_until(
        &self,
        account_id: &str,
        cooldown_until: DateTime<Utc>,
    ) -> bool {
        let persisted = match self
            .store
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
}

#[derive(Debug, Error)]
pub enum RuntimeAccountPoolError {
    #[error("account pool error")]
    Generic,
}

// ============================================================================
// RuntimeSessionAffinityService (re-export from gateway)
// ============================================================================

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
    pub admin_oauth: StdArc<AdminOAuthService>,
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
        Self::with_installation_id(config, stores, fingerprint, None)
    }

    pub fn with_installation_id(
        config: &AppConfig,
        stores: BackgroundTaskStores,
        fingerprint: Fingerprint,
        installation_id: Option<String>,
    ) -> Self {
        let installation_id = installation_id.filter(|id| !id.trim().is_empty());
        let account_store_trait =
            StdArc::new(stores.accounts.clone()) as StdArc<dyn AccountStoreTrait>;
        let codex = {
            let client = CodexBackendClient::new(
                reqwest::Client::new(),
                config.api.base_url.clone(),
                fingerprint.clone(),
            );
            if config.ws_pool.enabled {
                let pool = StdArc::new(crate::codex::transport::CodexWebSocketPool::with_config(
                    crate::codex::transport::CodexWebSocketPoolConfig {
                        enabled: config.ws_pool.enabled,
                        max_age: std::time::Duration::from_millis(config.ws_pool.max_age_ms),
                        max_per_account: config.ws_pool.max_per_account,
                        ..crate::codex::transport::CodexWebSocketPoolConfig::default()
                    },
                ));
                StdArc::new(client.with_websocket_pool(pool))
            } else {
                StdArc::new(client)
            }
        };
        let settings = StdArc::new(RuntimeSettingsService::with_config_path(
            config.clone(),
            "config.yaml",
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
        let account_pool = StdArc::new(RuntimeAccountPoolService::new(
            account_store_trait.clone(),
            AccountPoolOptions::default(),
            config.auth.request_interval_ms,
        ));
        let admin_accounts = StdArc::new(AdminAccountService::new(
            stores.accounts.clone(),
            stores.cookies.clone(),
            config.quota.warning_thresholds.clone(),
            codex.clone(),
            account_pool.clone(),
            StdArc::new(default_openai_oauth_client(
                crate::accounts::oauth::OAuthConfig {
                    client_id: config.auth.oauth_client_id.clone(),
                    auth_endpoint: config.auth.oauth_auth_endpoint.clone(),
                    device_code_endpoint: config
                        .auth
                        .oauth_token_endpoint
                        .strip_suffix("/token")
                        .map(|p| format!("{p}/device/code"))
                        .unwrap_or_else(|| "https://auth.openai.com/oauth/device/code".to_string()),
                    token_endpoint: config.auth.oauth_token_endpoint.clone(),
                },
            )),
            config.auth.refresh_margin_seconds,
            installation_id.clone(),
        ));
        let admin_oauth = StdArc::new(AdminOAuthService::new(
            crate::accounts::oauth::OAuthConfig {
                client_id: config.auth.oauth_client_id.clone(),
                auth_endpoint: config.auth.oauth_auth_endpoint.clone(),
                device_code_endpoint: config
                    .auth
                    .oauth_token_endpoint
                    .strip_suffix("/token")
                    .map(|p| format!("{p}/device/code"))
                    .unwrap_or_else(|| "https://auth.openai.com/oauth/device/code".to_string()),
                token_endpoint: config.auth.oauth_token_endpoint.clone(),
            },
            StdArc::new(default_openai_oauth_client(
                crate::accounts::oauth::OAuthConfig {
                    client_id: config.auth.oauth_client_id.clone(),
                    auth_endpoint: config.auth.oauth_auth_endpoint.clone(),
                    device_code_endpoint: config
                        .auth
                        .oauth_token_endpoint
                        .strip_suffix("/token")
                        .map(|p| format!("{p}/device/code"))
                        .unwrap_or_else(|| "https://auth.openai.com/oauth/device/code".to_string()),
                    token_endpoint: config.auth.oauth_token_endpoint.clone(),
                },
            )),
        ));
        let logs = StdArc::new(AdminLogService::new(
            stores.event_logs.clone(),
            config.logging.enabled,
            config.logging.capacity,
            config.logging.capture_body,
        ));
        let usage = StdArc::new(AdminUsageService::new(stores.accounts.clone()));
        let session_affinity = StdArc::new(RuntimeSessionAffinityService::new(
            stores.session_affinity.clone(),
        ));

        let upstream_client: StdArc<dyn CodexModelCatalogClient> = codex.clone();
        let snapshot_store: StdArc<dyn ModelSnapshotStore> = StdArc::new(
            crate::codex::models::SqliteModelSnapshotStore::new(stores.accounts.pool().clone()),
        );
        let models = StdArc::new(ModelService::new(
            crate::codex::models::ModelConfig {
                default_model: config.model.default_model.clone(),
                default_reasoning_effort: config.model.default_reasoning_effort.clone(),
                service_tier: config.model.service_tier.clone(),
                aliases: config.model.aliases.clone(),
            },
            Some(snapshot_store.clone()),
            Some(upstream_client.clone()),
            None,
        ));
        let admin_models = StdArc::new(AdminModelService::new(
            StdArc::new(ModelService::new(
                crate::codex::models::ModelConfig {
                    default_model: config.model.default_model.clone(),
                    default_reasoning_effort: config.model.default_reasoning_effort.clone(),
                    service_tier: config.model.service_tier.clone(),
                    aliases: config.model.aliases.clone(),
                },
                Some(snapshot_store),
                Some(upstream_client),
                None,
            )),
            account_store_trait.clone(),
            installation_id.clone(),
        ));
        let cloudflare_recovery =
            crate::gateway::dispatch::responses::CloudflareRecovery::new(stores.cookies.clone());
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

        Self {
            models,
            admin_models,
            accounts: account_store_trait,
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
            fingerprint,
            installation_id,
            background_tasks: stores,
        }
    }

    pub async fn probe_codex_models_endpoint(&self, request_id: &str) -> UpstreamProbeResult {
        let config = self.settings.current();
        let context = crate::codex::transport::CodexRequestContext {
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
        };
        let result = self.codex.probe_connectivity(context).await;
        match result {
            Ok(probe) => UpstreamProbeResult {
                target: "codexModels",
                backend_base_url: config.api.base_url.clone(),
                endpoint: probe.endpoint,
                reachable: !probe.status.is_server_error(),
                status_code: Some(probe.status.as_u16()),
                authorization: "unknown",
            },
            Err(_error) => UpstreamProbeResult {
                target: "codexModels",
                backend_base_url: config.api.base_url.clone(),
                endpoint: String::new(),
                reachable: false,
                status_code: None,
                authorization: "unknown",
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct UpstreamProbeResult {
    pub target: &'static str,
    pub backend_base_url: String,
    pub endpoint: String,
    pub reachable: bool,
    pub status_code: Option<u16>,
    pub authorization: &'static str,
}
