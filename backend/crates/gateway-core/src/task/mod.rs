//! 后台任务、leader lease 与运行计划的中立契约。
//!
//! 本模块不执行任务，也不启动异步运行时。各业务 owner 只返回
//! [`WorkerContribution`]，由 Host 负责监督、续租、重启与关闭。

use std::fmt;
use std::num::NonZeroU64;
use std::time::Duration;

use futures::future::BoxFuture;

use crate::engine::CancellationToken;

/// 冻结架构中的全部后台任务类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WorkerKind {
    OAuthRefresh,
    QuotaCatalogHealth,
    RuntimeSnapshotReconciliation,
    RuntimeChangeSubscription,
    NativeClaimRecovery,
    StaleModelRequestRecovery,
    Retention,
    OpsFlush,
}

impl WorkerKind {
    pub const ALL: [Self; 8] = [
        Self::OAuthRefresh,
        Self::QuotaCatalogHealth,
        Self::RuntimeSnapshotReconciliation,
        Self::RuntimeChangeSubscription,
        Self::NativeClaimRecovery,
        Self::StaleModelRequestRecovery,
        Self::Retention,
        Self::OpsFlush,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OAuthRefresh => "oauth_refresh",
            Self::QuotaCatalogHealth => "quota_catalog_health",
            Self::RuntimeSnapshotReconciliation => "runtime_snapshot_reconciliation",
            Self::RuntimeChangeSubscription => "runtime_change_subscription",
            Self::NativeClaimRecovery => "native_claim_recovery",
            Self::StaleModelRequestRecovery => "stale_model_request_recovery",
            Self::Retention => "retention",
            Self::OpsFlush => "ops_flush",
        }
    }
}

impl fmt::Display for WorkerKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 一个后台任务的稳定身份；同类任务可由多个 owner 各自贡献。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkerId {
    kind: WorkerKind,
    owner: String,
}

impl WorkerId {
    /// 从任务类别和可用于持久化 key 的 owner 创建身份。
    ///
    /// owner 长度为 1..=64，必须以小写 ASCII 字母或数字开头，
    /// 其余字符仅允许小写 ASCII 字母、数字、`-`、`_` 和 `.`。
    pub fn try_new(
        kind: WorkerKind,
        owner: impl Into<String>,
    ) -> Result<Self, WorkerDefinitionError> {
        let owner = owner.into();
        if !valid_owner(&owner) {
            return Err(WorkerDefinitionError::InvalidOwner);
        }
        Ok(Self { kind, owner })
    }

    #[must_use]
    pub const fn kind(&self) -> WorkerKind {
        self.kind
    }

    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }
}

impl fmt::Display for WorkerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.kind, self.owner)
    }
}

/// 正整数 fencing token；零值无法被构造。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkerFencingToken(NonZeroU64);

impl WorkerFencingToken {
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl From<NonZeroU64> for WorkerFencingToken {
    fn from(value: NonZeroU64) -> Self {
        Self::new(value)
    }
}

/// 一个周期任务的完整监督时序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerSchedule {
    interval: Duration,
    initial_backoff: Duration,
    maximum_backoff: Duration,
    leader_lease_ttl: Duration,
    leader_lease_renewal_interval: Duration,
}

impl WorkerSchedule {
    /// 构造已校验的时序。
    ///
    /// 所有时长必须大于零；最大退避不得小于初始退避；
    /// lease 续租间隔必须严格小于 lease TTL。
    pub fn try_new(
        interval: Duration,
        initial_backoff: Duration,
        maximum_backoff: Duration,
        leader_lease_ttl: Duration,
        leader_lease_renewal_interval: Duration,
    ) -> Result<Self, WorkerDefinitionError> {
        if interval.is_zero()
            || initial_backoff.is_zero()
            || maximum_backoff.is_zero()
            || leader_lease_ttl.is_zero()
            || leader_lease_renewal_interval.is_zero()
            || maximum_backoff < initial_backoff
            || leader_lease_renewal_interval >= leader_lease_ttl
        {
            return Err(WorkerDefinitionError::InvalidSchedule);
        }
        Ok(Self {
            interval,
            initial_backoff,
            maximum_backoff,
            leader_lease_ttl,
            leader_lease_renewal_interval,
        })
    }

    #[must_use]
    pub const fn interval(self) -> Duration {
        self.interval
    }

    #[must_use]
    pub const fn initial_backoff(self) -> Duration {
        self.initial_backoff
    }

    #[must_use]
    pub const fn maximum_backoff(self) -> Duration {
        self.maximum_backoff
    }

    #[must_use]
    pub const fn leader_lease_ttl(self) -> Duration {
        self.leader_lease_ttl
    }

    #[must_use]
    pub const fn leader_lease_renewal_interval(self) -> Duration {
        self.leader_lease_renewal_interval
    }
}

/// 长驻任务退出或 panic 后的 Host 重启策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonRestartPolicy {
    initial_backoff: Duration,
    maximum_backoff: Duration,
}

impl DaemonRestartPolicy {
    pub fn try_new(
        initial_backoff: Duration,
        maximum_backoff: Duration,
    ) -> Result<Self, WorkerDefinitionError> {
        if initial_backoff.is_zero()
            || maximum_backoff.is_zero()
            || maximum_backoff < initial_backoff
        {
            return Err(WorkerDefinitionError::InvalidDaemonRestartPolicy);
        }
        Ok(Self {
            initial_backoff,
            maximum_backoff,
        })
    }

    #[must_use]
    pub const fn initial_backoff(self) -> Duration {
        self.initial_backoff
    }

    #[must_use]
    pub const fn maximum_backoff(self) -> Duration {
        self.maximum_backoff
    }
}

/// 一次 leader lease 申请。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLeaseRequest {
    worker: WorkerId,
    ttl: Duration,
}

impl WorkerLeaseRequest {
    pub fn try_new(worker: WorkerId, ttl: Duration) -> Result<Self, WorkerDefinitionError> {
        if ttl.is_zero() {
            return Err(WorkerDefinitionError::InvalidLeaseTtl);
        }
        Ok(Self { worker, ttl })
    }

    #[must_use]
    pub const fn worker(&self) -> &WorkerId {
        &self.worker
    }

    #[must_use]
    pub const fn ttl(&self) -> Duration {
        self.ttl
    }
}

/// 已获取 lease 的可续租句柄。
///
/// Host 在正常路径必须显式 `await` [`Self::release`]；异常退出只依赖
/// Redis TTL 兜底，实现不得在 `Drop` 中启动异步任务。
pub trait WorkerLeaderLeaseGuard: Send + Sync {
    fn fencing_token(&self) -> WorkerFencingToken;

    fn renew(&mut self) -> BoxFuture<'_, Result<(), WorkerLeaseError>>;

    fn release(self: Box<Self>) -> BoxFuture<'static, Result<(), WorkerLeaseError>>;
}

/// Leader lease 的稳定获取结果。
pub enum WorkerLeaseAcquisition {
    Acquired(Box<dyn WorkerLeaderLeaseGuard>),
    Busy { retry_after: Option<Duration> },
}

impl fmt::Debug for WorkerLeaseAcquisition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Acquired(_) => formatter.write_str("Acquired([LEASE_GUARD])"),
            Self::Busy { retry_after } => formatter
                .debug_struct("Busy")
                .field("retry_after", retry_after)
                .finish(),
        }
    }
}

/// 多实例 worker leader lease 的中立端口。
pub trait WorkerLeaderLeasePort: Send + Sync {
    fn try_acquire(
        &self,
        request: WorkerLeaseRequest,
    ) -> BoxFuture<'_, Result<WorkerLeaseAcquisition, WorkerLeaseError>>;
}

/// 周期任务收到的单周期上下文。
#[derive(Debug, Clone)]
pub struct WorkerCycleContext {
    worker: WorkerId,
    fencing_token: Option<WorkerFencingToken>,
    cancellation: CancellationToken,
}

impl WorkerCycleContext {
    #[must_use]
    pub fn new(
        worker: WorkerId,
        fencing_token: Option<WorkerFencingToken>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            worker,
            fencing_token,
            cancellation,
        }
    }

    #[must_use]
    pub const fn worker(&self) -> &WorkerId {
        &self.worker
    }

    #[must_use]
    pub const fn fencing_token(&self) -> Option<WorkerFencingToken> {
        self.fencing_token
    }

    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

/// 由 Host 周期调用的短生命任务。
pub trait ScheduledTask: Send + Sync {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>>;
}

/// 长驻任务。
///
/// 实现负责连接级重连退避；Host 负责任务退出或 panic 后的重启退避。
pub trait DaemonTask: Send + Sync {
    fn run(&self, cancellation: CancellationToken) -> BoxFuture<'_, Result<(), WorkerTaskError>>;
}

/// Host 可执行的两种任务形态，无法组合出“守护任务 + 周期调度”等非法状态。
pub enum WorkerRunnable {
    Scheduled {
        schedule: WorkerSchedule,
        lease: Option<WorkerLeaseRequest>,
        task: Box<dyn ScheduledTask>,
    },
    Daemon {
        restart: DaemonRestartPolicy,
        task: Box<dyn DaemonTask>,
    },
}

impl fmt::Debug for WorkerRunnable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scheduled {
                schedule, lease, ..
            } => formatter
                .debug_struct("Scheduled")
                .field("schedule", schedule)
                .field("lease", lease)
                .field("task", &"[SCHEDULED_TASK]")
                .finish(),
            Self::Daemon { restart, .. } => formatter
                .debug_struct("Daemon")
                .field("restart", restart)
                .field("task", &"[DAEMON_TASK]")
                .finish(),
        }
    }
}

/// 一个 owner 交给 Host 的任务注册。
#[derive(Debug)]
pub struct WorkerRegistration {
    pub id: WorkerId,
    pub runnable: WorkerRunnable,
}

impl WorkerRegistration {
    pub fn try_new(id: WorkerId, runnable: WorkerRunnable) -> Result<Self, WorkerDefinitionError> {
        let registration = Self { id, runnable };
        registration.validate()?;
        Ok(registration)
    }

    /// 校验公开字段构造出的注册是否保持身份与 lease 时序一致。
    pub fn validate(&self) -> Result<(), WorkerDefinitionError> {
        let WorkerRunnable::Scheduled {
            schedule,
            lease: Some(lease),
            ..
        } = &self.runnable
        else {
            return Ok(());
        };
        if lease.worker() != &self.id {
            return Err(WorkerDefinitionError::LeaseWorkerMismatch);
        }
        if lease.ttl() != schedule.leader_lease_ttl() {
            return Err(WorkerDefinitionError::LeaseTtlMismatch);
        }
        Ok(())
    }
}

/// Final DB 明确没有相应持久化状态时允许的两种禁用原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WorkerDisabledReason {
    NoPersistentNativeClaimState,
    NoBufferedOpsEvents,
}

impl WorkerDisabledReason {
    #[must_use]
    pub const fn kind(self) -> WorkerKind {
        match self {
            Self::NoPersistentNativeClaimState => WorkerKind::NativeClaimRecovery,
            Self::NoBufferedOpsEvents => WorkerKind::OpsFlush,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoPersistentNativeClaimState => {
                "native continuation has no persistent claim state"
            }
            Self::NoBufferedOpsEvents => "ops events have no flush buffer",
        }
    }
}

impl fmt::Display for WorkerDisabledReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 各包对 Host 计划的一项贡献。
#[derive(Debug)]
pub enum WorkerContribution {
    Registration(WorkerRegistration),
    Disabled {
        kind: WorkerKind,
        reason: WorkerDisabledReason,
    },
}

impl WorkerContribution {
    #[must_use]
    pub const fn kind(&self) -> WorkerKind {
        match self {
            Self::Registration(registration) => registration.id.kind(),
            Self::Disabled { kind, .. } => *kind,
        }
    }

    pub fn validate(&self) -> Result<(), WorkerDefinitionError> {
        match self {
            Self::Registration(registration) => registration.validate(),
            Self::Disabled { kind, reason } if *kind == reason.kind() => Ok(()),
            Self::Disabled { .. } => Err(WorkerDefinitionError::DisabledReasonMismatch),
        }
    }
}

/// 任务定义在交给 Host 前的稳定校验错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WorkerDefinitionError {
    #[error("worker owner is invalid")]
    InvalidOwner,
    #[error("worker schedule is invalid")]
    InvalidSchedule,
    #[error("daemon restart policy is invalid")]
    InvalidDaemonRestartPolicy,
    #[error("worker lease ttl is invalid")]
    InvalidLeaseTtl,
    #[error("worker registration lease belongs to a different worker")]
    LeaseWorkerMismatch,
    #[error("worker registration lease ttl differs from its schedule")]
    LeaseTtlMismatch,
    #[error("disabled worker kind does not match its reason")]
    DisabledReasonMismatch,
}

/// 不暴露基础设施原文或 lease resource 的错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WorkerLeaseError {
    message: String,
}

impl WorkerLeaseError {
    #[must_use]
    pub fn safe(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn as_safe_str(&self) -> &str {
        &self.message
    }
}

/// 不暴露 Provider 原文或存储细节的后台任务错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WorkerTaskError {
    message: String,
}

impl WorkerTaskError {
    #[must_use]
    pub fn safe(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn as_safe_str(&self) -> &str {
        &self.message
    }
}

fn valid_owner(owner: &str) -> bool {
    !owner.is_empty()
        && owner.len() <= 64
        && owner.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.".contains(&byte)
        })
        && owner
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
}
