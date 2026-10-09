//! AttemptContext 驱动的 Codex 账号选择与 Redis lease port

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use gateway_core::error::ErrorSource;

use gateway_core::account::scope::FrozenAccountScope;
use gateway_core::account::{
    AccountAffinity, AccountCandidate, AccountCapacitySnapshot, AccountEligibilityPolicy,
    AccountErrorReason, AccountFeedbackStats, AccountModelAccessMode, AccountRuntimeSignals,
    AccountSchedulingBlocker, AccountSelectionContext, AccountSelectionPolicy, AccountSelector,
    AccountStatus, CredentialState, PreferredAccountSelection, ProviderAccount, ProviderAccountId,
    QuotaEvidence,
};
use gateway_core::concurrency::{CapacityWait, ConcurrencyWaitQueue, QueueRejection, WaitPriority};
use gateway_core::engine::{AttemptContext, ContinuationAttempt, policy::AccountPolicyError};
use gateway_core::provider_ports::{
    ProviderConcurrencyPool, ProviderLeaseAcquisition, ProviderLeaseGuard, ProviderLeasePort,
    ProviderLeaseRequest, ProviderSchedulingLeaseRequest, ProviderSessionAffinityKey,
    ProviderSessionAffinityPort, ProviderSessionBinding, ProviderSessionExclusionPort,
    ProviderSessionExclusions, ProviderStoreError,
};
use gateway_core::routing::ProviderKind;
use secrecy::ExposeSecret;
use thiserror::Error;
use url::Url;

use super::affinity::CodexSessionAffinity;
use super::cookie::CodexCookiePolicy;
use super::quota::CodexCredentialQuotaService;
use super::refresh::refresh_recovery_deadline;
use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use super::security::CodexRuntimeAuthentication;
use super::types::{
    CODEX_AUTHENTICATION_KIND_OAUTH, CodexCookie, CodexCookieCaptureOutcome, RuntimeCodexCookie,
};

const CLOUDFLARE_RECOVERY_STALE_AFTER: Duration = Duration::from_secs(60 * 60);
const CLOUDFLARE_CHALLENGE_BACKOFF: [Duration; 4] = [
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(90),
    Duration::from_secs(120),
];
const CLOUDFLARE_PATH_BLOCK_THRESHOLD: u32 = 3;
const SESSION_AFFINITY_TIMEOUT: Duration = Duration::from_millis(100);
const CYBER_POLICY_SESSION_TTL: Duration = Duration::from_secs(60 * 60);
const MAX_ACCOUNT_SNAPSHOT_RETRIES: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAccountFailure {
    /// Access token 已被上游明确判定为过期或失效
    CredentialExpired,
    /// 账号需要完成身份验证后才能继续使用
    IdentityVerificationRequired,
    /// 账号、workspace 或 organization 已被封禁或停用
    Banned,
    /// 账号信用额度已耗尽
    QuotaExhausted,
    /// 当前用量窗口已耗尽；到重置时间后可自动恢复
    UsageLimitExhausted {
        /// 上游返回的窗口绝对重置时刻
        reset_at: Option<SystemTime>,
    },
    /// 账号触发临时用量限制
    RateLimited {
        /// 上游明确返回的最短冷却时长
        retry_after: Option<Duration>,
    },
    /// Cloudflare challenge 要求账号进入递增冷却
    CloudflareChallenge {
        /// 上游明确返回的最短冷却时长
        retry_after: Option<Duration>,
    },
    /// Cloudflare 对当前上游路径返回空 404
    CloudflarePathBlocked,
}

#[derive(Debug, Clone, Copy)]
struct RiskRecoveryState {
    challenge_count: u32,
    path_block_count: u32,
    observed_at: SystemTime,
}

#[derive(Debug, Clone, Copy)]
enum CookieRecovery {
    ExpireAt(SystemTime),
    Clear,
}

pub struct SelectCodexCredential<'a> {
    pub upstream_model: &'a str,
    pub request_url: &'a Url,
    pub attempt: &'a AttemptContext,
    pub session_affinity_key: Option<&'a ProviderSessionAffinityKey>,
}

pub(crate) struct SelectCodexProviderEndpointCredential<'a> {
    pub request_url: &'a Url,
    pub attempt: &'a AttemptContext,
    pub session_affinity: Option<&'a CodexSessionAffinity>,
    pub model: CodexSelectionModel<'a>,
    /// 本端点只接受 OAuth 凭据时排除 API Key 账号，避免混合池选中后必然失败。
    pub requires_oauth: bool,
}

/// 选号使用的模型类别，区分图片资格与不涉及模型的端点
#[derive(Clone, Copy)]
pub(crate) enum CodexSelectionModel<'a> {
    NotApplicable,
    Requested(&'a str),
    Image(Option<&'a str>),
}

impl<'a> CodexSelectionModel<'a> {
    pub(crate) const fn model(self) -> Option<&'a str> {
        match self {
            Self::NotApplicable => None,
            Self::Requested(model) => Some(model),
            Self::Image(model) => model,
        }
    }

    pub(crate) fn allows_account(self, account: &ProviderAccount) -> bool {
        // 官方 Codex 的 image_generation_available 排除 Free，API Key 不套用 ChatGPT 套餐
        // https://github.com/openai/codex/commit/0a0a9b6c8f
        !(matches!(self, Self::Image(_))
            && account.authentication_kind() == CODEX_AUTHENTICATION_KIND_OAUTH
            && account
                .plan_type()
                .is_some_and(|plan| plan.trim().eq_ignore_ascii_case("free")))
    }

    pub(crate) fn allows(self, scope: &FrozenAccountScope, account: &ProviderAccountId) -> bool {
        match self {
            Self::NotApplicable => scope.allows(account),
            Self::Requested(model) | Self::Image(Some(model)) => scope.allows_model(account, model),
            // 缺失、重复或不可解析的图片模型不能绕过黑白名单；不猜上游默认值
            Self::Image(None) => {
                scope.allows(account)
                    && scope.directory().account(account).is_some_and(|account| {
                        account.model_access().mode() == AccountModelAccessMode::All
                    })
            }
        }
    }
}

struct CredentialSelectionInput<'a> {
    requires_websocket: bool,
    requires_oauth: bool,
    request_url: &'a Url,
    attempt: &'a AttemptContext,
    session_affinity_key: Option<&'a ProviderSessionAffinityKey>,
    session_affinity_observation: Option<&'a CodexSessionAffinity>,
    /// Codex Guardian 自动审批请求；仅在配置独立额度时使用审批容量池
    guardian: bool,
}

#[derive(Clone)]
pub(crate) struct CodexCyberPolicyScope {
    key: ProviderSessionAffinityKey,
    state: Option<ProviderSessionExclusions>,
}

pub struct CodexCredentialSelector {
    waiting: ConcurrencyWaitQueue<ProviderAccountId>,
    reserved_waiting: ConcurrencyWaitQueue<ProviderAccountId>,
    provider_kind: ProviderKind,
    repository: CodexCredentialRepository,
    leases: Arc<dyn ProviderLeasePort>,
    session_affinity: Arc<dyn ProviderSessionAffinityPort>,
    session_exclusions: Arc<dyn ProviderSessionExclusionPort>,
    quota: Arc<CodexCredentialQuotaService>,
    cookie_policy: CodexCookiePolicy,
    risk_recovery: Mutex<HashMap<String, RiskRecoveryState>>,
    account_feedback: Arc<AccountFeedbackStats>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AffinityEscapeReason {
    HardUnavailable,
    QuotaExhausted,
    Cooldown,
    LeaseSaturated,
    HigherPriority,
    PinnedAccount,
    SchedulingPolicy,
    SelectionInvariant,
}

impl AffinityEscapeReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::HardUnavailable => "hard_unavailable",
            Self::QuotaExhausted => "quota_exhausted",
            Self::Cooldown => "cooldown",
            Self::LeaseSaturated => "lease_saturated",
            Self::HigherPriority => "higher_priority",
            Self::PinnedAccount => "pinned_account",
            Self::SchedulingPolicy => "scheduling_policy",
            Self::SelectionInvariant => "selection_invariant",
        }
    }
}

#[derive(Debug, Default)]
struct AffinitySelection {
    bound_account: Option<ProviderAccountId>,
    preferred_account: Option<ProviderAccountId>,
    escape_reason: Option<AffinityEscapeReason>,
}

impl AffinitySelection {
    fn preferred(account_id: ProviderAccountId) -> Self {
        Self {
            bound_account: Some(account_id.clone()),
            preferred_account: Some(account_id),
            escape_reason: None,
        }
    }

    fn escaped(account_id: ProviderAccountId, reason: AffinityEscapeReason) -> Self {
        Self {
            bound_account: Some(account_id),
            preferred_account: None,
            escape_reason: Some(reason),
        }
    }

    fn bound_account(&self) -> Option<&ProviderAccountId> {
        self.bound_account.as_ref()
    }

    fn preferred_account(&self) -> Option<&ProviderAccountId> {
        self.preferred_account.as_ref()
    }

    fn escape(&mut self, reason: AffinityEscapeReason) {
        if self.bound_account.is_some() && self.escape_reason.is_none() {
            self.escape_reason = Some(reason);
            self.preferred_account = None;
        }
    }

    fn observe_preferred_selection(&mut self, selection: PreferredAccountSelection) {
        if self.bound_account.is_none() || self.escape_reason.is_some() {
            return;
        }
        match selection {
            PreferredAccountSelection::Hit => {}
            PreferredAccountSelection::OverriddenByPolicy => {
                self.escape(AffinityEscapeReason::SchedulingPolicy);
            }
            PreferredAccountSelection::Blocked(AccountSchedulingBlocker::ConcurrencyLimit) => {
                self.escape(AffinityEscapeReason::LeaseSaturated);
            }
            PreferredAccountSelection::Blocked(AccountSchedulingBlocker::RequestInterval) => {
                self.escape(AffinityEscapeReason::Cooldown);
            }
            PreferredAccountSelection::Blocked(AccountSchedulingBlocker::LowerWeight) => {
                self.escape(AffinityEscapeReason::HigherPriority);
            }
            PreferredAccountSelection::Blocked(
                AccountSchedulingBlocker::LocalAvailability
                | AccountSchedulingBlocker::Excluded
                | AccountSchedulingBlocker::OutsideClientScope,
            )
            | PreferredAccountSelection::Missing => {
                self.escape(AffinityEscapeReason::HardUnavailable);
            }
            PreferredAccountSelection::NotRequested => {
                self.escape(AffinityEscapeReason::SelectionInvariant);
            }
        }
    }

    fn observe_lease_busy(&mut self, account_id: &ProviderAccountId) {
        if self.bound_account.as_ref() == Some(account_id) {
            self.escape(AffinityEscapeReason::LeaseSaturated);
        }
    }

    fn telemetry(&self, selected_account: &ProviderAccountId) -> AffinityTelemetry {
        AffinityTelemetry {
            affinity_hit: self.bound_account.as_ref() == Some(selected_account)
                && self.escape_reason.is_none(),
            escape_reason: self.escape_reason,
            account_switch: self
                .bound_account
                .as_ref()
                .is_some_and(|bound| bound != selected_account),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AffinityTelemetry {
    affinity_hit: bool,
    escape_reason: Option<AffinityEscapeReason>,
    account_switch: bool,
}

impl CodexCredentialSelector {
    #[must_use]
    // 选择器显式持有各能力边界，避免把 Provider 私有服务重新包装成通用容器
    #[expect(clippy::too_many_arguments)]
    pub fn new(
        provider_kind: ProviderKind,
        repository: CodexCredentialRepository,
        leases: Arc<dyn ProviderLeasePort>,
        session_affinity: Arc<dyn ProviderSessionAffinityPort>,
        session_exclusions: Arc<dyn ProviderSessionExclusionPort>,
        quota: Arc<CodexCredentialQuotaService>,
        account_feedback: Arc<AccountFeedbackStats>,
        cookie_policy: CodexCookiePolicy,
    ) -> Self {
        Self {
            provider_kind,
            repository,
            leases,
            session_affinity,
            session_exclusions,
            quota,
            cookie_policy,
            risk_recovery: Mutex::new(HashMap::new()),
            waiting: ConcurrencyWaitQueue::default(),
            reserved_waiting: ConcurrencyWaitQueue::default(),
            account_feedback,
        }
    }

    pub async fn select(
        &self,
        request: &SelectCodexCredential<'_>,
    ) -> Result<CodexCredentialLease, CredentialSelectionError> {
        let input = CredentialSelectionInput {
            requires_websocket: false,
            requires_oauth: false,
            request_url: request.request_url,
            attempt: request.attempt,
            session_affinity_key: request.session_affinity_key,
            session_affinity_observation: None,
            guardian: false,
        };
        self.select_inner(
            &input,
            None,
            CodexSelectionModel::Requested(request.upstream_model),
        )
        .await
    }

    pub(crate) async fn select_with_cyber_policy(
        &self,
        request: &SelectCodexCredential<'_>,
        cyber_policy_session_key: Option<&ProviderSessionAffinityKey>,
        session_affinity_observation: Option<&CodexSessionAffinity>,
        requires_websocket: bool,
        guardian: bool,
    ) -> Result<CodexCredentialLease, CredentialSelectionError> {
        let input = CredentialSelectionInput {
            requires_websocket,
            requires_oauth: false,
            request_url: request.request_url,
            attempt: request.attempt,
            session_affinity_key: request.session_affinity_key,
            session_affinity_observation,
            guardian,
        };
        self.select_inner(
            &input,
            cyber_policy_session_key,
            CodexSelectionModel::Requested(request.upstream_model),
        )
        .await
    }

    /// 中间件完成请求改写后，用真实 OpenAI 会话事实复验已持有的租约
    ///
    /// 此处只复用既有亲和与 cyber-policy 端口，不再次选号；冲突必须在发送前失败，
    /// 避免同一 attempt 持有旧租约时重入账号选择
    pub(crate) async fn validate_translated_selection(
        &self,
        lease: &mut CodexCredentialLease,
        session_affinity: Option<&CodexSessionAffinity>,
        cyber_policy_session_key: Option<&ProviderSessionAffinityKey>,
        policy: AccountSelectionPolicy,
    ) -> Result<(), CredentialSelectionError> {
        let selected_account = lease.account.id().clone();
        if let Some(affinity) = session_affinity
            && lease.admitted_session.as_ref() != Some(affinity.key())
        {
            let current = self.lookup_session_affinity(affinity.key()).await?;
            if current
                .as_ref()
                .is_some_and(|binding| binding.account_id() != &selected_account)
                || !self
                    .admit_session(
                        affinity.key(),
                        current.as_ref(),
                        &selected_account,
                        policy.openai_session_affinity_ttl(),
                    )
                    .await?
            {
                return Err(CredentialSelectionError::SessionBound(Box::new(
                    CredentialSelectionError::NoEligibleCredential,
                )));
            }
            lease.admitted_session = Some(affinity.key().clone());
        }

        let cyber_policy_scope = self
            .prepare_cyber_policy_scope(cyber_policy_session_key)
            .await;
        if cyber_policy_scope
            .as_ref()
            .and_then(|scope| scope.state.as_ref())
            .is_some_and(|state| state.excluded_accounts().contains(&selected_account))
        {
            return Err(CredentialSelectionError::NoEligibleCredential);
        }
        lease.cyber_policy_scope = cyber_policy_scope;
        Ok(())
    }

    /// 为不属于 Responses 文本模型目录的 Provider 原生端点选择账号
    ///
    /// 账号范围、健康度、配额、并发租约、cookie 与认证准备仍走同一套选择链路；
    /// Images 与 live 语音按实际请求模型执行账号权限，不依赖文本模型目录
    /// `requires_oauth` 限定本端点支持的认证类型
    pub(crate) async fn select_for_provider_endpoint(
        &self,
        request: &SelectCodexProviderEndpointCredential<'_>,
    ) -> Result<CodexCredentialLease, CredentialSelectionError> {
        let input = CredentialSelectionInput {
            requires_websocket: false,
            requires_oauth: request.requires_oauth,
            request_url: request.request_url,
            attempt: request.attempt,
            session_affinity_key: request.session_affinity.map(CodexSessionAffinity::key),
            session_affinity_observation: request.session_affinity,
            guardian: false,
        };
        self.select_inner(&input, None, request.model).await
    }

    async fn select_inner(
        &self,
        request: &CredentialSelectionInput<'_>,
        cyber_policy_session_key: Option<&ProviderSessionAffinityKey>,
        model: CodexSelectionModel<'_>,
    ) -> Result<CodexCredentialLease, CredentialSelectionError> {
        let follow_only = !request.attempt.is_diagnostic_required_account()
            && request
                .attempt
                .account_selection_policy()
                .openai_account_affinity()
                == AccountAffinity::Strict
            && request
                .session_affinity_observation
                .is_some_and(CodexSessionAffinity::follow_only);
        let result = tokio::select! {
            biased;
            () = request.attempt.cancellation().cancelled() => Err(CredentialSelectionError::Cancelled),
            result = self.select_inner_loop(request, cyber_policy_session_key, model, follow_only) => result,
        };
        result.map_err(|error| {
            if follow_only
                && !matches!(
                    error,
                    CredentialSelectionError::SessionBound(_)
                        | CredentialSelectionError::ContinuationOwnerChanged
                        | CredentialSelectionError::PolicyRejected
                        | CredentialSelectionError::PolicyUnavailable
                )
            {
                CredentialSelectionError::SessionBound(Box::new(error))
            } else {
                error
            }
        })
    }

    async fn select_inner_loop(
        &self,
        request: &CredentialSelectionInput<'_>,
        cyber_policy_session_key: Option<&ProviderSessionAffinityKey>,
        model: CodexSelectionModel<'_>,
        follow_only: bool,
    ) -> Result<CodexCredentialLease, CredentialSelectionError> {
        let diagnostic = request.attempt.is_diagnostic_required_account();
        let binding_key = request.session_affinity_key.filter(|_| !diagnostic);
        let affinity_mode = request
            .attempt
            .account_selection_policy()
            .openai_account_affinity();
        let mut queue_policy = request.attempt.account_selection_policy().queue_policy();
        if follow_only {
            // 关闭普通账号排队不能放开后代线程换号，沿用队列时间设置并保留有界人数
            queue_policy.max_waiting = if queue_policy.max_waiting == 0 {
                1_000
            } else {
                queue_policy.max_waiting
            };
            if queue_policy.timeout.is_zero() {
                queue_policy.timeout = Duration::from_secs(30);
            }
        }
        // 审批使用独立容量与等待队列，任一池饱和都不能阻塞另一池取得空闲名额
        let reserve = request
            .attempt
            .account_selection_policy()
            .openai_guardian_reserved_concurrency();
        let prioritized = request.guardian && reserve > 0;
        let reserved_concurrency = if prioritized { reserve } else { 0 };
        let concurrency_pool = if prioritized {
            ProviderConcurrencyPool::Reserved
        } else {
            ProviderConcurrencyPool::Shared
        };
        let mut waiting = CapacityWait::new(
            if prioritized {
                &self.reserved_waiting
            } else {
                &self.waiting
            },
            queue_policy,
            request.attempt.deadline().at(),
            request.attempt.concurrency_wait_budget(),
        )
        .with_periodic_recheck(binding_key.is_some())
        .with_priority(if prioritized {
            WaitPriority::High
        } else {
            WaitPriority::Normal
        });
        let continuation_account = match request.attempt.continuation_attempt() {
            ContinuationAttempt::Native => request
                .attempt
                .continuation()
                .and_then(gateway_core::engine::continuation::ContinuationBinding::pinned)
                .map(|continuation| continuation.account().clone()),
            ContinuationAttempt::ReplayOwner => request
                .attempt
                .account_state_owner()
                .filter(|owner| owner.provider() == &self.provider_kind)
                .map(|owner| owner.account().clone()),
            ContinuationAttempt::None | ContinuationAttempt::ReplayAny => None,
        };
        let required_account = request.attempt.required_account().cloned();
        if required_account
            .as_ref()
            .zip(continuation_account.as_ref())
            .is_some_and(|(required, continuation)| required != continuation)
        {
            return Err(CredentialSelectionError::NoEligibleCredential);
        }
        let pinned_account = required_account.or_else(|| continuation_account.clone());
        let mut snapshot_retries = 0;
        'capacity: loop {
            let binding = if let Some(key) = binding_key {
                self.lookup_session_affinity(key).await?
            } else {
                None
            };
            // 非严格模式的续写可以归属分流账号；严格绑定迁移后才拒绝旧 owner
            if affinity_mode == AccountAffinity::Strict
                && binding
                    .as_ref()
                    .zip(continuation_account.as_ref())
                    .is_some_and(|(binding, owner)| binding.account_id() != owner)
            {
                // 旧账号的增量状态不能把已迁移的会话拉回去，交回客户端完整历史重放
                return Err(CredentialSelectionError::ContinuationOwnerChanged);
            }
            let mut accounts = self.repository.list_for_provider().await?;
            // store 侧常规调度列表不包含停用账号；管理端诊断要对固定账号执行真实上游
            // 验证，先补回 required 账号，再统一检查认证类型和传输能力
            if diagnostic
                && let Some(required) = request.attempt.required_account()
                && !accounts.iter().any(|account| account.id() == required)
                && let Some(account) = self
                    .repository
                    .store()
                    .get_account(required)
                    .await
                    .map_err(|source| CredentialSelectionError::Store(ErrorSource::new(source)))?
            {
                accounts.push(account);
            }
            let mut model_access_rejected = 0_usize;
            let accounts = accounts
                .into_iter()
                .filter(|account| {
                    account.provider() == &self.provider_kind
                        && (diagnostic
                            || request
                                .attempt
                                .account_scope()
                                .is_some_and(|scope| scope.allows(account.id())))
                        && (!request.requires_oauth
                            || account.authentication_kind() == CODEX_AUTHENTICATION_KIND_OAUTH)
                        && model.allows_account(account)
                        && (diagnostic || {
                            let allowed = request
                                .attempt
                                .account_scope()
                                .is_some_and(|scope| model.allows(scope, account.id()));
                            if !allowed {
                                model_access_rejected += 1;
                            }
                            allowed
                        })
                })
                .collect::<Vec<_>>();
            let mut eligible = Vec::with_capacity(accounts.len());
            for account in accounts {
                if request.requires_websocket
                    && pinned_account.as_ref().is_none_or(|id| id == account.id())
                {
                    let runtime = match self.repository.load_runtime_credential(&account).await {
                        Ok(runtime) => runtime,
                        Err(CredentialRepositoryError::RevisionConflict(_)) => {
                            retry_account_snapshot(
                                request.attempt,
                                &account,
                                &mut snapshot_retries,
                            )?;
                            continue 'capacity;
                        }
                        // 非固定账号的损坏凭据不能阻断其余账号的传输资格检查
                        Err(CredentialRepositoryError::InvalidCredentialData(_))
                            if pinned_account.is_none() =>
                        {
                            continue;
                        }
                        Err(error) => return Err(error.into()),
                    };
                    if runtime.transport == super::ResponsesTransport::Http {
                        continue;
                    }
                }
                eligible.push(account);
            }
            let accounts = eligible;
            if model_access_rejected > 0 && request.attempt.trace().is_enabled() {
                request.attempt.trace().record(
                    "account.model_access",
                    serde_json::json!({"rejectedCount": model_access_rejected}),
                );
            }
            if !diagnostic {
                self.quota.prepare_scheduling(&accounts).await;
            }
            let mut rate_limits = HashMap::with_capacity(accounts.len());
            if !diagnostic {
                for account in &accounts {
                    let until = self.quota.cooldown(account.id()).await.unwrap_or(None);
                    rate_limits.insert(account.id().clone(), until);
                }
            }
            let account_ids = accounts
                .iter()
                .map(|account| account.id().clone())
                .collect::<Vec<_>>();
            let scheduling = self
                .leases
                .load_state(
                    request.attempt.client_api_key_ref(),
                    &self.provider_kind,
                    &account_ids,
                    concurrency_pool,
                )
                .await?;
            let round_robin_cursor = scheduling.round_robin_cursor();
            let candidates = accounts
                .into_iter()
                .map(|account| {
                    let health = self
                        .account_feedback
                        .scheduling_signals(&self.provider_kind, account.id());
                    let signals = scheduling
                        .signals()
                        .get(account.id())
                        .cloned()
                        .unwrap_or(AccountRuntimeSignals {
                            in_flight: 0,
                            last_started_at: None,
                            quota_reset_at: None,
                            quota_remaining_rank: None,
                            cooldown: None,
                            failure_rate_basis_points: None,
                            first_output_latency_ms: None,
                        })
                        .with_provider_quota(self.quota.scheduling_signals(&account))
                        .with_rate_limit(rate_limits.get(account.id()).copied().flatten())
                        .with_runtime_health(health.0, health.1);
                    AccountCandidate { account, signals }
                })
                .collect::<Vec<_>>();
            let mut affinity = binding
                .as_ref()
                .filter(|_| affinity_mode != AccountAffinity::Relaxed)
                .map_or_else(AffinitySelection::default, |binding| {
                    affinity_selection_for_bound_account(
                        binding.account_id().clone(),
                        &candidates,
                        SystemTime::now(),
                    )
                });
            let cyber_policy_scope = self
                .prepare_cyber_policy_scope(cyber_policy_session_key)
                .await;
            let mut excluded = request.attempt.excluded_accounts().clone();
            if let Some(state) = cyber_policy_scope
                .as_ref()
                .and_then(|scope| scope.state.as_ref())
            {
                excluded.extend(state.excluded_accounts().iter().cloned());
            }
            // 原生续写归属和显式固定账号优先于会话偏好，不能把增量状态发送给其他账号
            if let Some(required) = pinned_account.as_ref() {
                excluded.extend(
                    candidates
                        .iter()
                        .filter(|candidate| candidate.account.id() != required)
                        .map(|candidate| candidate.account.id().clone()),
                );
                if affinity
                    .bound_account()
                    .is_some_and(|bound| bound != required)
                {
                    affinity.escape(AffinityEscapeReason::PinnedAccount);
                }
            }
            let mut shortest_retry = None;
            let base_excluded = excluded.clone();
            let policy = request.attempt.account_selection_policy();

            loop {
                let preferred = pinned_account
                    .clone()
                    .or_else(|| affinity.preferred_account().cloned());
                let mut context = AccountSelectionContext {
                    policy,
                    now: SystemTime::now(),
                    excluded_accounts: excluded.clone(),
                    preferred_account: preferred.clone(),
                    preferred_account_overrides_weight: true,
                    round_robin_cursor,
                    eligibility: if diagnostic {
                        AccountEligibilityPolicy::BypassForDiagnostic
                    } else {
                        AccountEligibilityPolicy::Enforce
                    },
                    account_scope: request.attempt.account_scope().cloned(),
                    reserved_concurrency,
                };
                let wait_context = AccountSelectionContext {
                    excluded_accounts: base_excluded.clone(),
                    ..context.clone()
                };
                let mut wait_candidates =
                    AccountSelector.wait_candidates(&candidates, &wait_context);
                let preferred_wait = preferred.as_ref().filter(|account| {
                    !diagnostic
                        && (affinity_mode == AccountAffinity::Strict || pinned_account.is_some())
                        && queue_policy.max_waiting > 0
                        && wait_candidates.contains(account)
                });
                if let Some(preferred) = preferred_wait {
                    wait_candidates.retain(|account| account == preferred);
                }
                let capacity = AccountSelector.capacity_snapshot(&candidates, &context);
                for candidate in &candidates {
                    if !waiting.can_try(candidate.account.id()) {
                        context
                            .excluded_accounts
                            .insert(candidate.account.id().clone());
                    }
                }
                let selection = match request
                    .attempt
                    .select_account(&self.provider_kind, model.model(), &candidates, &context)
                    .await
                {
                    Ok(selection) => selection,
                    Err(AccountPolicyError::StaleCandidate) => continue 'capacity,
                    Err(AccountPolicyError::Rejected) => {
                        return Err(CredentialSelectionError::PolicyRejected);
                    }
                    Err(AccountPolicyError::Fault) => {
                        return Err(CredentialSelectionError::PolicyUnavailable);
                    }
                };
                // 只约束内置调度；插件的显式选号保留原裁决和完整候选
                // 首次绑定由任意会话请求原子认领；后代只在已有绑定时限制为跟随当前账号
                let selection = if ((follow_only && binding.is_some())
                    || (affinity_mode == AccountAffinity::Preferred
                        && affinity.preferred_account().is_some()))
                    && selection.is_none_or(|selection| !selection.is_policy_choice())
                {
                    // 优先模式先尝试会话主账号，繁忙或不可用时才采用调度结果；高权重回切不抢占主账号
                    let preferred = if follow_only {
                        binding.as_ref().map(ProviderSessionBinding::account_id)
                    } else {
                        preferred.as_ref()
                    };
                    let mut follow_context = context.clone();
                    follow_context.now = SystemTime::now();
                    follow_context.excluded_accounts.extend(
                        candidates
                            .iter()
                            .filter(|candidate| preferred != Some(candidate.account.id()))
                            .map(|candidate| candidate.account.id().clone()),
                    );
                    let preferred_selection = AccountSelector.select(&candidates, &follow_context);
                    if follow_only {
                        preferred_selection
                    } else {
                        preferred_selection.or(selection)
                    }
                } else {
                    selection
                };
                // 可等待集合使用原始排除集；这里的 Excluded 只可能来自本轮租约争用或队列让位
                let selection = selection.filter(|selection| {
                    preferred_wait.is_none()
                        || selection.is_policy_choice()
                        || !matches!(
                            selection.preferred(),
                            PreferredAccountSelection::Blocked(
                                AccountSchedulingBlocker::ConcurrencyLimit
                                    | AccountSchedulingBlocker::RequestInterval
                                    | AccountSchedulingBlocker::Excluded
                            )
                        )
                });
                request.attempt.trace().account_selection(
                    &candidates,
                    &context,
                    selection.as_ref(),
                );
                let Some(selection) = selection else {
                    if follow_only && let Some(binding) = binding.as_ref() {
                        // 冷却、排除、停用或当前账号不在本次范围时也只等待根线程恢复或迁移
                        waiting
                            .wait(std::slice::from_ref(binding.account_id()))
                            .await?;
                        continue 'capacity;
                    }
                    if !diagnostic && queue_policy.max_waiting > 0 && !wait_candidates.is_empty() {
                        AccountSelector
                            .wait_for_capacity(
                                &mut waiting,
                                &wait_candidates,
                                &candidates,
                                &wait_context,
                            )
                            .await
                            .map_err(|error| {
                                tracing::info!(
                                    request_id = request.attempt.request_id().as_str(),
                                    queue_layer = "account",
                                    queue_wait_ms = waiting.elapsed().as_millis() as u64,
                                    reason = %error,
                                    "OpenAI 账号排队请求被拒绝"
                                );
                                CredentialSelectionError::QueueRejected(error)
                            })?;
                        continue 'capacity;
                    }
                    // 只判断本次账号范围；空池、认证失效和租约失败不能伪装成额度耗尽
                    // 使用最初的排除集合，避免本轮临时跳过的繁忙账号丢失其可恢复语义
                    let mut statuses = candidates
                        .iter()
                        .filter(|candidate| !base_excluded.contains(candidate.account.id()))
                        .map(|candidate| {
                            candidate
                                .account
                                .status_projection(context.now, candidate.signals.cooldown)
                                .status
                        });
                    let quota_exhausted = !diagnostic
                        && statuses.next() == Some(AccountStatus::QuotaExhausted)
                        && statuses.all(|status| status == AccountStatus::QuotaExhausted);
                    return if !wait_candidates.is_empty() || shortest_retry.is_some() {
                        Err(CredentialSelectionError::CapacityUnavailable {
                            retry_after: shortest_retry,
                        })
                    } else if quota_exhausted {
                        Err(CredentialSelectionError::QuotaExhausted)
                    } else {
                        Err(CredentialSelectionError::NoEligibleCredential)
                    };
                };
                affinity.observe_preferred_selection(selection.preferred());
                let selected = selection.candidate();
                let account = candidates
                    .iter()
                    .find(|candidate| candidate.account.id() == selected.account.id())
                    .map(|candidate| candidate.account.clone())
                    .ok_or(CredentialSelectionError::InvalidCredential(None))?;
                // 额度观测等并发更新会使整个账号快照失效，必须重新选号并校验资格
                // 在占用租约和请求间隔前完成校验，避免重读被自己的异步释放挡住
                let runtime = match self.repository.load_runtime_credential(&account).await {
                    Ok(runtime) => runtime,
                    Err(CredentialRepositoryError::RevisionConflict(_)) => {
                        retry_account_snapshot(request.attempt, &account, &mut snapshot_retries)?;
                        continue 'capacity;
                    }
                    Err(error) => return Err(error.into()),
                };
                // 凭据重读和插件选号都可能挂起，取得租约前再次让位于新队首
                if !waiting.can_try(account.id()) {
                    excluded.insert(account.id().clone());
                    continue;
                }
                let allows_account_state_mutation = !diagnostic || account.enabled();
                match self
                    .leases
                    .try_acquire(ProviderLeaseRequest::Scheduling(
                        ProviderSchedulingLeaseRequest::new(
                            self.provider_kind.clone(),
                            account.id().clone(),
                            account.revision(),
                            context.concurrency_limit(&account),
                            policy.request_interval(),
                            request.attempt.deadline(),
                        )
                        .with_concurrency_pool(concurrency_pool)
                        .with_cancellation(request.attempt.cancellation().clone()),
                    ))
                    .await?
                {
                    ProviderLeaseAcquisition::Busy { retry_after } => {
                        if preferred_wait != Some(account.id()) {
                            affinity.observe_lease_busy(account.id());
                        }
                        shortest_retry = minimum_duration(shortest_retry, retry_after);
                        excluded.insert(account.id().clone());
                    }
                    ProviderLeaseAcquisition::Acquired(guard) => {
                        if let Some(key) = binding_key
                            && !self
                                .admit_session(
                                    key,
                                    binding.as_ref(),
                                    // 分流只改变本次租约，CAS 续期仍保留会话主账号
                                    binding
                                        .as_ref()
                                        .filter(|_| affinity_mode == AccountAffinity::Preferred)
                                        .map_or(account.id(), ProviderSessionBinding::account_id),
                                    request
                                        .attempt
                                        .account_selection_policy()
                                        .openai_session_affinity_ttl(),
                                )
                                .await?
                        {
                            drop(guard);
                            continue 'capacity;
                        }
                        let affinity_telemetry = affinity.telemetry(account.id());
                        let affinity_observation = request.session_affinity_observation;
                        tracing::info!(
                            request_id = %request.attempt.request_id(),
                            attempt_index = request.attempt.attempt_index().get(),
                            rotation_strategy = policy.strategy().as_str(),
                            account_id = %account.id(),
                            affinity_hit = affinity_telemetry.affinity_hit,
                            escape_reason = affinity_telemetry
                                .escape_reason
                                .map_or("", AffinityEscapeReason::as_str),
                            account_switch = affinity_telemetry.account_switch,
                            affinity_key_hash = affinity_observation
                                .map_or("", CodexSessionAffinity::key_hash),
                            affinity_anchor_source = affinity_observation
                                .map_or("", CodexSessionAffinity::anchor_source),
                            affinity_anchor = affinity_observation
                                .map_or("", CodexSessionAffinity::anchor),
                            session_id = affinity_observation
                                .and_then(CodexSessionAffinity::session_id)
                                .unwrap_or(""),
                            session_id_present = affinity_observation
                                .is_some_and(CodexSessionAffinity::session_id_present),
                            "OpenAI account selected"
                        );
                        let cookies = runtime
                            .cookies
                            .into_iter()
                            .filter(|cookie| {
                                cookie
                                    .expires_at
                                    .is_none_or(|expires| expires > chrono::Utc::now())
                                    && self.cookie_policy.may_replay(
                                        request.request_url,
                                        &cookie.domain,
                                        &cookie.path,
                                        cookie.host_only,
                                        cookie.secure,
                                    )
                            })
                            .collect();
                        if !waiting.elapsed().is_zero() {
                            request.attempt.trace().record(
                                "account.queue.acquired",
                                serde_json::json!({"waitMs": waiting.elapsed().as_millis() as u64}),
                            );
                        }
                        return Ok(CodexCredentialLease {
                            installation_id: runtime.installation_id,
                            transport: runtime.transport,
                            account,
                            authentication: runtime.authentication,
                            cookies,
                            cyber_policy_scope,
                            allows_account_state_mutation,
                            affinity_telemetry,
                            admitted_session: binding_key.cloned(),
                            capacity: capacity.map(AccountCapacitySnapshot::with_acquired_request),
                            _guard: guard,
                        });
                    }
                }
            }
        }
    }

    pub(crate) async fn remember_turn(
        &self,
        turn: &ProviderSessionAffinityKey,
        session: &CodexSessionAffinity,
        session_ttl: Duration,
    ) -> Result<(), CredentialSelectionError> {
        let mut alias = session.alias_record();
        if session.turn_alias().is_none()
            && let Some(existing) = self.session_for_turn(turn).await?
        {
            // 同一轮次可能保存在线程键下；复验会话与权限后续期原记录，不能因格式不同改写归属
            if existing.key() != session.key() || existing.follow_only() != session.follow_only() {
                return Err(CredentialSelectionError::SessionBound(Box::new(
                    CredentialSelectionError::NoEligibleCredential,
                )));
            }
            alias = existing.alias_record();
        }
        let applied = tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_affinity
                .bind_alias(&self.provider_kind, turn, &alias, session_ttl),
        )
        .await
        .map_err(binding_store_error)?
        .map_err(binding_store_error)?;
        if !applied {
            return Err(CredentialSelectionError::SessionBound(Box::new(
                CredentialSelectionError::NoEligibleCredential,
            )));
        }
        Ok(())
    }

    pub(crate) async fn session_for_turn(
        &self,
        turn: &ProviderSessionAffinityKey,
    ) -> Result<Option<CodexSessionAffinity>, CredentialSelectionError> {
        tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_affinity.load_alias(&self.provider_kind, turn),
        )
        .await
        .map_err(binding_store_error)?
        .map_err(binding_store_error)
        .map(|alias| alias.map(|alias| CodexSessionAffinity::from_turn_alias(turn.clone(), alias)))
    }

    async fn lookup_session_affinity(
        &self,
        key: &ProviderSessionAffinityKey,
    ) -> Result<Option<ProviderSessionBinding>, CredentialSelectionError> {
        tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_affinity.load(&self.provider_kind, key),
        )
        .await
        .map_err(binding_store_error)?
        .map_err(binding_store_error)
    }

    async fn admit_session(
        &self,
        key: &ProviderSessionAffinityKey,
        expected: Option<&ProviderSessionBinding>,
        account: &ProviderAccountId,
        session_ttl: Duration,
    ) -> Result<bool, CredentialSelectionError> {
        tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_affinity.compare_and_bind(
                &self.provider_kind,
                key,
                expected,
                account,
                session_ttl,
            ),
        )
        .await
        .map_err(binding_store_error)?
        .map(|binding| binding.is_some())
        .map_err(binding_store_error)
    }

    async fn prepare_cyber_policy_scope(
        &self,
        key: Option<&ProviderSessionAffinityKey>,
    ) -> Option<CodexCyberPolicyScope> {
        let key = key?.clone();
        let state = match tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_exclusions.load(&self.provider_kind, &key),
        )
        .await
        {
            Ok(Ok(state)) => state,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "OpenAI cyber policy state read failed open");
                None
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = SESSION_AFFINITY_TIMEOUT.as_millis(),
                    "OpenAI cyber policy state read timed out"
                );
                None
            }
        };
        Some(CodexCyberPolicyScope { key, state })
    }

    pub(crate) async fn record_cyber_policy_failure(
        &self,
        scope: Option<&CodexCyberPolicyScope>,
        account: &ProviderAccount,
    ) {
        let Some(scope) = scope else {
            return;
        };
        match tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_exclusions.record_failure(
                &self.provider_kind,
                &scope.key,
                account.id(),
                CYBER_POLICY_SESSION_TTL,
            ),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(
                    account_id = %account.id(),
                    error = %error,
                    "OpenAI cyber policy exclusion write failed open"
                );
            }
            Err(_) => {
                tracing::warn!(
                    account_id = %account.id(),
                    timeout_ms = SESSION_AFFINITY_TIMEOUT.as_millis(),
                    "OpenAI cyber policy exclusion write timed out"
                );
            }
        }
    }

    pub(crate) async fn observe_cyber_policy_success(&self, scope: Option<&CodexCyberPolicyScope>) {
        let Some(scope) = scope.filter(|scope| scope.state.is_some()) else {
            return;
        };
        let Some(state) = scope.state.as_ref() else {
            return;
        };
        match tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_exclusions
                .clear(&self.provider_kind, &scope.key, state.revision()),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "OpenAI cyber policy exclusion clear failed open");
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = SESSION_AFFINITY_TIMEOUT.as_millis(),
                    "OpenAI cyber policy exclusion clear timed out"
                );
            }
        }
    }

    pub async fn record_failure(
        &self,
        account: &ProviderAccount,
        failure: CodexAccountFailure,
        message: Option<String>,
    ) -> Result<(), CredentialSelectionError> {
        let now = SystemTime::now();
        let message = message.filter(|value| !value.trim().is_empty());
        match failure {
            CodexAccountFailure::CredentialExpired => {
                if account.authentication_kind() == CODEX_AUTHENTICATION_KIND_OAUTH
                    && account.has_refresh_token()
                    && account
                        .access_token_expires_at()
                        .is_some_and(|expires_at| expires_at <= now)
                    && refresh_recovery_deadline(account.access_token_expires_at())
                        .is_some_and(|deadline| deadline > now)
                {
                    tracing::info!(
                        account_id = %account.id(),
                        access_token_expires_at = ?account.access_token_expires_at()
                            .map(chrono::DateTime::<chrono::Utc>::from),
                        recovery_deadline = ?refresh_recovery_deadline(account.access_token_expires_at())
                            .map(chrono::DateTime::<chrono::Utc>::from),
                        "OpenAI access token expired; retaining account for bounded OAuth refresh recovery"
                    );
                    return self
                        .apply_credential_state(
                            account,
                            CredentialState::Ready,
                            AccountErrorReason::AccessTokenExpired,
                            now,
                            message,
                        )
                        .await;
                }
                self.apply_credential_state(
                    account,
                    CredentialState::Expired,
                    AccountErrorReason::AccessTokenExpired,
                    now,
                    message,
                )
                .await
            }
            CodexAccountFailure::IdentityVerificationRequired => {
                self.apply_credential_state(
                    account,
                    CredentialState::Invalid,
                    AccountErrorReason::AccountUnverified,
                    now,
                    message,
                )
                .await
            }
            CodexAccountFailure::Banned => {
                self.apply_credential_state(
                    account,
                    CredentialState::Banned,
                    AccountErrorReason::AccountBanned,
                    now,
                    message,
                )
                .await
            }
            CodexAccountFailure::QuotaExhausted => self
                .quota
                .record_confirmed_exhaustion(account, QuotaEvidence::PaymentRequired, None, now)
                .await
                .map_err(|source| CredentialSelectionError::Store(ErrorSource::new(source))),
            CodexAccountFailure::UsageLimitExhausted { reset_at } => self
                .quota
                .record_confirmed_exhaustion(
                    account,
                    QuotaEvidence::UsageLimitReached,
                    reset_at,
                    now,
                )
                .await
                .map_err(|source| CredentialSelectionError::Store(ErrorSource::new(source))),
            // 429：临时限流只写运行时冷却，不改变凭据或额度事实
            CodexAccountFailure::RateLimited { retry_after } => {
                self.quota
                    .apply_rate_limit_429(account, retry_after, now)
                    .await
                    .map_err(|source| CredentialSelectionError::Store(ErrorSource::new(source)))?;
                Ok(())
            }
            // Cloudflare 挑战：内存退避表（记录风险计数），不写账号事实
            CodexAccountFailure::CloudflareChallenge { retry_after } => {
                let delay = self.cloudflare_challenge_delay(account.id(), now, retry_after);
                let recovery = now
                    .checked_add(delay)
                    .map_or(CookieRecovery::Clear, CookieRecovery::ExpireAt);
                self.apply_cookie_recovery(account, recovery).await?;
                Ok(())
            }
            // Cloudflare 路径被封：连续超阈值才置 Invalid；否则内存退避
            CodexAccountFailure::CloudflarePathBlocked => {
                let blocked = self.record_cloudflare_path_block(account.id(), now);
                if blocked >= CLOUDFLARE_PATH_BLOCK_THRESHOLD {
                    self.apply_credential_state(
                        account,
                        CredentialState::Invalid,
                        AccountErrorReason::CredentialInvalid,
                        now,
                        message,
                    )
                    .await?;
                }
                self.apply_cookie_recovery(account, CookieRecovery::Clear)
                    .await?;
                Ok(())
            }
        }
    }

    async fn apply_credential_state(
        &self,
        account: &ProviderAccount,
        credential_state: CredentialState,
        error_reason: AccountErrorReason,
        observed_at: SystemTime,
        message: Option<String>,
    ) -> Result<(), CredentialSelectionError> {
        self.repository
            .apply_state_with_reason(
                account,
                credential_state,
                observed_at,
                Some(error_reason),
                message,
            )
            .await?;
        Ok(())
    }

    pub async fn record_success(&self, account: &ProviderAccount) {
        self.restore_recoverable_account_state(account).await;
        self.risk_recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(account.id().as_str());
        // 绑定只在发送前认领，迟到的响应不能迁移或复活已经过期的会话绑定
    }

    async fn restore_recoverable_account_state(&self, account: &ProviderAccount) {
        let Ok(current) = self.current_account(account.id()).await else {
            return;
        };
        if current.provider() != &self.provider_kind
            || current.revision() != account.revision()
            || !current.enabled()
        {
            return;
        }
        let observed_at = SystemTime::now();
        if current.credential_state() != CredentialState::Ready
            && let Err(error) = self
                .repository
                .apply_state(&current, CredentialState::Ready, observed_at)
                .await
        {
            tracing::warn!(
                account_id = %account.id(),
                error = %error,
                "OpenAI credential recovery after successful upstream response failed"
            );
        }
        if let Err(error) = self
            .quota
            .record_successful_inference(&current, observed_at)
            .await
        {
            tracing::warn!(
                account_id = %account.id(),
                error = %error,
                "OpenAI quota recovery after successful upstream response failed"
            );
        }
    }

    pub async fn current_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ProviderAccount, CredentialSelectionError> {
        self.repository
            .store()
            .get_account(account_id)
            .await
            .map_err(|source| CredentialSelectionError::Store(ErrorSource::new(source)))?
            .ok_or(CredentialSelectionError::InvalidCredential(None))
    }

    pub async fn capture_response_cookies(
        &self,
        account: &ProviderAccount,
        response_origin: &Url,
        headers: &[String],
    ) -> Result<CodexCookieCaptureOutcome, CredentialSelectionError> {
        if account.authentication_kind() != CODEX_AUTHENTICATION_KIND_OAUTH {
            return Ok(CodexCookieCaptureOutcome {
                credential_revision: None,
                rejected: headers.len(),
            });
        }
        let parsed = self.cookie_policy.parse_response_headers(
            account.id().as_str(),
            account.revision().get(),
            response_origin,
            headers,
            chrono::Utc::now(),
        );
        if parsed.inputs.is_empty() {
            return Ok(CodexCookieCaptureOutcome {
                credential_revision: None,
                rejected: parsed.rejected,
            });
        }
        let mut data = self.repository.load_complete_data(account).await?;
        let Some(cookies) = data.cookies_mut() else {
            return Ok(CodexCookieCaptureOutcome {
                credential_revision: None,
                rejected: headers.len(),
            });
        };
        for input in parsed.inputs {
            let scope = self.cookie_policy.validate_capture(
                &input.response_origin,
                input.domain_attribute.as_deref(),
                &input.name,
                &input.path,
            )?;
            cookies.retain(|cookie| {
                !(cookie.name == input.name
                    && cookie.domain == scope.domain
                    && cookie.path == input.path)
            });
            if !input.delete {
                cookies.push(CodexCookie {
                    name: input.name,
                    value: input.value.expose_secret().to_owned(),
                    domain: scope.domain,
                    path: input.path,
                    host_only: scope.host_only,
                    secure: input.secure,
                    expires_at: input.expires_at,
                });
            }
        }
        let revision = self.repository.compare_and_swap_data(account, data).await?;
        Ok(CodexCookieCaptureOutcome {
            credential_revision: Some(revision.get()),
            rejected: parsed.rejected,
        })
    }

    fn cloudflare_challenge_delay(
        &self,
        account_id: &ProviderAccountId,
        now: SystemTime,
        retry_after: Option<Duration>,
    ) -> Duration {
        let mut recovery = self
            .risk_recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = active_risk_recovery(&mut recovery, account_id.as_str(), now);
        state.challenge_count = state.challenge_count.saturating_add(1);
        state.observed_at = now;
        let index = usize::try_from(state.challenge_count.saturating_sub(1))
            .unwrap_or(usize::MAX)
            .min(CLOUDFLARE_CHALLENGE_BACKOFF.len() - 1);
        retry_after
            .unwrap_or_default()
            .max(CLOUDFLARE_CHALLENGE_BACKOFF[index])
    }

    fn record_cloudflare_path_block(&self, account_id: &ProviderAccountId, now: SystemTime) -> u32 {
        let mut recovery = self
            .risk_recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = active_risk_recovery(&mut recovery, account_id.as_str(), now);
        state.path_block_count = state.path_block_count.saturating_add(1);
        state.observed_at = now;
        state.path_block_count
    }

    async fn apply_cookie_recovery(
        &self,
        account: &ProviderAccount,
        recovery: CookieRecovery,
    ) -> Result<(), CredentialSelectionError> {
        let mut data = self.repository.load_complete_data(account).await?;
        if data.cookies().is_empty() {
            return Ok(());
        }
        let Some(cookies) = data.cookies_mut() else {
            return Ok(());
        };
        match recovery {
            CookieRecovery::ExpireAt(expires_at) => {
                let expires_at = chrono::DateTime::<chrono::Utc>::from(expires_at);
                for cookie in cookies {
                    cookie.expires_at = Some(
                        cookie
                            .expires_at
                            .map_or(expires_at, |current| current.min(expires_at)),
                    );
                }
            }
            CookieRecovery::Clear => cookies.clear(),
        }
        self.repository.compare_and_swap_data(account, data).await?;
        Ok(())
    }
}

fn binding_store_error(
    source: impl std::error::Error + Send + Sync + 'static,
) -> CredentialSelectionError {
    CredentialSelectionError::SessionBound(Box::new(CredentialSelectionError::Store(
        ErrorSource::new(source),
    )))
}

fn affinity_selection_for_bound_account(
    account_id: ProviderAccountId,
    candidates: &[AccountCandidate],
    now: SystemTime,
) -> AffinitySelection {
    let Some(candidate) = candidates
        .iter()
        .find(|candidate| candidate.account.id() == &account_id)
    else {
        return AffinitySelection::escaped(account_id, AffinityEscapeReason::HardUnavailable);
    };
    match candidate
        .account
        .status_projection(now, candidate.signals.cooldown)
        .status
    {
        AccountStatus::Normal => AffinitySelection::preferred(account_id),
        AccountStatus::QuotaExhausted
        | AccountStatus::RateLimited
        | AccountStatus::Disabled
        | AccountStatus::Error => {
            AffinitySelection::escaped(account_id, affinity_unavailable_reason(candidate, now))
        }
    }
}

fn affinity_unavailable_reason(
    candidate: &AccountCandidate,
    now: SystemTime,
) -> AffinityEscapeReason {
    match candidate
        .account
        .status_projection(now, candidate.signals.cooldown)
        .status
    {
        AccountStatus::QuotaExhausted => AffinityEscapeReason::QuotaExhausted,
        AccountStatus::RateLimited => AffinityEscapeReason::Cooldown,
        AccountStatus::Normal => AffinityEscapeReason::SelectionInvariant,
        AccountStatus::Disabled | AccountStatus::Error => AffinityEscapeReason::HardUnavailable,
    }
}

fn active_risk_recovery<'a>(
    recovery: &'a mut HashMap<String, RiskRecoveryState>,
    account_id: &str,
    now: SystemTime,
) -> &'a mut RiskRecoveryState {
    recovery.retain(|_, state| match now.duration_since(state.observed_at) {
        Ok(elapsed) => elapsed <= CLOUDFLARE_RECOVERY_STALE_AFTER,
        Err(_) => true,
    });
    recovery
        .entry(account_id.to_owned())
        .or_insert(RiskRecoveryState {
            challenge_count: 0,
            path_block_count: 0,
            observed_at: now,
        })
}

impl fmt::Debug for CodexCredentialSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialSelector")
            .field("repository", &"ProviderAccountStore")
            .field("leases", &"ProviderLeasePort")
            .field("quota", &"CodexCredentialQuotaService")
            .field("cookie_policy", &self.cookie_policy)
            .finish()
    }
}

pub struct CodexCredentialLease {
    transport: super::ResponsesTransport,
    account: ProviderAccount,
    authentication: CodexRuntimeAuthentication,
    cookies: Vec<RuntimeCodexCookie>,
    installation_id: String,
    cyber_policy_scope: Option<CodexCyberPolicyScope>,
    allows_account_state_mutation: bool,
    affinity_telemetry: AffinityTelemetry,
    // 已准入请求允许完成；只有中间件改变会话身份时才需要再次认领
    admitted_session: Option<ProviderSessionAffinityKey>,
    capacity: Option<AccountCapacitySnapshot>,
    _guard: Box<dyn ProviderLeaseGuard>,
}

impl CodexCredentialLease {
    pub(crate) const fn transport(&self) -> super::ResponsesTransport {
        self.transport
    }

    #[must_use]
    pub const fn account(&self) -> &ProviderAccount {
        &self.account
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        self.account.id()
    }

    #[must_use]
    pub const fn authentication(&self) -> &CodexRuntimeAuthentication {
        &self.authentication
    }

    #[must_use]
    pub fn cookies(&self) -> &[RuntimeCodexCookie] {
        &self.cookies
    }

    #[must_use]
    pub fn installation_id(&self) -> &str {
        &self.installation_id
    }

    #[must_use]
    pub(crate) const fn cyber_policy_scope(&self) -> Option<&CodexCyberPolicyScope> {
        self.cyber_policy_scope.as_ref()
    }

    /// 禁用账号的管理端诊断必须只返回真实上游结果，不能回写账号侧状态
    #[must_use]
    pub(crate) const fn allows_account_state_mutation(&self) -> bool {
        self.allows_account_state_mutation
    }

    #[must_use]
    pub const fn affinity_hit(&self) -> bool {
        self.affinity_telemetry.affinity_hit
    }

    #[must_use]
    pub const fn escape_reason(&self) -> Option<&'static str> {
        match self.affinity_telemetry.escape_reason {
            Some(reason) => Some(reason.as_str()),
            None => None,
        }
    }

    #[must_use]
    pub const fn account_switch(&self) -> bool {
        self.affinity_telemetry.account_switch
    }

    #[must_use]
    pub const fn capacity_snapshot(&self) -> Option<AccountCapacitySnapshot> {
        self.capacity
    }
}

impl fmt::Debug for CodexCredentialLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialLease")
            .field("account", &self.account)
            .field("authentication", &"<redacted>")
            .field("cookies", &self.cookies)
            .field("installation_id", &"<pseudonymous>")
            .finish()
    }
}

fn retry_account_snapshot(
    attempt: &AttemptContext,
    account: &ProviderAccount,
    retries: &mut u32,
) -> Result<(), CredentialSelectionError> {
    let retry = *retries < MAX_ACCOUNT_SNAPSHOT_RETRIES;
    if retry {
        *retries += 1;
    }
    attempt.trace().record(
        "account.snapshot_conflict",
        serde_json::json!({
            "accountId": account.id().as_str(),
            "credentialRevision": account.revision().get(),
            "retry": retry,
            "retryCount": *retries,
            "maxRetries": MAX_ACCOUNT_SNAPSHOT_RETRIES,
        }),
    );
    if retry {
        Ok(())
    } else {
        Err(CredentialSelectionError::AccountSnapshotChanged)
    }
}

#[derive(Debug, Error)]
pub enum CredentialSelectionError {
    #[error("request was cancelled")]
    Cancelled,
    #[error("session account changed during native continuation")]
    ContinuationOwnerChanged,
    #[error("session account binding cannot be bypassed: {0}")]
    SessionBound(#[source] Box<CredentialSelectionError>),
    #[error(transparent)]
    QueueRejected(#[from] QueueRejection),
    #[error("no eligible Codex account")]
    NoEligibleCredential,
    #[error("all eligible Codex accounts have exhausted their quota")]
    QuotaExhausted,
    #[error("Codex account capacity is unavailable")]
    CapacityUnavailable { retry_after: Option<Duration> },
    #[error("Codex account data is invalid")]
    InvalidCredential(#[source] Option<ErrorSource>),
    #[error("Codex account changed repeatedly during selection")]
    AccountSnapshotChanged,
    #[error("Codex account store is unavailable")]
    Store(#[source] ErrorSource),
    #[error("Codex account lease runtime is unavailable")]
    Coordinator(#[source] ProviderStoreError),
    #[error("Codex Cookie policy rejected the value")]
    CookiePolicy(#[source] super::cookie::CookiePolicyError),
    #[error("account scheduling policy rejected the request")]
    PolicyRejected,
    #[error("account scheduling policy is unavailable")]
    PolicyUnavailable,
}

impl From<CredentialRepositoryError> for CredentialSelectionError {
    fn from(error: CredentialRepositoryError) -> Self {
        match &error {
            CredentialRepositoryError::InvalidCredentialData(_) => {
                Self::InvalidCredential(Some(ErrorSource::new(error)))
            }
            CredentialRepositoryError::RevisionConflict(_)
            | CredentialRepositoryError::Store(_) => Self::Store(ErrorSource::new(error)),
        }
    }
}

impl From<ProviderStoreError> for CredentialSelectionError {
    fn from(error: ProviderStoreError) -> Self {
        Self::Coordinator(error)
    }
}

impl From<super::cookie::CookiePolicyError> for CredentialSelectionError {
    fn from(error: super::cookie::CookiePolicyError) -> Self {
        Self::CookiePolicy(error)
    }
}

fn minimum_duration(current: Option<Duration>, candidate: Option<Duration>) -> Option<Duration> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (Some(current), None) => Some(current),
        (None, candidate) => candidate,
    }
}
