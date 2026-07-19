//! 后台任务注册、leader lease、监督、退避、健康状态与优雅关闭。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroU64;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use futures::FutureExt as _;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// 冻结架构允许出现的六类后台任务组。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WorkerKind {
    OAuthRefresh,
    QuotaCatalogHealth,
    NativeClaimRecovery,
    StaleModelRequestRecovery,
    Retention,
    OpsFlush,
}

impl WorkerKind {
    pub const ALL: [Self; 6] = [
        Self::OAuthRefresh,
        Self::QuotaCatalogHealth,
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

/// 同类任务中的真实 owner 标识；一个任务组可以组合多个 owner。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkerId {
    kind: WorkerKind,
    owner: String,
}

impl WorkerId {
    pub fn new(kind: WorkerKind, owner: impl Into<String>) -> Result<Self, WorkerRegistryError> {
        let owner = owner.into();
        if !valid_owner(&owner) {
            return Err(WorkerRegistryError::InvalidOwner(owner));
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

/// Final DB 明确不存在对应状态时允许使用的两种终态。
///
/// 该枚举刻意不提供通用字符串分支，避免把缺少 owner 的真实任务伪装成禁用。
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

/// 正整数 fencing token。
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

/// 一次 leader lease 申请；resource 由任务 kind 与 owner 共同确定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLeaseRequest {
    worker: WorkerId,
    ttl: Duration,
}

impl WorkerLeaseRequest {
    #[must_use]
    pub const fn worker(&self) -> &WorkerId {
        &self.worker
    }

    #[must_use]
    pub const fn ttl(&self) -> Duration {
        self.ttl
    }
}

/// Leader lease guard 暴露 fencing token 与续租操作，并在 Drop 时释放。
#[async_trait]
pub trait WorkerLeaderLeaseGuard: Send + Sync + 'static {
    fn fencing_token(&self) -> WorkerFencingToken;

    async fn renew(&mut self) -> Result<(), WorkerLeaseError>;
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

/// 多实例 worker leader lease 的 App 抽象端口。
#[async_trait]
pub trait WorkerLeaderLeasePort: Send + Sync + 'static {
    async fn try_acquire(
        &self,
        request: WorkerLeaseRequest,
    ) -> Result<WorkerLeaseAcquisition, WorkerLeaseError>;
}

/// 不暴露 Redis 原文或资源标识的 lease 错误。
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

/// 任务算法收到的单周期上下文。
#[derive(Clone)]
pub struct WorkerCycleContext {
    worker: WorkerId,
    fencing_token: WorkerFencingToken,
    cancellation: CancellationToken,
}

impl WorkerCycleContext {
    #[must_use]
    pub const fn worker(&self) -> &WorkerId {
        &self.worker
    }

    #[must_use]
    pub const fn fencing_token(&self) -> WorkerFencingToken {
        self.fencing_token
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.cancellation.cancelled().await;
    }
}

/// 任务算法的外层端口。实现必须位于对应 Store 或 Provider owner。
#[async_trait]
pub trait WorkerTask: Send + Sync + 'static {
    async fn run_cycle(&self, context: WorkerCycleContext) -> Result<(), WorkerTaskError>;
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

/// 单个 owner 任务的固定监督策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerSchedule {
    interval: Duration,
    initial_backoff: Duration,
    maximum_backoff: Duration,
    leader_lease_ttl: Duration,
    leader_lease_renewal_interval: Duration,
}

impl WorkerSchedule {
    /// 所有时长都必须大于零，且最大退避不能小于初始退避。
    pub fn new(
        interval: Duration,
        initial_backoff: Duration,
        maximum_backoff: Duration,
        leader_lease_ttl: Duration,
    ) -> Result<Self, WorkerRegistryError> {
        let Some(leader_lease_renewal_interval) = leader_lease_ttl.checked_div(3) else {
            return Err(WorkerRegistryError::InvalidSchedule);
        };
        if interval.is_zero()
            || initial_backoff.is_zero()
            || maximum_backoff < initial_backoff
            || leader_lease_ttl.is_zero()
            || leader_lease_renewal_interval.is_zero()
        {
            return Err(WorkerRegistryError::InvalidSchedule);
        }
        Ok(Self {
            interval,
            initial_backoff,
            maximum_backoff,
            leader_lease_ttl,
            leader_lease_renewal_interval,
        })
    }

    fn success_freshness(self) -> Duration {
        self.interval
            .saturating_mul(2)
            .saturating_add(self.maximum_backoff)
    }
}

struct WorkerRegistration {
    id: WorkerId,
    task: Arc<dyn WorkerTask>,
    schedule: WorkerSchedule,
}

/// 六类终态 worker 的唯一注册表。
#[derive(Default)]
pub struct WorkerRegistry {
    registrations: BTreeMap<WorkerId, WorkerRegistration>,
    disabled: BTreeMap<WorkerKind, WorkerDisabledReason>,
}

impl WorkerRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            registrations: BTreeMap::new(),
            disabled: BTreeMap::new(),
        }
    }

    /// 注册一个真实 Store/Provider-owned 周期任务。
    pub fn register(
        &mut self,
        kind: WorkerKind,
        owner: impl Into<String>,
        task: Arc<dyn WorkerTask>,
        schedule: WorkerSchedule,
    ) -> Result<(), WorkerRegistryError> {
        if self.disabled.contains_key(&kind) {
            return Err(WorkerRegistryError::KindDisabled(kind));
        }
        let id = WorkerId::new(kind, owner)?;
        if self.registrations.contains_key(&id) {
            return Err(WorkerRegistryError::Duplicate(id));
        }
        self.registrations
            .insert(id.clone(), WorkerRegistration { id, task, schedule });
        Ok(())
    }

    /// 声明 Final DB 不存在相应可恢复或可 flush 状态。
    pub fn disable(&mut self, reason: WorkerDisabledReason) -> Result<(), WorkerRegistryError> {
        let kind = reason.kind();
        if self
            .registrations
            .keys()
            .any(|registered| registered.kind() == kind)
        {
            return Err(WorkerRegistryError::KindHasTasks(kind));
        }
        if self.disabled.insert(kind, reason).is_some() {
            return Err(WorkerRegistryError::DuplicateDisabled(kind));
        }
        Ok(())
    }

    /// 校验六类 worker 都有真实 owner 或合法终态后启动监督器。
    pub fn start(
        self,
        leader_leases: Arc<dyn WorkerLeaderLeasePort>,
    ) -> Result<WorkerSupervisor, WorkerRegistryError> {
        let active_kinds = self
            .registrations
            .keys()
            .map(WorkerId::kind)
            .collect::<BTreeSet<_>>();
        let missing = WorkerKind::ALL
            .into_iter()
            .filter(|kind| !active_kinds.contains(kind) && !self.disabled.contains_key(kind))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(WorkerRegistryError::Missing(missing));
        }

        let cancellation = CancellationToken::new();
        let health = WorkerHealthRegistry::new(&self.registrations, &self.disabled);
        let active_ids = self.registrations.keys().cloned().collect::<Vec<_>>();
        let handles = self
            .registrations
            .into_values()
            .map(|registration| {
                let id = registration.id.clone();
                let handle = spawn_worker(
                    registration,
                    Arc::clone(&leader_leases),
                    cancellation.child_token(),
                    health.clone(),
                );
                (id, handle)
            })
            .collect();
        Ok(WorkerSupervisor {
            cancellation,
            health,
            active_ids,
            handles,
        })
    }
}

/// 注册或启动监督器失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkerRegistryError {
    #[error("worker schedule is invalid")]
    InvalidSchedule,
    #[error("worker owner is invalid: {0}")]
    InvalidOwner(String),
    #[error("worker `{0}` was registered more than once")]
    Duplicate(WorkerId),
    #[error("worker kind `{0}` is already disabled")]
    KindDisabled(WorkerKind),
    #[error("worker kind `{0}` already has real tasks")]
    KindHasTasks(WorkerKind),
    #[error("worker kind `{0}` was disabled more than once")]
    DuplicateDisabled(WorkerKind),
    #[error("required workers are missing: {0:?}")]
    Missing(Vec<WorkerKind>),
}

/// 一个 owner 任务的运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerRuntimeState {
    Starting,
    AcquiringLease,
    Standby,
    Running,
    Idle,
    BackingOff,
    Stopped,
    Disabled(WorkerDisabledReason),
}

/// 健康快照中的稳定 key。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkerHealthKey {
    Task(WorkerId),
    Disabled(WorkerKind),
}

/// 一个任务或终态 worker kind 的监督状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerHealth {
    pub state: WorkerRuntimeState,
    pub consecutive_failures: u32,
    pub completed_cycles: u64,
    pub last_fencing_token: Option<WorkerFencingToken>,
    pub last_success_at: Option<SystemTime>,
    pub last_failure_at: Option<SystemTime>,
    pub last_error: Option<String>,
    success_freshness: Duration,
}

impl WorkerHealth {
    fn active(schedule: WorkerSchedule) -> Self {
        Self {
            state: WorkerRuntimeState::Starting,
            consecutive_failures: 0,
            completed_cycles: 0,
            last_fencing_token: None,
            last_success_at: None,
            last_failure_at: None,
            last_error: None,
            success_freshness: schedule.success_freshness(),
        }
    }

    const fn disabled(reason: WorkerDisabledReason) -> Self {
        Self {
            state: WorkerRuntimeState::Disabled(reason),
            consecutive_failures: 0,
            completed_cycles: 0,
            last_fencing_token: None,
            last_success_at: None,
            last_failure_at: None,
            last_error: None,
            success_freshness: Duration::ZERO,
        }
    }

    fn is_healthy(&self, now: SystemTime) -> bool {
        match self.state {
            WorkerRuntimeState::Disabled(_) | WorkerRuntimeState::Standby => true,
            WorkerRuntimeState::AcquiringLease
            | WorkerRuntimeState::Running
            | WorkerRuntimeState::Idle => {
                self.consecutive_failures == 0
                    && self.last_success_at.is_some_and(|last_success| {
                        now.duration_since(last_success)
                            .is_ok_and(|age| age <= self.success_freshness)
                    })
            }
            WorkerRuntimeState::Starting
            | WorkerRuntimeState::BackingOff
            | WorkerRuntimeState::Stopped => false,
        }
    }
}

/// 可供健康检查读取的 worker 状态快照。
#[derive(Clone)]
pub struct WorkerHealthRegistry {
    states: Arc<Mutex<BTreeMap<WorkerHealthKey, WorkerHealth>>>,
}

impl WorkerHealthRegistry {
    fn new(
        registrations: &BTreeMap<WorkerId, WorkerRegistration>,
        disabled: &BTreeMap<WorkerKind, WorkerDisabledReason>,
    ) -> Self {
        let mut states = registrations
            .values()
            .map(|registration| {
                (
                    WorkerHealthKey::Task(registration.id.clone()),
                    WorkerHealth::active(registration.schedule),
                )
            })
            .collect::<BTreeMap<_, _>>();
        states.extend(disabled.iter().map(|(kind, reason)| {
            (
                WorkerHealthKey::Disabled(*kind),
                WorkerHealth::disabled(*reason),
            )
        }));
        Self {
            states: Arc::new(Mutex::new(states)),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> BTreeMap<WorkerHealthKey, WorkerHealth> {
        lock_unpoisoned(&self.states).clone()
    }

    #[must_use]
    pub fn all_healthy(&self) -> bool {
        let now = SystemTime::now();
        lock_unpoisoned(&self.states)
            .values()
            .all(|health| health.is_healthy(now))
    }

    fn update_task(&self, id: &WorkerId, update: impl FnOnce(&mut WorkerHealth)) {
        if let Some(state) =
            lock_unpoisoned(&self.states).get_mut(&WorkerHealthKey::Task(id.clone()))
        {
            update(state);
        }
    }

    fn acquiring_lease(&self, id: &WorkerId) {
        self.update_task(id, |state| {
            state.state = WorkerRuntimeState::AcquiringLease;
        });
    }

    fn standby(&self, id: &WorkerId) {
        self.update_task(id, |state| {
            state.state = WorkerRuntimeState::Standby;
            state.consecutive_failures = 0;
            state.last_error = None;
        });
    }

    fn running(&self, id: &WorkerId, fencing_token: WorkerFencingToken) {
        self.update_task(id, |state| {
            state.state = WorkerRuntimeState::Running;
            state.last_fencing_token = Some(fencing_token);
        });
    }

    fn succeeded(&self, id: &WorkerId) {
        self.update_task(id, |state| {
            state.state = WorkerRuntimeState::Idle;
            state.consecutive_failures = 0;
            state.completed_cycles = state.completed_cycles.saturating_add(1);
            state.last_success_at = Some(SystemTime::now());
            state.last_error = None;
        });
    }

    fn failed(&self, id: &WorkerId, error: String) -> u32 {
        let mut failures = 0;
        self.update_task(id, |state| {
            state.state = WorkerRuntimeState::BackingOff;
            state.consecutive_failures = state.consecutive_failures.saturating_add(1);
            state.last_failure_at = Some(SystemTime::now());
            state.last_error = Some(error);
            failures = state.consecutive_failures;
        });
        failures
    }

    fn stopped(&self, id: &WorkerId) {
        self.update_task(id, |state| {
            state.state = WorkerRuntimeState::Stopped;
        });
    }
}

/// 所有后台任务的统一 shutdown/join 句柄。
pub struct WorkerSupervisor {
    cancellation: CancellationToken,
    health: WorkerHealthRegistry,
    active_ids: Vec<WorkerId>,
    handles: Vec<(WorkerId, JoinHandle<()>)>,
}

impl WorkerSupervisor {
    #[must_use]
    pub const fn health(&self) -> &WorkerHealthRegistry {
        &self.health
    }

    /// 先协作取消；超时后强制 abort，并等待 guard 完成 Drop。
    pub async fn shutdown(mut self, timeout: Duration) {
        self.cancellation.cancel();
        let mut handles = std::mem::take(&mut self.handles);
        let completed = tokio::time::timeout(timeout, async {
            for (_, handle) in &mut handles {
                let _ = handle.await;
            }
        })
        .await
        .is_ok();
        if !completed {
            for (_, handle) in &handles {
                handle.abort();
            }
            for (_, handle) in handles {
                let _ = handle.await;
            }
        }
        for id in &self.active_ids {
            self.health.stopped(id);
        }
    }
}

impl Drop for WorkerSupervisor {
    fn drop(&mut self) {
        self.cancellation.cancel();
        for (_, handle) in &self.handles {
            handle.abort();
        }
        for id in &self.active_ids {
            self.health.stopped(id);
        }
    }
}

fn spawn_worker(
    registration: WorkerRegistration,
    leader_leases: Arc<dyn WorkerLeaderLeasePort>,
    cancellation: CancellationToken,
    health: WorkerHealthRegistry,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        supervise_worker(registration, leader_leases, cancellation, &health).await;
    })
}

async fn supervise_worker(
    registration: WorkerRegistration,
    leader_leases: Arc<dyn WorkerLeaderLeasePort>,
    cancellation: CancellationToken,
    health: &WorkerHealthRegistry,
) {
    let WorkerRegistration { id, task, schedule } = registration;
    let mut backoff = schedule.initial_backoff;

    loop {
        health.acquiring_lease(&id);
        let request = WorkerLeaseRequest {
            worker: id.clone(),
            ttl: schedule.leader_lease_ttl,
        };
        let acquisition = AssertUnwindSafe(leader_leases.try_acquire(request)).catch_unwind();
        tokio::pin!(acquisition);
        let acquisition = tokio::select! {
            () = cancellation.cancelled() => break,
            acquisition = &mut acquisition => acquisition,
        };

        let delay = match acquisition {
            Ok(Ok(WorkerLeaseAcquisition::Acquired(guard))) => {
                match run_leased_cycle(
                    &id,
                    &task,
                    guard,
                    schedule.leader_lease_renewal_interval,
                    &cancellation,
                    health,
                )
                .await
                {
                    LeasedCycleOutcome::Completed(TaskCycleOutcome::Succeeded) => {
                        health.succeeded(&id);
                        backoff = schedule.initial_backoff;
                        schedule.interval
                    }
                    LeasedCycleOutcome::Completed(TaskCycleOutcome::Failed(error)) => {
                        let failures = health.failed(&id, error.as_safe_str().to_owned());
                        tracing::warn!(
                            worker = %id,
                            failures,
                            "后台任务周期执行失败"
                        );
                        take_backoff(&mut backoff, schedule.maximum_backoff)
                    }
                    LeasedCycleOutcome::Completed(TaskCycleOutcome::Panicked) => {
                        let failures = health.failed(&id, "worker panicked".to_owned());
                        tracing::error!(worker = %id, failures, "后台任务发生 panic");
                        take_backoff(&mut backoff, schedule.maximum_backoff)
                    }
                    LeasedCycleOutcome::LeaseLost(error) => {
                        let failures = health.failed(&id, error);
                        tracing::warn!(worker = %id, failures, "后台任务 leader lease 已失效");
                        take_backoff(&mut backoff, schedule.maximum_backoff)
                    }
                    LeasedCycleOutcome::ShuttingDown => break,
                }
            }
            Ok(Ok(WorkerLeaseAcquisition::Busy { retry_after })) => {
                health.standby(&id);
                retry_after
                    .filter(|delay| !delay.is_zero())
                    .unwrap_or(schedule.interval)
                    .min(schedule.maximum_backoff)
            }
            Ok(Err(error)) => {
                let failures = health.failed(&id, error.as_safe_str().to_owned());
                tracing::warn!(worker = %id, failures, "后台任务 leader lease 获取失败");
                take_backoff(&mut backoff, schedule.maximum_backoff)
            }
            Err(_) => {
                let failures = health.failed(&id, "leader lease port panicked".to_owned());
                tracing::error!(worker = %id, failures, "后台任务 leader lease 端口发生 panic");
                take_backoff(&mut backoff, schedule.maximum_backoff)
            }
        };

        tokio::select! {
            () = cancellation.cancelled() => break,
            () = tokio::time::sleep(delay) => {}
        }
    }
    health.stopped(&id);
}

enum TaskCycleOutcome {
    Succeeded,
    Failed(WorkerTaskError),
    Panicked,
}

enum LeasedCycleOutcome {
    Completed(TaskCycleOutcome),
    LeaseLost(String),
    ShuttingDown,
}

async fn run_leased_cycle(
    id: &WorkerId,
    task: &Arc<dyn WorkerTask>,
    mut guard: Box<dyn WorkerLeaderLeaseGuard>,
    renewal_interval: Duration,
    cancellation: &CancellationToken,
    health: &WorkerHealthRegistry,
) -> LeasedCycleOutcome {
    let fencing_token = match std::panic::catch_unwind(AssertUnwindSafe(|| guard.fencing_token())) {
        Ok(fencing_token) => fencing_token,
        Err(_) => return LeasedCycleOutcome::LeaseLost("leader lease guard panicked".to_owned()),
    };
    health.running(id, fencing_token);
    let cycle_cancellation = cancellation.child_token();
    let context = WorkerCycleContext {
        worker: id.clone(),
        fencing_token,
        cancellation: cycle_cancellation.clone(),
    };
    let cycle = run_task_cycle(task, context);
    tokio::pin!(cycle);
    let renewal_sleep = tokio::time::sleep(renewal_interval);
    tokio::pin!(renewal_sleep);

    loop {
        tokio::select! {
            biased;
            outcome = &mut cycle => return LeasedCycleOutcome::Completed(outcome),
            () = cancellation.cancelled() => {
                cycle_cancellation.cancel();
                let _ = cycle.await;
                return LeasedCycleOutcome::ShuttingDown;
            }
            () = &mut renewal_sleep => {
                let renewal = AssertUnwindSafe(guard.renew()).catch_unwind();
                tokio::pin!(renewal);
                let renewal_timeout = tokio::time::sleep(renewal_interval);
                tokio::pin!(renewal_timeout);
                let renewal = tokio::select! {
                    biased;
                    outcome = &mut cycle => {
                        return LeasedCycleOutcome::Completed(outcome);
                    }
                    () = cancellation.cancelled() => {
                        cycle_cancellation.cancel();
                        let _ = cycle.await;
                        return LeasedCycleOutcome::ShuttingDown;
                    }
                    renewal = &mut renewal => Some(renewal),
                    () = &mut renewal_timeout => None,
                };
                match renewal {
                    Some(Ok(Ok(()))) => {
                        renewal_sleep
                            .as_mut()
                            .reset(tokio::time::Instant::now() + renewal_interval);
                    }
                    Some(Ok(Err(error))) => {
                        cycle_cancellation.cancel();
                        return LeasedCycleOutcome::LeaseLost(error.as_safe_str().to_owned());
                    }
                    Some(Err(_)) => {
                        cycle_cancellation.cancel();
                        return LeasedCycleOutcome::LeaseLost(
                            "leader lease renewal panicked".to_owned(),
                        );
                    }
                    None => {
                        cycle_cancellation.cancel();
                        return LeasedCycleOutcome::LeaseLost(
                            "leader lease renewal timed out".to_owned(),
                        );
                    }
                }
            }
        }
    }
}

async fn run_task_cycle(
    task: &Arc<dyn WorkerTask>,
    context: WorkerCycleContext,
) -> TaskCycleOutcome {
    match AssertUnwindSafe(task.run_cycle(context))
        .catch_unwind()
        .await
    {
        Ok(Ok(())) => TaskCycleOutcome::Succeeded,
        Ok(Err(error)) => TaskCycleOutcome::Failed(error),
        Err(_) => TaskCycleOutcome::Panicked,
    }
}

fn take_backoff(backoff: &mut Duration, maximum: Duration) -> Duration {
    let delay = *backoff;
    *backoff = backoff.checked_mul(2).unwrap_or(maximum).min(maximum);
    delay
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

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
