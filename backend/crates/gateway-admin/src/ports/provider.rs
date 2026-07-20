//! Provider 管理能力与动态注册表。

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use gateway_core::{engine::credential::ProviderAccountId, routing::ProviderKind};

use crate::model::observability::{
    CalculatedBillingBreakdown, DashboardWireProfile, ProviderBillingInput,
};
use crate::model::provider_credentials::{
    AuthorizationStarted, CompleteAuthorization, PendingAuthorizationMutation,
    PrepareCredentialImport, PrepareCredentialRefresh, PrepareCredentialRotation,
    PreparedAuthorizationCommit, PreparedCredentialImport, PreparedCredentialRotation,
    ProviderExport, ProviderExportCredentialInput, ProviderModels, ProviderQuota,
};

/// Provider 管理失败的稳定分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAdminErrorKind {
    Invalid,
    Unsupported,
    NotFound,
    Conflict,
    Unavailable,
    Internal,
}

/// 不泄漏 OAuth material 与 Provider 响应体的管理错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("provider admin operation failed: {kind:?}")]
pub struct ProviderAdminError {
    kind: ProviderAdminErrorKind,
}

impl ProviderAdminError {
    #[must_use]
    pub const fn new(kind: ProviderAdminErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> ProviderAdminErrorKind {
        self.kind
    }
}

/// 一个具体 Provider 对管理控制面提供的解析、验证、上游交互与运行时资源回收能力。
///
/// 数据变更由 Provider 返回 prepared facts；config revision、审计与 PostgreSQL 事务
/// 全部由 [`crate::ports::store::AccountStore`] 提交。运行时资源通知只在事务成功后发生。
#[async_trait]
pub trait ProviderAdmin: Send + Sync {
    fn provider_kind(&self) -> &ProviderKind;

    /// 账号已经由控制面提交为不可调度状态，释放 Provider 持有的账号级运行时资源。
    ///
    /// 无账号级运行时资源的 Provider 不需要执行额外操作。该通知发生在 Store 事务
    /// 成功之后，不参与事务成败，也不得恢复或改写已经提交的账号状态。
    async fn account_unavailable(&self, account_id: &ProviderAccountId);

    /// 返回该 Provider 实际持有的 Dashboard wire 画像；没有画像能力时返回 `None`。
    fn dashboard_wire_profile(&self) -> Option<DashboardWireProfile>;

    /// 使用 Provider-owned 价格规则恢复持久请求的逐项费用。
    fn calculated_billing(
        &self,
        input: &ProviderBillingInput,
    ) -> Result<Option<CalculatedBillingBreakdown>, ProviderAdminError>;

    async fn prepare_import(
        &self,
        command: PrepareCredentialImport,
    ) -> Result<PreparedCredentialImport, ProviderAdminError>;

    async fn start_authorization(
        &self,
        pending: PendingAuthorizationMutation,
    ) -> Result<AuthorizationStarted, ProviderAdminError>;

    async fn complete_authorization(
        &self,
        command: CompleteAuthorization,
    ) -> Result<PreparedAuthorizationCommit, ProviderAdminError>;

    async fn prepare_rotation(
        &self,
        command: PrepareCredentialRotation,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError>;

    async fn prepare_refresh(
        &self,
        command: PrepareCredentialRefresh,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError>;

    async fn quota(
        &self,
        account_id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<ProviderQuota, ProviderAdminError>;

    async fn models(
        &self,
        account_id: &ProviderAccountId,
        refresh: bool,
    ) -> Result<ProviderModels, ProviderAdminError>;

    async fn export_credentials(
        &self,
        credentials: Vec<ProviderExportCredentialInput>,
    ) -> Result<ProviderExport, ProviderAdminError>;
}

/// 按 ProviderKind 动态发现管理能力；不含具体 Provider 分支。
#[derive(Clone)]
pub struct ProviderAdminRegistry {
    providers: Arc<BTreeMap<ProviderKind, Arc<dyn ProviderAdmin>>>,
}

impl ProviderAdminRegistry {
    /// 创建无重复 ProviderKind 的注册表。
    ///
    /// # Errors
    ///
    /// 重复注册同一 ProviderKind 时返回 Conflict。
    pub fn new(
        providers: impl IntoIterator<Item = Arc<dyn ProviderAdmin>>,
    ) -> Result<Self, ProviderAdminError> {
        let mut registered = BTreeMap::new();
        for provider in providers {
            let kind = provider.provider_kind().clone();
            if registered.insert(kind, provider).is_some() {
                return Err(ProviderAdminError::new(ProviderAdminErrorKind::Conflict));
            }
        }
        Ok(Self {
            providers: Arc::new(registered),
        })
    }

    pub fn require(
        &self,
        provider_kind: &ProviderKind,
    ) -> Result<Arc<dyn ProviderAdmin>, ProviderAdminError> {
        self.providers
            .get(provider_kind)
            .cloned()
            .ok_or_else(|| ProviderAdminError::new(ProviderAdminErrorKind::Unsupported))
    }

    /// 返回唯一注册的 Dashboard wire 画像。
    ///
    /// # Errors
    ///
    /// 没有 Provider 提供画像时返回 Unsupported；多个 Provider 同时声明画像时返回 Conflict。
    pub fn dashboard_wire_profile(&self) -> Result<DashboardWireProfile, ProviderAdminError> {
        let mut profiles = self
            .providers
            .values()
            .filter_map(|provider| provider.dashboard_wire_profile());
        let profile = profiles
            .next()
            .ok_or_else(|| ProviderAdminError::new(ProviderAdminErrorKind::Unsupported))?;
        if profiles.next().is_some() {
            return Err(ProviderAdminError::new(ProviderAdminErrorKind::Conflict));
        }
        Ok(profile)
    }

    /// 动态分派 Provider-owned 费用规则，不含任何具体 Provider 分支。
    pub fn calculated_billing(
        &self,
        provider_kind: &ProviderKind,
        input: &ProviderBillingInput,
    ) -> Result<Option<CalculatedBillingBreakdown>, ProviderAdminError> {
        self.require(provider_kind)?.calculated_billing(input)
    }
}
