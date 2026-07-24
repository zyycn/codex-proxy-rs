//! 管理控制面所需的持久化能力。
//!
//! 端口按业务资源拆分，方法使用领域模型，不暴露连接池、事务或 Redis client。

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::model::{
    MutationContext, Revision,
    accounts::{
        AccountListQuery, AccountPage, AccountRecord, AccountUsage, DeleteAccounts,
        SetAccountEnabled,
    },
    auth::{AdminAuditEvent, AdminSession},
    client_keys::{
        ClientKeyListQuery, ClientKeyPage, ClientKeyRecord, ClientKeySecret, DeleteClientKey,
        NewClientKey, SetClientKeyEnabled, UpdateClientKey,
    },
    observability::{
        DashboardObservation, DashboardRuntimeSlots, DiagnosticDimension, DiagnosticObservation,
        OpsErrorPage, OpsErrorQuery, RequestMetricPoint, TimeRange, UsageDetail, UsageFilter,
        UsageOverview, UsagePage, UsageQuery,
    },
    provider_credentials::{
        AuthorizationCommit, CredentialDetails, CredentialImportCommit, CredentialImportResult,
        CredentialListQuery, CredentialMutationResult, CredentialPage, CredentialRotationCommit,
        ProviderExportCredentialInput,
    },
    settings::{AdminApiKey, AdminApiKeyMutation, ReplaceRuntimeSettings, RuntimeSettings},
};

/// 管理端可判定的持久化失败类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminStoreErrorKind {
    Invalid,
    NotFound,
    StaleRevision,
    Conflict,
    Unavailable,
}

/// 隐藏数据库实现细节的持久化错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{resource} store operation failed: {message}")]
pub struct AdminStoreError {
    kind: AdminStoreErrorKind,
    resource: &'static str,
    message: String,
}

impl AdminStoreError {
    #[must_use]
    pub fn new(
        kind: AdminStoreErrorKind,
        resource: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            resource,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> AdminStoreErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn resource(&self) -> &'static str {
        self.resource
    }
}

pub type AdminStoreResult<T> = Result<T, AdminStoreError>;

/// 账号目录与公共账号写操作。
#[async_trait]
pub trait AccountStore: Send + Sync {
    async fn list_accounts(&self, query: AccountListQuery) -> AdminStoreResult<AccountPage>;

    async fn load_account(&self, account_id: &str) -> AdminStoreResult<Option<AccountRecord>>;

    async fn load_account_usage(
        &self,
        range: TimeRange,
        account_ids: &[String],
    ) -> AdminStoreResult<Vec<AccountUsage>>;

    async fn list_credentials(
        &self,
        provider_kind: &gateway_core::routing::ProviderKind,
        query: CredentialListQuery,
    ) -> AdminStoreResult<CredentialPage>;

    async fn credential_details(
        &self,
        provider_kind: &gateway_core::routing::ProviderKind,
        account_id: &gateway_core::engine::credential::ProviderAccountId,
    ) -> AdminStoreResult<Option<CredentialDetails>>;

    async fn load_credentials_for_export(
        &self,
        provider_kind: &gateway_core::routing::ProviderKind,
        account_ids: &[gateway_core::engine::credential::ProviderAccountId],
    ) -> AdminStoreResult<Vec<ProviderExportCredentialInput>>;

    async fn commit_credential_import(
        &self,
        command: CredentialImportCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialImportResult>;

    async fn commit_authorization(
        &self,
        command: AuthorizationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult>;

    async fn commit_credential_rotation(
        &self,
        command: CredentialRotationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult>;

    async fn commit_credential_refresh(
        &self,
        command: CredentialRotationCommit,
        context: &MutationContext,
    ) -> AdminStoreResult<CredentialMutationResult>;

    async fn set_account_enabled(
        &self,
        command: SetAccountEnabled,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision>;

    async fn delete_accounts(
        &self,
        command: DeleteAccounts,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision>;

    async fn record_credential_export(
        &self,
        account_ids: &[gateway_core::engine::credential::ProviderAccountId],
        context: &MutationContext,
    ) -> AdminStoreResult<()>;
}

/// 管理员密码、会话、登录限流和安全审计。
#[async_trait]
pub trait AuthStore: Send + Sync {
    async fn load_password_hash(&self, admin_user_id: &str) -> AdminStoreResult<Option<String>>;

    async fn create_password_hash_if_absent(
        &self,
        admin_user_id: &str,
        password_hash: &str,
    ) -> AdminStoreResult<bool>;

    async fn load_admin_api_key(&self) -> AdminStoreResult<Option<AdminApiKey>>;

    async fn load_session(&self, session_id: &str) -> AdminStoreResult<Option<AdminSession>>;

    async fn store_session(&self, session_id: &str, session: &AdminSession)
    -> AdminStoreResult<()>;

    async fn delete_session(&self, session_id: &str) -> AdminStoreResult<Option<AdminSession>>;

    async fn login_source_is_throttled(
        &self,
        source: &str,
        failure_limit: u32,
        window_seconds: u64,
    ) -> AdminStoreResult<bool>;

    async fn record_login_failure(
        &self,
        source: &str,
        failure_limit: u32,
        window_seconds: u64,
    ) -> AdminStoreResult<bool>;

    async fn clear_login_failures(&self, source: &str) -> AdminStoreResult<()>;

    async fn append_audit_event(&self, event: AdminAuditEvent) -> AdminStoreResult<()>;
}

/// Client API Key 管理及其 revision CAS 写入。
#[async_trait]
pub trait ClientKeyStore: Send + Sync {
    async fn list_client_keys(&self, query: ClientKeyListQuery) -> AdminStoreResult<ClientKeyPage>;

    async fn reveal_client_key(
        &self,
        id: &gateway_core::policy::ClientApiKeyId,
    ) -> AdminStoreResult<Option<ClientKeySecret>>;

    async fn create_client_key(
        &self,
        command: NewClientKey,
        context: &MutationContext,
    ) -> AdminStoreResult<(Revision, ClientKeyRecord)>;

    async fn update_client_key(
        &self,
        command: UpdateClientKey,
        context: &MutationContext,
    ) -> AdminStoreResult<(Revision, ClientKeyRecord)>;

    async fn set_client_key_enabled(
        &self,
        command: SetClientKeyEnabled,
        context: &MutationContext,
    ) -> AdminStoreResult<(Revision, ClientKeyRecord)>;

    async fn delete_client_key(
        &self,
        command: DeleteClientKey,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision>;
}

/// 用量、趋势、诊断与运维错误的只读能力。
#[async_trait]
pub trait ObservabilityStore: Send + Sync {
    async fn dashboard_summary(&self, range: TimeRange) -> AdminStoreResult<DashboardObservation>;

    /// 返回 Dashboard 可选的实时槽位事实。
    ///
    /// 该状态来自可丢失的运行时存储；无实现或运行时存储不可用时返回 `None`，不影响
    /// 持久观测数据的读取。
    async fn dashboard_runtime_slots(
        &self,
        _observed_at: DateTime<Utc>,
    ) -> AdminStoreResult<Option<DashboardRuntimeSlots>> {
        Ok(None)
    }

    async fn dashboard_trend(&self, range: TimeRange) -> AdminStoreResult<Vec<RequestMetricPoint>>;

    async fn usage_trend(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> AdminStoreResult<Vec<RequestMetricPoint>>;

    async fn list_usage_records(&self, query: UsageQuery) -> AdminStoreResult<UsagePage>;

    async fn usage_record_detail(&self, request_id: &str) -> AdminStoreResult<UsageDetail>;

    async fn usage_summary(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> AdminStoreResult<UsageOverview>;

    async fn usage_diagnostics(
        &self,
        range: TimeRange,
        filter: UsageFilter,
        dimension: DiagnosticDimension,
    ) -> AdminStoreResult<Vec<DiagnosticObservation>>;

    async fn list_ops_errors(&self, query: OpsErrorQuery) -> AdminStoreResult<OpsErrorPage>;
}

/// Runtime settings 与管理员 API Key 的 revision CAS 写入。
#[async_trait]
pub trait SettingsStore: Send + Sync {
    async fn load_runtime_settings(&self) -> AdminStoreResult<RuntimeSettings>;

    async fn admin_api_key_exists(&self) -> AdminStoreResult<bool>;

    async fn replace_runtime_settings(
        &self,
        command: ReplaceRuntimeSettings,
        context: &MutationContext,
    ) -> AdminStoreResult<RuntimeSettings>;

    async fn replace_admin_api_key(
        &self,
        expected_config_revision: Revision,
        key: AdminApiKey,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation>;

    async fn delete_admin_api_key(
        &self,
        expected_config_revision: Revision,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation>;
}

/// 管理用例所需能力的封闭集合。
///
/// 字段保持私有，每个 getter 只交出一种明确能力。该类型不提供通用拆包入口。
#[derive(Clone)]
pub struct AdminStorePorts {
    accounts: Arc<dyn AccountStore>,
    auth: Arc<dyn AuthStore>,
    client_keys: Arc<dyn ClientKeyStore>,
    observability: Arc<dyn ObservabilityStore>,
    settings: Arc<dyn SettingsStore>,
}

impl AdminStorePorts {
    #[must_use]
    pub fn new(
        accounts: Arc<dyn AccountStore>,
        auth: Arc<dyn AuthStore>,
        client_keys: Arc<dyn ClientKeyStore>,
        observability: Arc<dyn ObservabilityStore>,
        settings: Arc<dyn SettingsStore>,
    ) -> Self {
        Self {
            accounts,
            auth,
            client_keys,
            observability,
            settings,
        }
    }

    #[must_use]
    pub fn accounts(&self) -> Arc<dyn AccountStore> {
        self.accounts.clone()
    }

    #[must_use]
    pub fn auth(&self) -> Arc<dyn AuthStore> {
        self.auth.clone()
    }

    #[must_use]
    pub fn client_keys(&self) -> Arc<dyn ClientKeyStore> {
        self.client_keys.clone()
    }

    #[must_use]
    pub fn observability(&self) -> Arc<dyn ObservabilityStore> {
        self.observability.clone()
    }

    #[must_use]
    pub fn settings(&self) -> Arc<dyn SettingsStore> {
        self.settings.clone()
    }
}
