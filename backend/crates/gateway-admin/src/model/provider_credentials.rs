//! Provider 管理能力交换的中立 Command 与 Result。

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use gateway_core::{
    engine::credential::{OpaqueProviderData, ProviderAccountId, ProviderAccountIdentity},
    routing::{ProviderKind, UpstreamModelId},
};
use uuid::Uuid;

use super::{
    AdminError, MutationActor, MutationContext, PageSize, Revision,
    accounts::{AccountRecord, AccountSummary, AccountUsage, CredentialState},
    observability::CurrencyCost,
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
    pub credential_state: Option<CredentialStateFilter>,
    pub enabled: Option<bool>,
    pub window: CredentialListWindow,
}

/// Credential 状态筛选。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialStateFilter {
    Exact(CredentialState),
    AnyOf(Vec<CredentialState>),
}

impl CredentialStateFilter {
    #[must_use]
    pub fn matches(&self, credential_state: CredentialState) -> bool {
        match self {
            Self::Exact(expected) => *expected == credential_state,
            Self::AnyOf(expected) => expected.contains(&credential_state),
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
    pub document: ProviderDocument,
}

impl fmt::Debug for ImportCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImportCredentials")
            .field("context", &self.context)
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

/// Provider 解析导入文档时只接收不透明文档，不接触 revision 或审计上下文。
pub struct PrepareCredentialImport {
    pub document: ProviderDocument,
}

impl fmt::Debug for PrepareCredentialImport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrepareCredentialImport")
            .field("document", &self.document)
            .finish()
    }
}

/// Provider 已验证、可由 Store 原子创建的一份 credential。
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCredentialCreate {
    pub account_id: ProviderAccountId,
    pub provider_kind: ProviderKind,
    pub name: String,
    pub email: Option<String>,
    pub upstream_user_id: Option<String>,
    pub upstream_account_id: Option<String>,
    pub plan_type: Option<String>,
    pub authentication_kind: String,
    pub provider_material: ProviderDocument,
    pub has_refresh_token: bool,
    pub access_token_expires_at: Option<DateTime<Utc>>,
    pub next_refresh_at: Option<DateTime<Utc>>,
    pub enabled: bool,
    pub credential_state: CredentialState,
    pub credential_observed_at: DateTime<Utc>,
}

/// Provider 对一份导入文档的完整验证结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCredentialImport {
    pub provider_kind: ProviderKind,
    pub credentials: Vec<PreparedCredentialCreate>,
}

/// Admin 交给 Store 的导入事务命令。
#[derive(Debug, Clone, PartialEq)]
pub struct CredentialImportCommit {
    pub prepared: PreparedCredentialImport,
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
///
/// 重新授权只绑定稳定的账号身份：credential revision 由服务端在临近写入时读取，
/// 长流程期间的后台刷新不得让恢复操作失效。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizationMutationTarget {
    Create { name: String },
    Reauthorize { account_id: ProviderAccountId },
}

/// 必须完整进入 Provider opaque pending payload、并在 complete 后原样恢复的事务信封。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAuthorizationMutation {
    provider_kind: ProviderKind,
    target: AuthorizationMutationTarget,
    owner_binding: AuthorizationOwnerBinding,
}

impl PendingAuthorizationMutation {
    #[must_use]
    pub const fn new(
        provider_kind: ProviderKind,
        target: AuthorizationMutationTarget,
        owner_binding: AuthorizationOwnerBinding,
    ) -> Self {
        Self {
            provider_kind,
            target,
            owner_binding,
        }
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
    pub name: String,
    pub reauthorization: Option<ProviderAccountId>,
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

/// OAuth pending claim 在 Store 事务结果确定后执行的显式结算动作。
///
/// OAuth provider 必须在 credential 准备阶段持有 claim：Store 提交成功后消费 flow，提交或
/// 校验失败后释放 flow。异步结算不能依赖 `Drop`，否则请求提前返回会让同一授权流无谓地
/// 进入短暂的不可重试状态。
#[async_trait]
pub trait AuthorizationCommitGuard: Send + 'static {
    /// Store 已提交 OAuth credential 后消费对应的一次性 flow。
    async fn commit(self: Box<Self>) -> Result<(), AdminError>;

    /// Store 未提交 OAuth credential 时释放 claim，允许同一 flow 重试。
    async fn abort(self: Box<Self>) -> Result<(), AdminError>;
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
    authorization_guard: Option<Box<dyn AuthorizationCommitGuard>>,
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

/// OAuth 准备结果拆解后的 Store 命令与两个结算 guard。
///
/// Store 成功时先结束 credential guard，再消费 OAuth claim；失败时丢弃 credential guard 并
/// 释放 OAuth claim，使同一授权流可以重试。
pub(crate) struct AuthorizationCommitSettlement {
    pub(crate) command: AuthorizationCommit,
    pub(crate) credential_guard: Option<Box<dyn CredentialCommitGuard>>,
    pub(crate) authorization_guard: Option<Box<dyn AuthorizationCommitGuard>>,
}

impl PreparedAuthorizationCommit {
    #[must_use]
    pub fn new(
        pending: PendingAuthorizationMutation,
        credential: PreparedAuthorizationCredential,
    ) -> Self {
        Self {
            pending,
            credential,
            authorization_guard: None,
        }
    }

    /// 让 OAuth claim 覆盖准备完成到 Store 事务结算之间的全部窗口。
    #[must_use]
    pub fn with_authorization_guard(mut self, guard: Box<dyn AuthorizationCommitGuard>) -> Self {
        self.authorization_guard = Some(guard);
        self
    }

    pub(crate) fn into_commit(self) -> AuthorizationCommitSettlement {
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
        AuthorizationCommitSettlement {
            command: AuthorizationCommit {
                pending: self.pending,
                credential,
            },
            credential_guard: guard,
            authorization_guard: self.authorization_guard,
        }
    }

    pub(crate) async fn abort(self) -> Result<(), AdminError> {
        let AuthorizationCommitSettlement {
            credential_guard,
            authorization_guard,
            ..
        } = self.into_commit();
        drop(credential_guard);
        if let Some(guard) = authorization_guard {
            guard.abort().await?;
        }
        Ok(())
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
    pub account_id: ProviderAccountId,
}

/// Credential 写入提交结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialMutationResult {
    pub config_revision: Revision,
    pub account_id: ProviderAccountId,
    pub credential_revision: Option<Revision>,
}

/// 同一 Provider 管理范围内的 credential 批量删除。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialDeletion {
    pub context: MutationContext,
    pub account_ids: Vec<ProviderAccountId>,
}

/// Credential 批量删除提交结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialDeletionResult {
    pub config_revision: Revision,
    pub account_ids: Vec<ProviderAccountId>,
}

/// Provider-owned token 轮换命令。
pub struct RotateCredential {
    pub mutation: CredentialMutation,
    pub provider_material: ProviderDocument,
}

/// Provider 校验手工轮换材料时所需的非事务输入。
pub struct PrepareCredentialRotation {
    pub account: AccountRecord,
    pub provider_material: ProviderDocument,
}

impl fmt::Debug for PrepareCredentialRotation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrepareCredentialRotation")
            .field("account", &self.account)
            .field("provider_material", &self.provider_material)
            .finish()
    }
}

/// Provider 已验证、可由 Store 以 credential revision CAS 原子提交的轮换 facts。
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCredentialRotationFacts {
    pub account_id: ProviderAccountId,
    pub provider_kind: ProviderKind,
    pub expected_credential_revision: Revision,
    pub replacement_identity: Option<ProviderAccountIdentity>,
    pub name: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub provider_material: ProviderDocument,
    pub has_refresh_token: bool,
    pub access_token_expires_at: Option<DateTime<Utc>>,
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
    pub prepared: PreparedCredentialRotationFacts,
}

impl fmt::Debug for RotateCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RotateCredential")
            .field("mutation", &self.mutation)
            .field("provider_material", &self.provider_material)
            .finish()
    }
}

/// 通用账号用量能否可靠归属到一个 Provider quota 窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaLocalUsageAttribution {
    /// 窗口覆盖账号的全部请求，可按账号与时间范围聚合。
    AccountWide,
    /// 窗口需要 Provider / 模型级归属，通用账号聚合不可用。
    Unavailable,
}

/// Provider quota bucket 中窗口的官方位置语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderQuotaWindowRole {
    Primary,
    Secondary,
    Monthly,
}

impl ProviderQuotaWindowRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Secondary => "secondary",
            Self::Monthly => "monthly",
        }
    }
}

/// 一个 Provider quota 窗口的公共投影。
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderQuotaWindow {
    pub key: String,
    pub group: String,
    pub label: String,
    pub limit_id: Option<String>,
    pub limit_name: Option<String>,
    pub role: Option<ProviderQuotaWindowRole>,
    pub local_usage_attribution: QuotaLocalUsageAttribution,
    pub window_seconds: Option<u64>,
    pub used_percent: Option<f64>,
    pub reset_at: Option<DateTime<Utc>>,
    pub limit_reached: bool,
    pub local_usage: Option<AccountUsage>,
    pub provider_data: Option<ProviderDocument>,
}

/// Provider 解释 quota 所需的公共请求事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderQuotaRequest {
    pub account_id: ProviderAccountId,
    pub refresh: bool,
    pub rolling_usage: Option<AccountUsage>,
}

/// Provider 已解析的 quota 结果及其不透明差异字段。
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderQuota {
    pub observed_at: Option<DateTime<Utc>>,
    pub refresh_token_expires_at: Option<DateTime<Utc>>,
    pub windows: Vec<ProviderQuotaWindow>,
    /// 展示用快照级触顶事实（顶层或任一窗口触顶）；不参与账号五态派生。
    pub limit_reached: bool,
    pub provider_data: Option<ProviderDocument>,
}

/// Provider 官方用量统计的账号形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderUsageStatisticsMode {
    Workspace,
    Personal,
}

impl ProviderUsageStatisticsMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Personal => "personal",
        }
    }
}

/// Provider 官方用量统计的目标额度周期。
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsageStatisticsCycle {
    pub offset: i8,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub window_seconds: u64,
    pub used_percent: Option<f64>,
    pub is_current: bool,
    pub can_go_previous: bool,
    pub can_go_next: bool,
}

/// Provider 官方用量统计查询。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUsageStatisticsRequest {
    pub account_id: ProviderAccountId,
    /// `0` 为当前周期，负数为历史周期。
    pub cycle_offset: i8,
    /// 客户端相对 UTC 的分钟偏移，例如 UTC+8 为 `480`。
    pub utc_offset_minutes: i16,
}

/// 官方用量统计中的 Token 分类。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderUsageStatisticsTokens {
    pub uncached_input: u64,
    pub cached_input: u64,
    pub output: u64,
    pub total: u64,
}

/// 官方模型报表中的服务档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderUsageStatisticsServiceTier {
    Standard,
    Fast,
}

impl ProviderUsageStatisticsServiceTier {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Fast => "fast",
        }
    }
}

/// 一个模型与服务档位在目标周期内的统计行。
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsageStatisticsModel {
    pub key: String,
    pub model: String,
    pub service_tier: ProviderUsageStatisticsServiceTier,
    pub credit_share: Option<f64>,
    pub quota_share: Option<f64>,
    pub tokens: ProviderUsageStatisticsTokens,
    pub estimated_cost: Option<CurrencyCost>,
    pub has_unknown_pricing: bool,
    pub has_estimated_allocation: bool,
    pub has_rate_fallback: bool,
    pub has_missing_token_data: bool,
}

/// 目标周期内的一天；官方日报只能提供本地日粒度。
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsageStatisticsDay {
    pub date: NaiveDate,
    pub credit_share: Option<f64>,
    pub tokens: ProviderUsageStatisticsTokens,
    pub estimated_cost: Option<CurrencyCost>,
    pub has_unknown_pricing: bool,
    pub has_missing_token_data: bool,
    pub is_boundary_day: bool,
}

/// 目标周期汇总。
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsageStatisticsSummary {
    pub tokens: ProviderUsageStatisticsTokens,
    pub estimated_cost: Option<CurrencyCost>,
    pub projected_tokens: Option<u64>,
    pub projected_cost: Option<CurrencyCost>,
    pub day_count: u32,
    pub has_unknown_pricing: bool,
    pub has_missing_token_data: bool,
}

/// Provider 已解释、计算并排序的官方用量统计。
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsageStatistics {
    pub mode: ProviderUsageStatisticsMode,
    pub cycle: ProviderUsageStatisticsCycle,
    pub summary: ProviderUsageStatisticsSummary,
    pub models: Vec<ProviderUsageStatisticsModel>,
    pub daily: Vec<ProviderUsageStatisticsDay>,
}

/// Provider 返回的一张安全主动额度重置卡。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResetCredit {
    pub id: String,
    pub status: Option<String>,
    pub title: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub reset_type: Option<String>,
}

/// Provider 主动额度重置卡列表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResetCredits {
    pub available_count: u64,
    pub credits: Vec<ProviderResetCredit>,
}

/// 一次主动额度重置卡消费命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumeProviderResetCredit {
    pub account_id: ProviderAccountId,
    pub credit_id: Option<String>,
    pub redeem_request_id: Uuid,
}

/// Provider 返回的主动额度重置卡消费结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResetCreditResult {
    pub code: String,
    pub credit: Option<ProviderResetCredit>,
}

impl ProviderQuota {
    /// 返回账号展开面板使用的当前额度窗口用量。
    ///
    /// Provider 已提供账号级本地用量时，固定窗口和无固定重置点的滚动窗口
    /// 都可作为代表用量；优先级与 Dashboard 代表性额度窗口一致，
    /// 同一优先级保持 Provider 投影顺序。
    #[must_use]
    pub fn representative_window_usage(&self) -> Option<&AccountUsage> {
        self.windows
            .iter()
            .enumerate()
            .filter(|(_, window)| {
                window.local_usage_attribution == QuotaLocalUsageAttribution::AccountWide
                    && window.window_seconds.is_some()
                    && window.local_usage.is_some()
            })
            .min_by_key(|(index, window)| (quota_usage_priority(window), *index))
            .and_then(|(_, window)| window.local_usage.as_ref())
    }

    /// 返回 Dashboard 使用的代表性额度比例。
    ///
    /// 优先使用覆盖账号全部请求的窗口；同一归属范围内再依次使用短周期、周、月
    /// 和其它窗口，同一优先级取较高的已用比例。这里只解释跨 Provider 共享的窗口
    /// 语义，绝不读取 Provider 私有 JSON。
    #[must_use]
    pub fn representative_used_percent(&self) -> Option<f64> {
        if self.limit_reached {
            return Some(100.0);
        }
        self.representative_used_window()
            .map(|(_, used_percent)| used_percent)
    }

    /// 将已确认的账号级额度耗尽事实投影到展示窗口。
    ///
    /// Provider 已指出具体触顶窗口时只归一化这些窗口；否则归一化 Dashboard
    /// 同样会选择的代表窗口，避免把多个独立额度窗口全部伪造成已用尽。
    pub fn apply_limit_reached_display(&mut self) {
        if !self.limit_reached {
            return;
        }
        let reached_windows = self
            .windows
            .iter()
            .enumerate()
            .filter_map(|(index, window)| window.limit_reached.then_some(index))
            .collect::<Vec<_>>();
        if !reached_windows.is_empty() {
            for index in reached_windows {
                self.windows[index].used_percent = Some(100.0);
            }
            return;
        }
        let representative = self
            .representative_used_window()
            .map(|(index, _)| index)
            .or_else(|| {
                self.windows
                    .iter()
                    .enumerate()
                    .min_by_key(|(index, window)| (quota_usage_priority(window), *index))
                    .map(|(index, _)| index)
            });
        if let Some(index) = representative {
            self.windows[index].used_percent = Some(100.0);
            self.windows[index].limit_reached = true;
        }
    }

    fn representative_used_window(&self) -> Option<(usize, f64)> {
        self.windows
            .iter()
            .enumerate()
            .filter_map(|window| {
                let (index, window) = window;
                let used_percent = window
                    .used_percent
                    .filter(|value| value.is_finite())
                    .map(|value| value.clamp(0.0, 100.0))?;
                Some((index, quota_usage_priority(window), used_percent))
            })
            .fold(
                None::<(usize, (u8, u8), f64)>,
                |selected, candidate| match selected {
                    Some(current)
                        if current.1 < candidate.1
                            || (current.1 == candidate.1 && current.2 >= candidate.2) =>
                    {
                        Some(current)
                    }
                    _ => Some(candidate),
                },
            )
            .map(|(index, _, used_percent)| (index, used_percent))
    }
}

fn quota_usage_priority(window: &ProviderQuotaWindow) -> (u8, u8) {
    let attribution = match window.local_usage_attribution {
        QuotaLocalUsageAttribution::AccountWide => 0,
        QuotaLocalUsageAttribution::Unavailable => 1,
    };
    let duration = match window.group.as_str() {
        "shortTerm" if window.window_seconds.is_some_and(is_week_window) => 1,
        "shortTerm" => 0,
        "monthly" => 2,
        _ => 3,
    };
    (attribution, duration)
}

fn is_week_window(seconds: u64) -> bool {
    const WEEK_SECONDS: u64 = 7 * 24 * 60 * 60;
    seconds > 0 && seconds.abs_diff(WEEK_SECONDS) <= WEEK_SECONDS / 20
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
    pub projection: gateway_core::engine::credential::AccountStatusProjection,
    pub usage: Option<super::accounts::AccountUsage>,
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
