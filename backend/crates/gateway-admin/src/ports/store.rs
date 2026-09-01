//! 管理控制面所需的持久化能力。
//!
//! 端口按业务资源拆分，方法使用领域模型，不暴露连接池、事务或 Redis client。

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::backup::BackupStorePorts;
use crate::model::{
    MutationContext, Revision,
    account_groups::{
        AccountGroupListQuery, AccountGroupMemberFact, AccountGroupMutation, AccountGroupPage,
        DeleteAccountGroup, NewAccountGroup, SetAccountGroupEnabled, UpdateAccountGroup,
    },
    accounts::{
        AccountListQuery, AccountPage, AccountPageItem, AccountRuntimeSnapshot,
        AccountUpdateResult, AccountUsage, AccountUsageWindowQuery, AccountUsageWindowResult,
        AccountsUpdateResult, BatchUpdateAccounts, DeleteAccounts, UpdateAccount,
    },
    auth::{AdminAuditEvent, AdminSession},
    client_keys::{
        ClientKeyListQuery, ClientKeyPage, ClientKeyRecord, ClientKeySecret, DeleteClientKey,
        NewClientKey, SetClientKeyEnabled, UpdateClientKey,
    },
    observability::{
        DashboardObservation, DashboardRuntimeSlots, DiagnosticDimension, DiagnosticObservation,
        OpsErrorPage, OpsErrorQuery, RequestMetricPoint, TimeRange, UsageCalculatedBillingFact,
        UsageDetail, UsageFilter, UsageOverview, UsagePage, UsageQuery,
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
    async fn list_accounts(
        &self,
        query: AccountListQuery,
        runtime: AccountRuntimeSnapshot,
    ) -> AdminStoreResult<AccountPage>;

    async fn load_account(
        &self,
        account_id: &str,
        runtime: AccountRuntimeSnapshot,
    ) -> AdminStoreResult<Option<AccountPageItem>>;

    async fn load_account_usage(
        &self,
        range: TimeRange,
        account_ids: &[String],
    ) -> AdminStoreResult<Vec<AccountUsage>>;

    async fn load_account_usage_by_windows(
        &self,
        windows: &[AccountUsageWindowQuery],
    ) -> AdminStoreResult<Vec<AccountUsageWindowResult>>;

    async fn list_credentials(
        &self,
        provider_kind: &gateway_core::routing::ProviderKind,
        query: CredentialListQuery,
    ) -> AdminStoreResult<CredentialPage>;

    async fn credential_details(
        &self,
        provider_kind: &gateway_core::routing::ProviderKind,
        account_id: &gateway_core::account::ProviderAccountId,
    ) -> AdminStoreResult<Option<CredentialDetails>>;

    async fn load_credentials_for_export(
        &self,
        provider_kind: &gateway_core::routing::ProviderKind,
        account_ids: &[gateway_core::account::ProviderAccountId],
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

    async fn update_account(
        &self,
        command: UpdateAccount,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult>;

    async fn recover_account(
        &self,
        account_id: &gateway_core::account::ProviderAccountId,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountUpdateResult>;

    async fn batch_update_accounts(
        &self,
        command: BatchUpdateAccounts,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountsUpdateResult>;

    async fn delete_accounts(
        &self,
        command: DeleteAccounts,
        context: &MutationContext,
    ) -> AdminStoreResult<Revision>;

    async fn record_credential_export(
        &self,
        account_ids: &[gateway_core::account::ProviderAccountId],
        context: &MutationContext,
    ) -> AdminStoreResult<()>;
}

/// 可丢失账号运行态的管理读端口；跨存储编排由 Admin application service 拥有。
#[async_trait]
pub trait AccountRuntimeStore: Send + Sync {
    async fn active_rate_limits(&self) -> AdminStoreResult<AccountRuntimeSnapshot>;

    async fn account_runtime(
        &self,
        account_ids: &[String],
    ) -> AdminStoreResult<AccountRuntimeSnapshot>;
}

/// 管理员密码、会话和安全审计。
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

    async fn append_audit_event(&self, event: AdminAuditEvent) -> AdminStoreResult<()>;
}

/// Client API Key 管理写入。
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

/// Provider-neutral account group management transactions.
#[async_trait]
pub trait AccountGroupStore: Send + Sync {
    async fn list_account_groups(
        &self,
        query: AccountGroupListQuery,
    ) -> AdminStoreResult<AccountGroupPage>;

    async fn load_account_group_members(
        &self,
        group_ids: &[gateway_core::routing::AccountGroupId],
    ) -> AdminStoreResult<Vec<AccountGroupMemberFact>>;

    async fn create_account_group(
        &self,
        command: NewAccountGroup,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation>;

    async fn update_account_group(
        &self,
        command: UpdateAccountGroup,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation>;

    async fn set_account_group_enabled(
        &self,
        command: SetAccountGroupEnabled,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation>;

    async fn delete_account_group(
        &self,
        command: DeleteAccountGroup,
        context: &MutationContext,
    ) -> AdminStoreResult<AccountGroupMutation>;
}

/// 用量、趋势、诊断与运维错误的只读能力。
#[async_trait]
pub trait ObservabilityStore: Send + Sync {
    /// 返回历史统计区间和指定观测时刻下的实时账号状态。
    async fn dashboard_summary(
        &self,
        range: TimeRange,
        observed_at: DateTime<Utc>,
    ) -> AdminStoreResult<DashboardObservation>;

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

    /// 返回可由 Provider 重新校验的已计算费用事实，用于恢复标准费用趋势。
    async fn usage_calculated_billing_facts(
        &self,
        range: TimeRange,
        filter: UsageFilter,
    ) -> AdminStoreResult<Vec<UsageCalculatedBillingFact>>;

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

/// Runtime settings 与管理员 API Key 写入。
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
        key: AdminApiKey,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation>;

    async fn delete_admin_api_key(
        &self,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation>;
}

/// 账号目录、运行态与分组所需的 Store 能力集合。
#[derive(Clone)]
pub struct AdminAccountStorePorts {
    accounts: Arc<dyn AccountStore>,
    runtime: Arc<dyn AccountRuntimeStore>,
    groups: Arc<dyn AccountGroupStore>,
}

impl AdminAccountStorePorts {
    #[must_use]
    pub fn new(
        accounts: Arc<dyn AccountStore>,
        runtime: Arc<dyn AccountRuntimeStore>,
        groups: Arc<dyn AccountGroupStore>,
    ) -> Self {
        Self {
            accounts,
            runtime,
            groups,
        }
    }
}

/// 管理用例所需能力的封闭集合。
///
/// 字段保持私有，每个 getter 只交出一种明确能力。该类型不提供通用拆包入口。
#[derive(Clone)]
pub struct AdminStorePorts {
    accounts: AdminAccountStorePorts,
    auth: Arc<dyn AuthStore>,
    client_keys: Arc<dyn ClientKeyStore>,
    observability: Arc<dyn ObservabilityStore>,
    settings: Arc<dyn SettingsStore>,
    backup: BackupStorePorts,
}

impl AdminStorePorts {
    #[must_use]
    pub fn new(
        accounts: AdminAccountStorePorts,
        auth: Arc<dyn AuthStore>,
        client_keys: Arc<dyn ClientKeyStore>,
        observability: Arc<dyn ObservabilityStore>,
        settings: Arc<dyn SettingsStore>,
        backup: BackupStorePorts,
    ) -> Self {
        Self {
            accounts,
            auth,
            client_keys,
            observability,
            settings,
            backup,
        }
    }

    #[must_use]
    pub fn accounts(&self) -> Arc<dyn AccountStore> {
        self.accounts.accounts.clone()
    }

    #[must_use]
    pub fn account_runtime(&self) -> Arc<dyn AccountRuntimeStore> {
        self.accounts.runtime.clone()
    }

    #[must_use]
    pub fn account_groups(&self) -> Arc<dyn AccountGroupStore> {
        self.accounts.groups.clone()
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

    #[must_use]
    pub fn backup(&self) -> BackupStorePorts {
        self.backup.clone()
    }
}
