use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gateway_core::engine::credential::{
    AccountEligibilityPolicy, AccountSelectionPolicy, CredentialRevision, ProviderAccountId,
};
use gateway_core::engine::provider::ProviderResource;
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::{FrozenAccountScope, UpstreamModelId};
use sha2::{Digest as _, Sha256};

use crate::SecretValue;

/// 注入的推理 transport 可识别的不透明假名化 egress/session 键。
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct GrokSessionBinding(String);

impl GrokSessionBinding {
    /// 创建有界的非敏感绑定引用。
    ///
    /// # Errors
    ///
    /// 值为空、超长、含控制字符或使用保留前缀时返回错误。
    pub fn new(value: impl Into<String>) -> Result<Self, GrokSessionDataError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || value.starts_with("__")
            || value.chars().any(char::is_control)
        {
            return Err(GrokSessionDataError::InvalidBinding);
        }
        Ok(Self(value))
    }

    /// 返回假名化的 transport 查找键。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for GrokSessionBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GrokSessionBinding([PSEUDONYM])")
    }
}

/// 下游租户与显式会话共同派生的账号亲和键。
#[derive(Clone, PartialEq, Eq)]
pub struct GrokSessionAffinityKey([u8; 32]);

impl GrokSessionAffinityKey {
    pub(crate) const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    /// 为候选账号计算稳定 rendezvous 分值，原始会话值不会离开 Provider。
    pub(crate) fn score(&self, account_id: &ProviderAccountId) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"xai-account-affinity-v1\0");
        hasher.update(self.0);
        hasher.update(b"\0");
        hasher.update(account_id.as_str().as_bytes());
        hasher.finalize().into()
    }
}

impl fmt::Debug for GrokSessionAffinityKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GrokSessionAffinityKey([PSEUDONYM])")
    }
}

/// 由 selector 持有的凭据、容量与 egress 亲和守卫。
pub trait GrokSessionLeaseGuard: Send + Sync + 'static {}

impl<T> GrokSessionLeaseGuard for T where T: Send + Sync + 'static {}

/// 一个选中的 OAuth 会话及其持有的活跃 lease。
pub struct SelectedGrokSession {
    account_id: ProviderAccountId,
    credential_revision: CredentialRevision,
    access_token: SecretValue,
    user_id: SecretValue,
    email: Option<SecretValue>,
    binding: GrokSessionBinding,
    allows_account_state_mutation: bool,
    _guard: Box<dyn GrokSessionLeaseGuard>,
}

impl SelectedGrokSession {
    /// 从 Provider 持有的明文 OAuth 材料构造选中会话。
    ///
    /// # Errors
    ///
    /// 快照缺少凭据，或 auth/header 值格式非法时返回错误。
    pub fn new(
        account_id: ProviderAccountId,
        credential_revision: CredentialRevision,
        access_token: SecretValue,
        user_id: SecretValue,
        email: Option<SecretValue>,
        binding: GrokSessionBinding,
        guard: impl GrokSessionLeaseGuard,
    ) -> Result<Self, GrokSessionDataError> {
        if !valid_secret_header(&access_token, 64 * 1024)
            || !valid_secret_header(&user_id, 1_024)
            || email
                .as_ref()
                .is_some_and(|value| !valid_secret_header(value, 1_024))
        {
            return Err(GrokSessionDataError::InvalidSecretValue);
        }
        Ok(Self {
            account_id,
            credential_revision,
            access_token,
            user_id,
            email,
            binding,
            allows_account_state_mutation: true,
            _guard: Box::new(guard),
        })
    }

    /// 标记禁用账号的管理端诊断只读取真实上游结果，不回写账号状态。
    #[must_use]
    pub(crate) const fn without_account_state_mutation(mut self) -> Self {
        self.allows_account_state_mutation = false;
        self
    }

    /// 返回选中的账号 ID。
    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    /// 返回 selector 冻结的凭据 revision。
    #[must_use]
    pub const fn credential_revision(&self) -> CredentialRevision {
        self.credential_revision
    }

    /// 返回 Core 为本次上游调用记录的元数据。
    #[must_use]
    pub fn resource(&self) -> ProviderResource {
        ProviderResource::Account {
            id: self.account_id.clone(),
            revision: self.credential_revision,
        }
    }

    /// 返回用于显式构造 header 的 OAuth access token。
    #[must_use]
    pub const fn access_token(&self) -> &SecretValue {
        &self.access_token
    }

    /// 返回用于官方代理 header 的已验证 user ID。
    #[must_use]
    pub const fn user_id(&self) -> &SecretValue {
        &self.user_id
    }

    /// 返回用于官方代理 header 的可选已验证 email。
    #[must_use]
    pub const fn email(&self) -> Option<&SecretValue> {
        self.email.as_ref()
    }

    /// 返回假名化的 egress/session transport 绑定。
    #[must_use]
    pub const fn binding(&self) -> &GrokSessionBinding {
        &self.binding
    }

    #[must_use]
    pub(crate) const fn allows_account_state_mutation(&self) -> bool {
        self.allows_account_state_mutation
    }
}

impl fmt::Debug for SelectedGrokSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SelectedGrokSession")
            .field("account_id", &self.account_id)
            .field("credential_revision", &self.credential_revision)
            .field("access_token", &"[REDACTED]")
            .field("user_id", &"[REDACTED]")
            .field("email", &self.email.as_ref().map(|_| "[REDACTED]"))
            .field("binding", &self.binding)
            .field(
                "allows_account_state_mutation",
                &self.allows_account_state_mutation,
            )
            .field("guard", &"[LEASE]")
            .finish()
    }
}

/// 单次 selector 调用的入参，持有冻结且不含密钥的尝试视图。
#[derive(Debug, Clone)]
pub struct GrokSessionSelection {
    upstream_model: UpstreamModelId,
    excluded_accounts: BTreeSet<ProviderAccountId>,
    required_account: Option<ProviderAccountId>,
    account_selection_policy: AccountSelectionPolicy,
    eligibility: AccountEligibilityPolicy,
    affinity: Option<GrokSessionAffinityKey>,
    deadline: SystemTime,
    account_scope: Arc<FrozenAccountScope>,
    client_api_key_id: ClientApiKeyId,
}

impl GrokSessionSelection {
    /// 创建不可变的选择请求。
    #[must_use]
    pub fn new(
        upstream_model: UpstreamModelId,
        excluded_accounts: BTreeSet<ProviderAccountId>,
        required_account: Option<ProviderAccountId>,
        account_selection_policy: AccountSelectionPolicy,
        deadline: SystemTime,
        account_scope: Arc<FrozenAccountScope>,
        client_api_key_id: ClientApiKeyId,
    ) -> Self {
        Self {
            upstream_model,
            excluded_accounts,
            required_account,
            account_selection_policy,
            eligibility: AccountEligibilityPolicy::Enforce,
            affinity: None,
            deadline,
            account_scope,
            client_api_key_id,
        }
    }

    /// 附着仅由显式客户端会话派生的账号亲和键。
    #[must_use]
    pub fn with_affinity(mut self, affinity: Option<GrokSessionAffinityKey>) -> Self {
        self.affinity = affinity;
        self
    }

    /// 为固定账号的管理端诊断指定本地可用性判定策略。
    #[must_use]
    pub(crate) const fn with_eligibility_policy(
        mut self,
        eligibility: AccountEligibilityPolicy,
    ) -> Self {
        self.eligibility = eligibility;
        self
    }

    /// 返回冻结的上游模型。
    #[must_use]
    pub const fn upstream_model(&self) -> &UpstreamModelId {
        &self.upstream_model
    }

    /// 返回协调方已尝试过的账号。
    #[must_use]
    pub const fn excluded_accounts(&self) -> &BTreeSet<ProviderAccountId> {
        &self.excluded_accounts
    }

    /// 返回 Core 约束下本次调用唯一允许的账号。
    #[must_use]
    pub const fn required_account(&self) -> Option<&ProviderAccountId> {
        self.required_account.as_ref()
    }

    /// 返回冻结的全局账号调度策略。
    #[must_use]
    pub const fn account_selection_policy(&self) -> AccountSelectionPolicy {
        self.account_selection_policy
    }

    #[must_use]
    pub const fn eligibility(&self) -> AccountEligibilityPolicy {
        self.eligibility
    }

    /// 返回不含原始客户端会话值的账号亲和键。
    #[must_use]
    pub const fn affinity(&self) -> Option<&GrokSessionAffinityKey> {
        self.affinity.as_ref()
    }

    /// 返回限定调度租约的绝对截止时间。
    #[must_use]
    pub const fn deadline(&self) -> SystemTime {
        self.deadline
    }

    #[must_use]
    pub const fn account_scope(&self) -> &Arc<FrozenAccountScope> {
        &self.account_scope
    }

    #[must_use]
    pub const fn client_api_key_id(&self) -> &ClientApiKeyId {
        &self.client_api_key_id
    }
}

/// Grok session selector 返回的 future。
pub type GrokSessionSelectorFuture<'a> = Pin<
    Box<dyn Future<Output = Result<SelectedGrokSession, GrokSessionSelectorError>> + Send + 'a>,
>;

/// 已脱敏的上游失败反馈；selector 决定账号持久状态或细粒度运行时 cooldown。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrokCredentialFailure {
    /// 选中的 OAuth token 被拒绝。
    Unauthorized,
    /// 选中的 OAuth 账号被限流。
    RateLimited {
        /// 已解析并脱敏的延迟；不保留上游原始 header。
        retry_after: Option<Duration>,
    },
    /// 选中的 OAuth 账号付费额度耗尽。
    QuotaExhausted,
    /// 选中的 OAuth 账号订阅级免费额度耗尽。
    FreeQuotaExhausted,
    /// HTTP 402 没有给出可证明账号额度耗尽的稳定信号。
    PaymentRequired {
        /// 已解析并脱敏的延迟；缺失时由 selector 使用短暂默认值。
        retry_after: Option<Duration>,
    },
    /// 上游声明指定模型的免费用量耗尽；selector 会收敛为账号级可恢复额度状态。
    ModelQuotaExhausted {
        upstream_model: UpstreamModelId,
        retry_after: Option<Duration>,
    },
    /// 选中账号只缺少指定模型的访问权限。
    ModelAccessDenied {
        upstream_model: UpstreamModelId,
        retry_after: Option<Duration>,
    },
    /// 成功的 SSE 响应在终止 Responses 事件前结束。
    StreamInterrupted,
}

/// 一次 best-effort 凭据反馈写入返回的 future。
pub type GrokCredentialFeedbackFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// 运行时端口：选出恰好一个可用 OAuth 会话，并获取流生命周期所需的全部
/// 凭据、容量与出口租约。
pub trait GrokSessionSelector: Send + Sync {
    /// 执行一次选择，不做 Provider 内部回退。
    fn select(&self, request: GrokSessionSelection) -> GrokSessionSelectorFuture<'_>;

    /// 记录一条已分类的失败反馈，不重试也不替换原错误。
    fn record_failure<'a>(
        &'a self,
        session: &'a SelectedGrokSession,
        failure: GrokCredentialFailure,
    ) -> GrokCredentialFeedbackFuture<'a>;

    /// 成功完成真实上游请求后收敛账号的可恢复状态。
    ///
    /// 默认实现让只关心选择或失败反馈的测试替身无需模拟持久状态写入。
    fn record_success<'a>(
        &'a self,
        _: &'a SelectedGrokSession,
    ) -> GrokCredentialFeedbackFuture<'a> {
        Box::pin(async {})
    }
}

/// 不含密钥的选择器失败。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GrokSessionSelectorError {
    /// 没有会话同时满足模型、状态与排除约束。
    #[error("no eligible Grok Build session is available")]
    NoEligibleSession,
    /// 所有可用会话当前均被租出或处于限速间隔。
    #[error("Grok Build session capacity is unavailable")]
    CapacityUnavailable {
        /// 选择器推导出的可选延迟。
        retry_after: Option<Duration>,
    },
    /// 每个可用会话都处于运行时冷却窗口内。
    #[error("Grok Build account is cooling down")]
    AccountCoolingDown {
        /// 最早恢复的会话剩余冷却时长。
        retry_after: Option<Duration>,
    },
    /// 每个可用会话都只针对当前模型处于运行时冷却窗口内。
    #[error("Grok Build model is cooling down for the available accounts")]
    ModelCoolingDown {
        /// 最早恢复的账号+模型组合剩余冷却时长。
        retry_after: Option<Duration>,
    },
    /// 会话元数据或 Provider 持有的明文密钥非法。
    #[error("Grok Build session data is invalid")]
    InvalidSession,
    /// 选择器依赖的后端服务不可用。
    #[error("Grok Build session selector is unavailable")]
    Unavailable,
}

/// 构造选中会话时的失败。
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum GrokSessionDataError {
    /// access token 或已验证身份头格式非法。
    #[error("selected session contains an invalid secret value")]
    InvalidSecretValue,
    /// 出口/会话绑定非法。
    #[error("selected session binding is invalid")]
    InvalidBinding,
}

fn valid_secret_header(value: &SecretValue, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value
            .expose()
            .bytes()
            .all(|byte| (0x20..=0x7e).contains(&byte))
}
