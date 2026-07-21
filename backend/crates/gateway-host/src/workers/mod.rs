//! Core 任务计划的唯一运行时：注册校验、lease、退避、健康与关闭。

use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::FutureExt as _;
use gateway_core::engine::CancellationToken;
use gateway_core::health::{
    WorkerHealthKey, WorkerHealthSnapshot, WorkerHealthSource, WorkerRuntimeState,
};
use gateway_core::task::{
    DaemonRestartPolicy, DaemonTask, ScheduledTask, WorkerContribution, WorkerCycleContext,
    WorkerDefinitionError, WorkerDisabledReason, WorkerId, WorkerKind, WorkerLeaderLeaseGuard,
    WorkerLeaderLeasePort, WorkerLeaseAcquisition, WorkerLeaseRequest, WorkerRegistration,
    WorkerRunnable, WorkerSchedule, WorkerTaskError,
};
use tokio::task::JoinHandle;

/// 进程内所有后台任务的唯一监督器。
pub struct WorkerSupervisor {
    cancellation: CancellationToken,
    health: Arc<WorkerHealthRegistry>,
    started: AtomicBool,
    handles: Mutex<Vec<(WorkerId, JoinHandle<()>)>>,
}

impl WorkerSupervisor {
    #[must_use]
    pub fn new(cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            health: Arc::new(WorkerHealthRegistry::default()),
            started: AtomicBool::new(false),
            handles: Mutex::new(Vec::new()),
        }
    }

    #[must_use]
    pub fn health_source(&self) -> Arc<dyn WorkerHealthSource> {
        self.health.clone()
    }

    /// 校验完整计划并且仅启动一次。
    pub fn start(
        &self,
        plan: Vec<WorkerContribution>,
        leader_lease: Arc<dyn WorkerLeaderLeasePort>,
    ) -> Result<(), WorkerStartError> {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(WorkerStartError::AlreadyStarted);
        }
        let result = self.start_inner(plan, leader_lease);
        if result.is_err() {
            self.started.store(false, Ordering::Release);
        }
        result
    }

    fn start_inner(
        &self,
        plan: Vec<WorkerContribution>,
        leader_lease: Arc<dyn WorkerLeaderLeasePort>,
    ) -> Result<(), WorkerStartError> {
        tokio::runtime::Handle::try_current().map_err(|_| WorkerStartError::RuntimeUnavailable)?;
        let WorkerPlan {
            registrations,
            disabled,
        } = WorkerPlan::try_from(plan)?;
        self.health.install(&registrations, &disabled);

        let handles = registrations
            .into_values()
            .map(|registration| {
                let id = registration.id.clone();
                let handle = match registration.runnable {
                    WorkerRunnable::Scheduled {
                        schedule,
                        lease,
                        task,
                    } => tokio::spawn(supervise_scheduled(
                        id.clone(),
                        task,
                        schedule,
                        lease,
                        Arc::clone(&leader_lease),
                        self.cancellation.clone(),
                        Arc::clone(&self.health),
                    )),
                    WorkerRunnable::Daemon { restart, task } => tokio::spawn(supervise_daemon(
                        id.clone(),
                        task,
                        restart,
                        self.cancellation.clone(),
                        Arc::clone(&self.health),
                    )),
                };
                (id, handle)
            })
            .collect();
        *lock_unpoisoned(&self.handles) = handles;
        Ok(())
    }

    /// 先协作取消；超时后 abort 不合作任务，lease 依赖 TTL 兜底。
    pub async fn shutdown(&self, timeout: Duration) {
        self.cancellation.cancel();
        let mut handles = std::mem::take(&mut *lock_unpoisoned(&self.handles));
        let joined = tokio::time::timeout(timeout, async {
            for (_, handle) in &mut handles {
                let _ = handle.await;
            }
        })
        .await
        .is_ok();
        if !joined {
            for (_, handle) in &handles {
                handle.abort();
            }
            for (_, handle) in handles {
                let _ = handle.await;
            }
        }
        self.health.stop_all();
    }
}

impl Drop for WorkerSupervisor {
    fn drop(&mut self) {
        self.cancellation.cancel();
        for (_, handle) in lock_unpoisoned(&self.handles).iter() {
            handle.abort();
        }
        self.health.stop_all();
    }
}

struct WorkerPlan {
    registrations: BTreeMap<WorkerId, WorkerRegistration>,
    disabled: BTreeMap<WorkerKind, WorkerDisabledReason>,
}

impl TryFrom<Vec<WorkerContribution>> for WorkerPlan {
    type Error = WorkerStartError;

    fn try_from(plan: Vec<WorkerContribution>) -> Result<Self, Self::Error> {
        let mut registrations = BTreeMap::new();
        let mut disabled = BTreeMap::new();
        for contribution in plan {
            contribution.validate()?;
            match contribution {
                WorkerContribution::Registration(registration) => {
                    let kind = registration.id.kind();
                    if disabled.contains_key(&kind) {
                        return Err(WorkerStartError::KindDisabled(kind));
                    }
                    let id = registration.id.clone();
                    if registrations.insert(id.clone(), registration).is_some() {
                        return Err(WorkerStartError::Duplicate(id));
                    }
                }
                WorkerContribution::Disabled { kind, reason } => {
                    if registrations.keys().any(|id| id.kind() == kind) {
                        return Err(WorkerStartError::KindHasTasks(kind));
                    }
                    if disabled.insert(kind, reason).is_some() {
                        return Err(WorkerStartError::DuplicateDisabled(kind));
                    }
                }
            }
        }
        let active = registrations
            .keys()
            .map(WorkerId::kind)
            .collect::<BTreeSet<_>>();
        let missing = WorkerKind::ALL
            .into_iter()
            .filter(|kind| !active.contains(kind) && !disabled.contains_key(kind))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(WorkerStartError::Missing(missing));
        }
        Ok(Self {
            registrations,
            disabled,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerStartError {
    #[error("worker supervisor was already started")]
    AlreadyStarted,
    #[error("Tokio runtime is unavailable")]
    RuntimeUnavailable,
    #[error(transparent)]
    InvalidDefinition(#[from] WorkerDefinitionError),
    #[error("worker `{0}` was registered more than once")]
    Duplicate(WorkerId),
    #[error("worker kind `{0}` is disabled")]
    KindDisabled(WorkerKind),
    #[error("worker kind `{0}` already has tasks")]
    KindHasTasks(WorkerKind),
    #[error("worker kind `{0}` was disabled more than once")]
    DuplicateDisabled(WorkerKind),
    #[error("required workers are missing: {0:?}")]
    Missing(Vec<WorkerKind>),
}

#[derive(Clone)]
struct HealthEntry {
    snapshot: WorkerHealthSnapshot,
    success_freshness: Duration,
}

#[derive(Default)]
struct WorkerHealthRegistry {
    states: Mutex<BTreeMap<WorkerHealthKey, HealthEntry>>,
}

impl WorkerHealthRegistry {
    fn install(
        &self,
        registrations: &BTreeMap<WorkerId, WorkerRegistration>,
        disabled: &BTreeMap<WorkerKind, WorkerDisabledReason>,
    ) {
        let mut states = registrations
            .values()
            .map(|registration| {
                let freshness = match &registration.runnable {
                    WorkerRunnable::Scheduled { schedule, .. } => (*schedule)
                        .interval()
                        .saturating_mul(2)
                        .saturating_add(schedule.maximum_backoff()),
                    WorkerRunnable::Daemon { restart, .. } => restart.maximum_backoff(),
                };
                let key = WorkerHealthKey::Task(registration.id.clone());
                (
                    key.clone(),
                    HealthEntry {
                        snapshot: empty_snapshot(key, WorkerRuntimeState::Starting),
                        success_freshness: freshness,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        states.extend(disabled.keys().map(|kind| {
            let key = WorkerHealthKey::Disabled(*kind);
            (
                key.clone(),
                HealthEntry {
                    snapshot: empty_snapshot(key, WorkerRuntimeState::Disabled),
                    success_freshness: Duration::ZERO,
                },
            )
        }));
        *lock_unpoisoned(&self.states) = states;
    }

    fn update(&self, id: &WorkerId, update: impl FnOnce(&mut WorkerHealthSnapshot)) {
        if let Some(entry) =
            lock_unpoisoned(&self.states).get_mut(&WorkerHealthKey::Task(id.clone()))
        {
            update(&mut entry.snapshot);
        }
    }

    fn acquiring(&self, id: &WorkerId) {
        self.update(id, |state| {
            state.state = WorkerRuntimeState::AcquiringLease;
        });
    }

    fn standby(&self, id: &WorkerId) {
        self.update(id, |state| {
            state.state = WorkerRuntimeState::Standby;
            state.consecutive_failures = 0;
            state.last_error = None;
        });
    }

    fn running(
        &self,
        id: &WorkerId,
        fencing_token: Option<gateway_core::task::WorkerFencingToken>,
    ) {
        self.update(id, |state| {
            state.state = WorkerRuntimeState::Running;
            state.last_fencing_token = fencing_token;
        });
    }

    fn succeeded(&self, id: &WorkerId) {
        self.update(id, |state| {
            state.state = WorkerRuntimeState::Idle;
            state.consecutive_failures = 0;
            state.completed_cycles = state.completed_cycles.saturating_add(1);
            state.last_success_at = Some(SystemTime::now());
            state.last_error = None;
        });
    }

    fn failed(&self, id: &WorkerId, error: String) -> u32 {
        let mut failures = 0;
        self.update(id, |state| {
            state.state = WorkerRuntimeState::BackingOff;
            state.consecutive_failures = state.consecutive_failures.saturating_add(1);
            state.last_failure_at = Some(SystemTime::now());
            state.last_error = Some(error);
            failures = state.consecutive_failures;
        });
        failures
    }

    fn stopped(&self, id: &WorkerId) {
        self.update(id, |state| state.state = WorkerRuntimeState::Stopped);
    }

    fn stop_all(&self) {
        for entry in lock_unpoisoned(&self.states).values_mut() {
            if entry.snapshot.state != WorkerRuntimeState::Disabled {
                entry.snapshot.state = WorkerRuntimeState::Stopped;
            }
        }
    }
}

impl WorkerHealthSource for WorkerHealthRegistry {
    fn snapshot(&self) -> Vec<WorkerHealthSnapshot> {
        let now = SystemTime::now();
        lock_unpoisoned(&self.states)
            .values()
            .map(|entry| {
                let mut snapshot = entry.snapshot.clone();
                let stale = matches!(
                    snapshot.state,
                    WorkerRuntimeState::Running | WorkerRuntimeState::Idle
                ) && snapshot.last_success_at.is_some_and(|last_success| {
                    now.duration_since(last_success)
                        .is_ok_and(|age| age > entry.success_freshness)
                });
                if stale {
                    snapshot.state = WorkerRuntimeState::BackingOff;
                    snapshot.last_error = Some("worker success is stale".to_owned());
                }
                snapshot
            })
            .collect()
    }
}

async fn supervise_scheduled(
    id: WorkerId,
    task: Box<dyn ScheduledTask>,
    schedule: WorkerSchedule,
    lease: Option<WorkerLeaseRequest>,
    leader_lease: Arc<dyn WorkerLeaderLeasePort>,
    cancellation: CancellationToken,
    health: Arc<WorkerHealthRegistry>,
) {
    let mut backoff = schedule.initial_backoff();
    loop {
        if cancellation.is_cancelled() {
            break;
        }
        let outcome = if let Some(request) = lease.clone() {
            health.acquiring(&id);
            acquire_and_run(
                &id,
                task.as_ref(),
                schedule,
                request,
                leader_lease.as_ref(),
                &cancellation,
                &health,
            )
            .await
        } else {
            run_local_cycle(&id, task.as_ref(), &cancellation, &health).await
        };

        let delay = match outcome {
            ScheduledOutcome::Succeeded => {
                health.succeeded(&id);
                backoff = schedule.initial_backoff();
                schedule.interval()
            }
            ScheduledOutcome::Busy(retry_after) => {
                health.standby(&id);
                retry_after
                    .filter(|delay| !delay.is_zero())
                    .unwrap_or(schedule.interval())
                    .min(schedule.maximum_backoff())
            }
            ScheduledOutcome::Failed(error) => {
                let failures = health.failed(&id, error.clone());
                let delay = take_backoff(&mut backoff, schedule.maximum_backoff());
                tracing::warn!(
                    worker = %id,
                    failures,
                    retry_after_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    error = %error,
                    "后台任务周期失败"
                );
                delay
            }
            ScheduledOutcome::ShuttingDown => break,
        };
        tokio::select! {
            () = cancellation.cancelled() => break,
            () = tokio::time::sleep(delay) => {}
        }
    }
    health.stopped(&id);
}

async fn acquire_and_run(
    id: &WorkerId,
    task: &dyn ScheduledTask,
    schedule: WorkerSchedule,
    request: WorkerLeaseRequest,
    leader_lease: &dyn WorkerLeaderLeasePort,
    cancellation: &CancellationToken,
    health: &WorkerHealthRegistry,
) -> ScheduledOutcome {
    let acquisition = AssertUnwindSafe(leader_lease.try_acquire(request)).catch_unwind();
    tokio::pin!(acquisition);
    let acquisition = tokio::select! {
        () = cancellation.cancelled() => return ScheduledOutcome::ShuttingDown,
        acquisition = &mut acquisition => acquisition,
    };
    match acquisition {
        Ok(Ok(WorkerLeaseAcquisition::Acquired(guard))) => {
            run_leased_cycle(id, task, guard, schedule, cancellation, health).await
        }
        Ok(Ok(WorkerLeaseAcquisition::Busy { retry_after })) => ScheduledOutcome::Busy(retry_after),
        Ok(Err(error)) => ScheduledOutcome::Failed(error.as_safe_str().to_owned()),
        Err(_) => ScheduledOutcome::Failed("leader lease port panicked".to_owned()),
    }
}

async fn run_local_cycle(
    id: &WorkerId,
    task: &dyn ScheduledTask,
    cancellation: &CancellationToken,
    health: &WorkerHealthRegistry,
) -> ScheduledOutcome {
    health.running(id, None);
    let cycle_cancel = CancellationToken::new();
    let cycle = AssertUnwindSafe(task.run_cycle(WorkerCycleContext::new(
        id.clone(),
        None,
        cycle_cancel.clone(),
    )))
    .catch_unwind();
    tokio::pin!(cycle);
    tokio::select! {
        outcome = &mut cycle => task_result(outcome),
        () = cancellation.cancelled() => {
            cycle_cancel.cancel();
            let _ = cycle.await;
            ScheduledOutcome::ShuttingDown
        }
    }
}

async fn run_leased_cycle(
    id: &WorkerId,
    task: &dyn ScheduledTask,
    mut guard: Box<dyn WorkerLeaderLeaseGuard>,
    schedule: WorkerSchedule,
    cancellation: &CancellationToken,
    health: &WorkerHealthRegistry,
) -> ScheduledOutcome {
    let fencing_token = match std::panic::catch_unwind(AssertUnwindSafe(|| guard.fencing_token())) {
        Ok(token) => token,
        Err(_) => return ScheduledOutcome::Failed("leader lease guard panicked".to_owned()),
    };
    health.running(id, Some(fencing_token));
    let cycle_cancel = CancellationToken::new();
    let cycle = AssertUnwindSafe(task.run_cycle(WorkerCycleContext::new(
        id.clone(),
        Some(fencing_token),
        cycle_cancel.clone(),
    )))
    .catch_unwind();
    tokio::pin!(cycle);
    let renewal = tokio::time::sleep(schedule.leader_lease_renewal_interval());
    tokio::pin!(renewal);

    let outcome = loop {
        tokio::select! {
            result = &mut cycle => break task_result(result),
            () = cancellation.cancelled() => {
                cycle_cancel.cancel();
                let _ = cycle.await;
                break ScheduledOutcome::ShuttingDown;
            }
            () = &mut renewal => {
                let renewed = renew_guard(
                    guard.as_mut(),
                    schedule.leader_lease_renewal_interval(),
                ).await;
                if let Err(error) = renewed {
                    cycle_cancel.cancel();
                    let _ = cycle.await;
                    break ScheduledOutcome::Failed(error);
                }
                renewal.as_mut().reset(
                    tokio::time::Instant::now() + schedule.leader_lease_renewal_interval(),
                );
            }
        }
    };
    match release_guard(guard, schedule.leader_lease_renewal_interval()).await {
        Ok(()) => outcome,
        Err(error) if matches!(outcome, ScheduledOutcome::ShuttingDown) => {
            tracing::warn!(worker = %id, error, "leader lease 释放失败");
            outcome
        }
        Err(error) => ScheduledOutcome::Failed(error),
    }
}

async fn renew_guard(
    guard: &mut dyn WorkerLeaderLeaseGuard,
    timeout: Duration,
) -> Result<(), String> {
    let future = AssertUnwindSafe(guard.renew()).catch_unwind();
    match tokio::time::timeout(timeout, future).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(error))) => Err(error.as_safe_str().to_owned()),
        Ok(Err(_)) => Err("leader lease renewal panicked".to_owned()),
        Err(_) => Err("leader lease renewal timed out".to_owned()),
    }
}

async fn release_guard(
    guard: Box<dyn WorkerLeaderLeaseGuard>,
    timeout: Duration,
) -> Result<(), String> {
    let future = std::panic::catch_unwind(AssertUnwindSafe(|| guard.release()))
        .map_err(|_| "leader lease release panicked".to_owned())?;
    match tokio::time::timeout(timeout, AssertUnwindSafe(future).catch_unwind()).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(error))) => Err(error.as_safe_str().to_owned()),
        Ok(Err(_)) => Err("leader lease release panicked".to_owned()),
        Err(_) => Err("leader lease release timed out".to_owned()),
    }
}

async fn supervise_daemon(
    id: WorkerId,
    task: Box<dyn DaemonTask>,
    restart: DaemonRestartPolicy,
    cancellation: CancellationToken,
    health: Arc<WorkerHealthRegistry>,
) {
    let mut backoff = restart.initial_backoff();
    loop {
        if cancellation.is_cancelled() {
            break;
        }
        health.running(&id, None);
        let daemon_cancel = CancellationToken::new();
        let run = AssertUnwindSafe(task.run(daemon_cancel.clone())).catch_unwind();
        tokio::pin!(run);
        let result = tokio::select! {
            result = &mut run => Some(result),
            () = cancellation.cancelled() => {
                daemon_cancel.cancel();
                let _ = run.await;
                None
            }
        };
        let Some(result) = result else { break };
        let error = match result {
            Ok(Ok(())) => "daemon exited unexpectedly".to_owned(),
            Ok(Err(error)) => error.as_safe_str().to_owned(),
            Err(_) => "daemon panicked".to_owned(),
        };
        let delay = take_backoff(&mut backoff, restart.maximum_backoff());
        let failures = health.failed(&id, error.clone());
        tracing::warn!(
            worker = %id,
            failures,
            retry_after_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            error = %error,
            "长驻任务将重启"
        );
        tokio::select! {
            () = cancellation.cancelled() => break,
            () = tokio::time::sleep(delay) => {}
        }
    }
    health.stopped(&id);
}

enum ScheduledOutcome {
    Succeeded,
    Busy(Option<Duration>),
    Failed(String),
    ShuttingDown,
}

fn task_result(
    result: Result<Result<(), WorkerTaskError>, Box<dyn std::any::Any + Send>>,
) -> ScheduledOutcome {
    match result {
        Ok(Ok(())) => ScheduledOutcome::Succeeded,
        Ok(Err(error)) => ScheduledOutcome::Failed(error.as_safe_str().to_owned()),
        Err(_) => ScheduledOutcome::Failed("worker panicked".to_owned()),
    }
}

fn empty_snapshot(key: WorkerHealthKey, state: WorkerRuntimeState) -> WorkerHealthSnapshot {
    WorkerHealthSnapshot {
        key,
        state,
        consecutive_failures: 0,
        completed_cycles: 0,
        last_fencing_token: None,
        last_success_at: None,
        last_failure_at: None,
        last_error: None,
    }
}

fn take_backoff(backoff: &mut Duration, maximum: Duration) -> Duration {
    let delay = *backoff;
    *backoff = backoff.checked_mul(2).unwrap_or(maximum).min(maximum);
    delay
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
