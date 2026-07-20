//! Provider 管理能力交换的中立 Command 与 Result。

use std::fmt;

use chrono::{DateTime, Utc};
use gateway_core::{
    engine::credential::{OpaqueProviderData, ProviderAccountId},
    routing::{ProviderInstanceId, ProviderKind, UpstreamModelId},
};

use super::{
    MutationActor, MutationContext, PageSize, Revision,
    accounts::{AccountAvailability, AccountRecord, AccountStatus, AccountSummary},
};

/// Provider-owned JSON；公共层只搬运且 Debug 不输出值。
#[derive(Clone, PartialEq)]
pub struct ProviderDocument(OpaqueProviderData);

impl ProviderDocument {
    #[must_use]
    pub const fn new(data: OpaqueProviderData) -> Self {
        Self(data)
    }

    /// 仅具体 Provider 可以解释内部字段。
    #[must_use]
    pub const fn expose_to_provider(&self) -> &OpaqueProviderData {
        &self.0
    }

    #[must_use]
    pub fn into_provider_data(self) -> OpaqueProviderData {
        self.0
    }
}

impl fmt::Debug for ProviderDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProviderDocument([PROVIDER_OWNED])")
    }
}

/// Credential 列表稳定游标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialCursor {
    pub created_at: DateTime<Utc>,
    pub account_id: ProviderAccountId,
}

/// Provider credential 列表查询。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialListQuery {
    pub provider_instance_id: Option<ProviderInstanceId>,
    pub availability: Option<CredentialAvailabilityFilter>,
    pub enabled: Option<bool>,
    pub window: CredentialListWindow,
}

/// Credential 可用性筛选；支持一个 wire 值对应多个持久状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialAvailabilityFilter {
    Exact(AccountAvailability),
    AnyOf(Vec<AccountAvailability>),
}

impl CredentialAvailabilityFilter {
    #[must_use]
    pub fn matches(&self, availability: AccountAvailability) -> bool {
        match self {
            Self::Exact(expected) => *expected == availability,
            Self::AnyOf(expected) => expected.contains(&availability),
        }
    }
}

/// Credential 目录的集合窗口；完整列表与游标分页互斥。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialListWindow {
    All,
    Page {
        cursor: Option<CredentialCursor>,
        page_size: PageSize,
    },
}

/// Provider credential 列表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialPage {
    pub config_revision: Revision,
    pub items: Vec<AccountRecord>,
    pub next_cursor: Option<CredentialCursor>,
}

/// Provider credential 详情。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialDetails {
    pub config_revision: Revision,
    pub credential: AccountRecord,
}

/// Provider 正式文档批量导入命令。
pub struct ImportCredentials {
    pub context: MutationContext,
    pub expected_config_revision: Revision,
    pub provider_instance_id: ProviderInstanceId,
    pub document: ProviderDocument,
}

impl fmt::Debug for ImportCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImportCredentials")
            .field("context", &self.context)
            .field("expected_config_revision", &self.expected_config_revision)
            .field("provider_instance_id", &self.provider_instance_id)
            .field("document", &self.document)
            .finish()
    }
}

/// 批量导入提交结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialImportResult {
    pub config_revision: Revision,
    pub credential_ids: Vec<ProviderAccountId>,
}

/// Provider 解析导入文档时只接收实例与不透明文档，不接触 revision 或审计上下文。
pub struct PrepareCredentialImport {
    pub provider_instance_id: ProviderInstanceId,
    pub document: ProviderDocument,
}

impl fmt::Debug for PrepareCredentialImport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrepareCredentialImport")
            .field("provider_instance_id", &self.provider_instance_id)
            .field("document", &self.document)
            .finish()
    }
}

/// Provider 已验证、可由 Store 原子创建的一份 credential。
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCredentialCreate {
    pub account_id: ProviderAccountId,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_kind: ProviderKind,
    pub name: String,
    pub email: Option<String>,
    pub upstream_user_id: String,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    pub provider_material: ProviderDocument,
    pub has_refresh_token: bool,
    pub access_token_expires_at: DateTime<Utc>,
    pub next_refresh_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub availability: AccountAvailability,
    pub availability_reason: Option<String>,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub availability_observed_at: DateTime<Utc>,
}

/// Provider 对一份导入文档的完整验证结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCredentialImport {
    pub provider_kind: ProviderKind,
    pub provider_instance_id: ProviderInstanceId,
    pub credentials: Vec<PreparedCredentialCreate>,
}

/// Admin 交给 Store 的导入事务命令。
#[derive(Debug, Clone, PartialEq)]
pub struct CredentialImportCommit {
    pub expected_config_revision: Revision,
    pub prepared: PreparedCredentialImport,
}

/// 可选的重新授权目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReauthorizationTarget {
    pub account_id: ProviderAccountId,
    pub credential_revision: Revision,
}

/// OAuth pending owner 的中立身份；不编码具体 Provider 的 Redis key 或 JSON。
#[derive(Clone, PartialEq, Eq)]
pub enum AuthorizationOwner {
    AdminSession { admin_user_id: String },
    AdminApiKey,
    System,
}

impl fmt::Debug for AuthorizationOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthorizationOwner([REDACTED])")
    }
}

/// Provider 写入 pending payload 与 Redis owner binding 所需的全部中立字段。
#[derive(Clone, PartialEq, Eq)]
pub struct AuthorizationOwnerBinding {
    owner: AuthorizationOwner,
    started_request_id: String,
}

impl AuthorizationOwnerBinding {
    #[must_use]
    pub fn from_context(context: &MutationContext) -> Self {
        let owner = match &context.actor {
            MutationActor::AdminSession { admin_user_id } => AuthorizationOwner::AdminSession {
                admin_user_id: admin_user_id.clone(),
            },
            MutationActor::AdminApiKey => AuthorizationOwner::AdminApiKey,
            MutationActor::System => AuthorizationOwner::System,
        };
        Self {
            owner,
            started_request_id: context.request_id.clone(),
        }
    }

    #[must_use]
    pub const fn owner(&self) -> &AuthorizationOwner {
        &self.owner
    }

    #[must_use]
    pub fn started_request_id(&self) -> &str {
        &self.started_request_id
    }

    #[must_use]
    pub fn matches_context(&self, context: &MutationContext) -> bool {
        Self::from_context(context).owner == self.owner
    }
}

impl fmt::Debug for AuthorizationOwnerBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthorizationOwnerBinding([REDACTED])")
    }
}

/// OAuth 完成时应创建新账号还是 CAS 更新既有 credential。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizationMutationTarget {
    Create {
        provider_instance_id: ProviderInstanceId,
        name: String,
    },
    Reauthorize {
        provider_instance_id: ProviderInstanceId,
        account_id: ProviderAccountId,
        expected_credential_revision: Revision,
    },
}

/// 必须完整进入 Provider opaque pending payload、并在 complete 后原样恢复的事务信封。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAuthorizationMutation {
    expected_config_revision: Revision,
    provider_kind: ProviderKind,
    target: AuthorizationMutationTarget,
    owner_binding: AuthorizationOwnerBinding,
}

impl PendingAuthorizationMutation {
    #[must_use]
    pub const fn new(
        expected_config_revision: Revision,
        provider_kind: ProviderKind,
        target: AuthorizationMutationTarget,
        owner_binding: AuthorizationOwnerBinding,
    ) -> Self {
        Self {
            expected_config_revision,
            provider_kind,
            target,
            owner_binding,
        }
    }

    #[must_use]
    pub const fn expected_config_revision(&self) -> Revision {
        self.expected_config_revision
    }

    #[must_use]
    pub const fn provider_kind(&self) -> &ProviderKind {
        &self.provider_kind
    }

    #[must_use]
    pub const fn target(&self) -> &AuthorizationMutationTarget {
        &self.target
    }

    #[must_use]
    pub const fn owner_binding(&self) -> &AuthorizationOwnerBinding {
        &self.owner_binding
    }
}

/// 启动 Provider OAuth Authorization Code 流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartAuthorization {
    pub context: MutationContext,
    pub expected_config_revision: Revision,
    pub provider_instance_id: ProviderInstanceId,
    pub name: String,
    pub reauthorization: Option<ReauthorizationTarget>,
}

/// OAuth 流程启动结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationStarted {
    pub flow_id: String,
    pub authorization_url: String,
    pub expires_at: DateTime<Utc>,
}

/// refresh/rotate/reauthorize 的 lease 或 completion 生命周期。
///
/// 该 guard 不可 Clone。Admin 只能在 Store CAS 与审计事务成功后调用 `finish`；失败路径直接
/// drop，使 Provider 可以释放 lease 或执行补偿。
pub trait CredentialCommitGuard: Send + 'static {
    fn finish(self: Box<Self>);
}

/// OAuth complete 后由 Provider 返回的准备结果；Store 仍是唯一提交者。
pub enum PreparedAuthorizationCredential {
    Create(PreparedCredentialCreate),
    Reauthorize(PreparedCredentialRotation),
}

/// Provider 从 opaque pending payload 恢复的信封与已验证 credential 必须一起返回。
pub struct PreparedAuthorizationCommit {
    pub pending: PendingAuthorizationMutation,
    pub credential: PreparedAuthorizationCredential,
}

impl fmt::Debug for PreparedAuthorizationCommit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedAuthorizationCommit")
            .field("pending", &self.pending)
            .field("credential", &"[PREPARED]")
            .finish()
    }
}

/// Store 可持久化的 OAuth credential facts，不携带 Provider guard。
#[derive(Debug, Clone, PartialEq)]
pub enum AuthorizationCredentialCommit {
    Create(PreparedCredentialCreate),
    Reauthorize(PreparedCredentialRotationFacts),
}

/// Admin 交给 Store 的 OAuth 原子事务命令。
#[derive(Debug, Clone, PartialEq)]
pub struct AuthorizationCommit {
    pub pending: PendingAuthorizationMutation,
    pub credential: AuthorizationCredentialCommit,
}

impl PreparedAuthorizationCommit {
    #[must_use]
    pub fn into_commit(self) -> (AuthorizationCommit, Option<Box<dyn CredentialCommitGuard>>) {
        let (credential, guard) = match self.credential {
            PreparedAuthorizationCredential::Create(credential) => {
                (AuthorizationCredentialCommit::Create(credential), None)
            }
            PreparedAuthorizationCredential::Reauthorize(prepared) => {
                let (facts, guard) = prepared.into_parts();
                (
                    AuthorizationCredentialCommit::Reauthorize(facts),
                    Some(guard),
                )
            }
        };
        (
            AuthorizationCommit {
                pending: self.pending,
                credential,
            },
            guard,
        )
    }
}

/// 完成 Provider OAuth 流程。
#[derive(Clone, PartialEq, Eq)]
pub struct CompleteAuthorization {
    pub context: MutationContext,
    pub flow_id: String,
    pub callback_url: String,
}

impl fmt::Debug for CompleteAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompleteAuthorization")
            .field("context", &self.context)
            .field("flow_id", &"[REDACTED]")
            .field("callback_url", &"[REDACTED]")
            .finish()
    }
}

/// Credential 生命周期写操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialMutation {
    pub context: MutationContext,
    pub expected_config_revision: Revision,
    pub account_id: ProviderAccountId,
}

/// Credential 写入提交结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialMutationResult {
    pub config_revision: Revision,
    pub account_id: ProviderAccountId,
    pub credential_revision: Option<Revision>,
}

/// Provider-owned token 轮换命令。
pub struct RotateCredential {
    pub mutation: CredentialMutation,
    pub expected_credential_revision: Revision,
    pub provider_material: ProviderDocument,
}

/// Provider 校验手工轮换材料时所需的非事务输入。
pub struct PrepareCredentialRotation {
    pub account: AccountRecord,
    pub expected_credential_revision: Revision,
    pub provider_material: ProviderDocument,
}

impl fmt::Debug for PrepareCredentialRotation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrepareCredentialRotation")
            .field("account", &self.account)
            .field(
                "expected_credential_revision",
                &self.expected_credential_revision,
            )
            .field("provider_material", &self.provider_material)
            .finish()
    }
}

/// Provider 已验证、可由 Store 以 credential revision CAS 原子提交的轮换 facts。
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCredentialRotationFacts {
    pub account_id: ProviderAccountId,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_kind: ProviderKind,
    pub expected_credential_revision: Revision,
    pub name: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub provider_material: ProviderDocument,
    pub has_refresh_token: bool,
    pub access_token_expires_at: DateTime<Utc>,
    pub next_refresh_at: Option<DateTime<Utc>>,
}

/// Provider 返回的轮换准备结果；guard 必须覆盖后续 Store CAS 与审计事务。
pub struct PreparedCredentialRotation {
    facts: PreparedCredentialRotationFacts,
    guard: Box<dyn CredentialCommitGuard>,
}

impl PreparedCredentialRotation {
    #[must_use]
    pub fn new(
        facts: PreparedCredentialRotationFacts,
        guard: Box<dyn CredentialCommitGuard>,
    ) -> Self {
        Self { facts, guard }
    }

    #[must_use]
    pub const fn facts(&self) -> &PreparedCredentialRotationFacts {
        &self.facts
    }

    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        PreparedCredentialRotationFacts,
        Box<dyn CredentialCommitGuard>,
    ) {
        (self.facts, self.guard)
    }
}

impl fmt::Debug for PreparedCredentialRotation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedCredentialRotation")
            .field("facts", &self.facts)
            .field("guard", &"[COMPLETION-GUARD]")
            .finish()
    }
}

/// Admin 交给 Store 的轮换或 refresh 事务命令。
#[derive(Debug, Clone, PartialEq)]
pub struct CredentialRotationCommit {
    pub expected_config_revision: Revision,
    pub prepared: PreparedCredentialRotationFacts,
}

impl fmt::Debug for RotateCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RotateCredential")
            .field("mutation", &self.mutation)
            .field(
                "expected_credential_revision",
                &self.expected_credential_revision,
            )
            .field("provider_material", &self.provider_material)
            .finish()
    }
}

/// 一个 Provider quota 窗口的公共投影。
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderQuotaWindow {
    pub key: String,
    pub group: String,
    pub source: Option<String>,
    pub window_seconds: Option<u64>,
    pub used_percent: Option<f64>,
    pub reset_at: Option<DateTime<Utc>>,
    pub provider_data: Option<ProviderDocument>,
}

/// Provider 已解析的 quota 结果及其不透明差异字段。
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderQuota {
    pub observed_at: Option<DateTime<Utc>>,
    pub refresh_token_expires_at: Option<DateTime<Utc>>,
    pub windows: Vec<ProviderQuotaWindow>,
    pub provider_data: Option<ProviderDocument>,
}

/// Provider 实时模型目录的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModel {
    pub id: UpstreamModelId,
    pub name: String,
}

/// Provider 实时模型目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModels {
    pub models: Vec<ProviderModel>,
    pub observed_at: Option<DateTime<Utc>>,
}

/// Provider 执行 refresh 时所需的当前公共账号事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareCredentialRefresh {
    pub account: AccountRecord,
}

/// Provider 敏感导出结果。
pub struct ProviderExport {
    pub provider_kind: ProviderKind,
    pub account_ids: Vec<ProviderAccountId>,
    pub document: ProviderDocument,
}

/// Store 为 Provider 导出序列化准备的最小输入；material 对公共层保持不透明。
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderExportCredentialInput {
    pub account: AccountRecord,
    pub provider_material: ProviderDocument,
}

impl fmt::Debug for ProviderExport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderExport")
            .field("provider_kind", &self.provider_kind)
            .field("account_ids", &self.account_ids)
            .field("document", &self.document)
            .finish()
    }
}

/// 统一账号目录的一行完整结果。
#[derive(Debug, Clone, PartialEq)]
pub struct AccountDirectoryItem {
    pub account: AccountRecord,
    pub provider_instance_name: String,
    pub status: AccountStatus,
    pub usage: Option<super::accounts::AccountUsage>,
    pub rolling_usage: Option<super::accounts::AccountUsage>,
    pub quota: ProviderQuota,
}

/// 统一账号目录页。
#[derive(Debug, Clone, PartialEq)]
pub struct AccountDirectoryPage {
    pub config_revision: Revision,
    pub items: Vec<AccountDirectoryItem>,
    pub total: u64,
    pub summary: AccountSummary,
}

/// 凭据刷新提交后的完整账号结果。
#[derive(Debug, Clone, PartialEq)]
pub struct AccountRefreshResult {
    pub config_revision: Revision,
    pub account: AccountDirectoryItem,
}

/// 多 Provider 导出文档集合。
pub struct AccountExportBundle {
    pub exported_at: DateTime<Utc>,
    pub documents: Vec<ProviderExport>,
}

impl fmt::Debug for AccountExportBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccountExportBundle")
            .field("exported_at", &self.exported_at)
            .field("document_count", &self.documents.len())
            .finish()
    }
}
