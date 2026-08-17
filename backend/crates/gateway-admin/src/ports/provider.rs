//! Provider 管理能力与动态注册表。

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use gateway_core::{
    engine::credential::ProviderAccountId,
    operation::Operation,
    routing::{ProviderKind, UpstreamModelId},
};

use crate::model::observability::{
    CalculatedBillingBreakdown, DashboardWireProfile, ProviderBillingInput,
};
use crate::model::provider_credentials::{
    AuthorizationStarted, CompleteAuthorization, PendingAuthorizationMutation,
    PrepareCredentialImport, PrepareCredentialRefresh, PrepareCredentialRotation,
    PreparedAuthorizationCommit, PreparedCredentialImport, PreparedCredentialRotation,
    ProviderExport, ProviderExportCredentialInput, ProviderModels, ProviderQuota,
    ProviderQuotaRequest,
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

/// 不携带 OAuth 请求材料的管理错误。
///
/// `message` 用于当前认证管理请求展示；上游没有标准 `error.message` 时，
/// Provider 可以按其官方契约回退为完整的非成功响应正文。
#[derive(Clone, PartialEq, Eq, thiserror::Error)]
#[error("provider admin operation failed: {kind:?}")]
pub struct ProviderAdminError {
    kind: ProviderAdminErrorKind,
    message: Option<String>,
}

impl std::fmt::Debug for ProviderAdminError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderAdminError")
            .field("kind", &self.kind)
            .field("message", &self.message.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl ProviderAdminError {
    #[must_use]
    pub const fn new(kind: ProviderAdminErrorKind) -> Self {
        Self {
            kind,
            message: None,
        }
    }

    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    #[must_use]
    pub const fn kind(&self) -> ProviderAdminErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
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

    /// 账号资格事实已经由控制面提交，失效 Provider 持有的可重建派生状态。
    ///
    /// 该通知发生在 Store 事务成功之后、下一份 RuntimeSnapshot 编译之前；通知
    /// 不参与已提交事务成败。没有账号派生状态的 Provider 可使用默认空实现。
    async fn account_facts_changed(&self, _account_ids: &[ProviderAccountId]) {}

    /// 生成一次连接测试所需的 Provider-owned operation；Core 负责实际执行与落账。
    fn connection_test_operation(
        &self,
        upstream_model: &UpstreamModelId,
        input_text: &str,
    ) -> Result<Operation, ProviderAdminError>;

    /// 返回该 Provider 实际持有的 Dashboard 上游身份画像。
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
        request: ProviderQuotaRequest,
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

    /// 返回所有已注册 Provider 的 Dashboard 上游身份画像。
    pub fn dashboard_wire_profiles(&self) -> Vec<DashboardWireProfile> {
        self.providers
            .values()
            .filter_map(|provider| provider.dashboard_wire_profile())
            .collect()
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
