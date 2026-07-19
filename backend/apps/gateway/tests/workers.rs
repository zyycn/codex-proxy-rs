use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::pending;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use codex_proxy_rs::workers::{
    WorkerCycleContext, WorkerDisabledReason, WorkerFencingToken, WorkerHealthKey, WorkerId,
    WorkerKind, WorkerLeaderLeaseGuard, WorkerLeaderLeasePort, WorkerLeaseAcquisition,
    WorkerLeaseError, WorkerLeaseRequest, WorkerRegistry, WorkerRegistryError, WorkerRuntimeState,
    WorkerSchedule, WorkerTask, WorkerTaskError,
};
use tokio::sync::Notify;

const ACTIVE_KINDS: [WorkerKind; 4] = [
    WorkerKind::OAuthRefresh,
    WorkerKind::QuotaCatalogHealth,
    WorkerKind::StaleModelRequestRecovery,
    WorkerKind::Retention,
];

struct CountingTask {
    runs: AtomicUsize,
    fail_until: usize,
    fences: Mutex<Vec<u64>>,
}

impl CountingTask {
    fn new(fail_until: usize) -> Self {
        Self {
            runs: AtomicUsize::new(0),
            fail_until,
            fences: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl WorkerTask for CountingTask {
    async fn run_cycle(&self, context: WorkerCycleContext) -> Result<(), WorkerTaskError> {
        lock_unpoisoned(&self.fences).push(context.fencing_token().get());
        let run = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        if run <= self.fail_until {
            Err(WorkerTaskError::safe("expected test failure"))
        } else {
            Ok(())
        }
    }
}

struct PanicOnceTask {
    runs: AtomicUsize,
}

#[async_trait]
impl WorkerTask for PanicOnceTask {
    async fn run_cycle(&self, _context: WorkerCycleContext) -> Result<(), WorkerTaskError> {
        if self.runs.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("expected isolated worker panic");
        }
        Ok(())
    }
}

struct CancellationTask {
    entered: AtomicUsize,
    observed: AtomicUsize,
    notification: Notify,
}

#[async_trait]
impl WorkerTask for CancellationTask {
    async fn run_cycle(&self, context: WorkerCycleContext) -> Result<(), WorkerTaskError> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        self.notification.notify_one();
        context.cancelled().await;
        self.observed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct HungTask {
    entered: AtomicBool,
    notification: Notify,
}

struct SucceedsThenHangsTask {
    runs: AtomicUsize,
    second_cycle: Notify,
}

#[async_trait]
impl WorkerTask for SucceedsThenHangsTask {
    async fn run_cycle(&self, _context: WorkerCycleContext) -> Result<(), WorkerTaskError> {
        if self.runs.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(());
        }
        self.second_cycle.notify_one();
        pending::<()>().await;
        Ok(())
    }
}

#[async_trait]
impl WorkerTask for HungTask {
    async fn run_cycle(&self, _context: WorkerCycleContext) -> Result<(), WorkerTaskError> {
        self.entered.store(true, Ordering::SeqCst);
        self.notification.notify_one();
        pending::<()>().await;
        Ok(())
    }
}

struct LeaseAwareTask {
    entered: Notify,
    release: Notify,
    completed: AtomicBool,
    saw_live_guard: AtomicBool,
    expected_worker: WorkerId,
    active_guards: Arc<Mutex<BTreeSet<WorkerId>>>,
    fence: AtomicU64,
}

#[async_trait]
impl WorkerTask for LeaseAwareTask {
    async fn run_cycle(&self, context: WorkerCycleContext) -> Result<(), WorkerTaskError> {
        self.saw_live_guard.store(
            lock_unpoisoned(&self.active_guards).contains(context.worker()),
            Ordering::SeqCst,
        );
        if context.worker() != &self.expected_worker {
            return Err(WorkerTaskError::safe("unexpected worker identity"));
        }
        self.fence
            .store(context.fencing_token().get(), Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        self.completed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

struct ConcurrentCycleTask {
    starts: AtomicUsize,
    active: AtomicUsize,
    maximum_active: AtomicUsize,
}

struct ActiveCycle<'a> {
    active: &'a AtomicUsize,
}

impl Drop for ActiveCycle<'_> {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl WorkerTask for ConcurrentCycleTask {
    async fn run_cycle(&self, context: WorkerCycleContext) -> Result<(), WorkerTaskError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum_active.fetch_max(active, Ordering::SeqCst);
        let _active_cycle = ActiveCycle {
            active: &self.active,
        };
        context.cancelled().await;
        Ok(())
    }
}

#[derive(Debug, Clone)]
enum LeaseStep {
    Acquired,
    Busy(Option<Duration>),
    Error,
    Panic,
}

#[derive(Debug, Clone, Copy)]
enum RenewStep {
    Success,
    Error,
}

#[derive(Default)]
struct FakeLeasePort {
    scripts: Mutex<BTreeMap<WorkerId, VecDeque<LeaseStep>>>,
    renew_scripts: Arc<Mutex<BTreeMap<WorkerId, VecDeque<RenewStep>>>>,
    requests: Mutex<Vec<WorkerLeaseRequest>>,
    renewals: Arc<Mutex<Vec<WorkerId>>>,
    active_guards: Arc<Mutex<BTreeSet<WorkerId>>>,
    next_fence: AtomicU64,
}

impl FakeLeasePort {
    fn script(&self, worker: WorkerId, steps: impl IntoIterator<Item = LeaseStep>) {
        lock_unpoisoned(&self.scripts).insert(worker, steps.into_iter().collect());
    }

    fn renew_script(&self, worker: WorkerId, steps: impl IntoIterator<Item = RenewStep>) {
        lock_unpoisoned(&self.renew_scripts).insert(worker, steps.into_iter().collect());
    }

    fn requests(&self) -> Vec<WorkerLeaseRequest> {
        lock_unpoisoned(&self.requests).clone()
    }

    fn renewal_count(&self, worker: &WorkerId) -> usize {
        lock_unpoisoned(&self.renewals)
            .iter()
            .filter(|renewed| *renewed == worker)
            .count()
    }
}

struct FakeLeaseGuard {
    worker: WorkerId,
    fencing_token: WorkerFencingToken,
    active_guards: Arc<Mutex<BTreeSet<WorkerId>>>,
    renew_scripts: Arc<Mutex<BTreeMap<WorkerId, VecDeque<RenewStep>>>>,
    renewals: Arc<Mutex<Vec<WorkerId>>>,
}

#[async_trait]
impl WorkerLeaderLeaseGuard for FakeLeaseGuard {
    fn fencing_token(&self) -> WorkerFencingToken {
        self.fencing_token
    }

    async fn renew(&mut self) -> Result<(), WorkerLeaseError> {
        lock_unpoisoned(&self.renewals).push(self.worker.clone());
        match lock_unpoisoned(&self.renew_scripts)
            .get_mut(&self.worker)
            .and_then(VecDeque::pop_front)
            .unwrap_or(RenewStep::Success)
        {
            RenewStep::Success => Ok(()),
            RenewStep::Error => Err(WorkerLeaseError::safe("expected renewal failure")),
        }
    }
}

impl Drop for FakeLeaseGuard {
    fn drop(&mut self) {
        lock_unpoisoned(&self.active_guards).remove(&self.worker);
    }
}

#[async_trait]
impl WorkerLeaderLeasePort for FakeLeasePort {
    async fn try_acquire(
        &self,
        request: WorkerLeaseRequest,
    ) -> Result<WorkerLeaseAcquisition, WorkerLeaseError> {
        lock_unpoisoned(&self.requests).push(request.clone());
        let step = lock_unpoisoned(&self.scripts)
            .get_mut(request.worker())
            .and_then(VecDeque::pop_front)
            .unwrap_or(LeaseStep::Acquired);
        match step {
            LeaseStep::Acquired => {
                let fence = self.next_fence.fetch_add(1, Ordering::SeqCst) + 1;
                lock_unpoisoned(&self.active_guards).insert(request.worker().clone());
                Ok(WorkerLeaseAcquisition::Acquired(Box::new(FakeLeaseGuard {
                    worker: request.worker().clone(),
                    fencing_token: WorkerFencingToken::new(
                        NonZeroU64::new(fence).expect("positive test fencing token"),
                    ),
                    active_guards: Arc::clone(&self.active_guards),
                    renew_scripts: Arc::clone(&self.renew_scripts),
                    renewals: Arc::clone(&self.renewals),
                })))
            }
            LeaseStep::Busy(retry_after) => Ok(WorkerLeaseAcquisition::Busy { retry_after }),
            LeaseStep::Error => Err(WorkerLeaseError::safe("expected lease failure")),
            LeaseStep::Panic => panic!("expected isolated lease port panic"),
        }
    }
}

#[test]
fn schedule_rejects_every_invalid_duration_boundary() {
    let valid = Duration::from_millis(1);
    for values in [
        (Duration::ZERO, valid, valid, valid),
        (valid, Duration::ZERO, valid, valid),
        (valid, valid, Duration::ZERO, valid),
        (valid, valid, valid, Duration::ZERO),
        (valid, valid, valid, Duration::from_nanos(2)),
        (
            valid,
            Duration::from_millis(2),
            Duration::from_millis(1),
            valid,
        ),
    ] {
        assert_eq!(
            WorkerSchedule::new(values.0, values.1, values.2, values.3),
            Err(WorkerRegistryError::InvalidSchedule),
        );
    }
}

#[test]
fn worker_owner_rejects_empty_unsafe_or_oversized_names() {
    for owner in [
        String::new(),
        "Codex".to_owned(),
        "provider/codex".to_owned(),
        "x".repeat(65),
    ] {
        assert!(matches!(
            WorkerId::new(WorkerKind::OAuthRefresh, owner),
            Err(WorkerRegistryError::InvalidOwner(_))
        ));
    }
}

#[test]
fn registry_composes_multiple_owners_but_rejects_duplicate_identity() {
    let mut registry = WorkerRegistry::new();
    let task = Arc::new(CountingTask::new(0));
    registry
        .register(
            WorkerKind::OAuthRefresh,
            "openai",
            task.clone(),
            test_schedule(),
        )
        .expect("first owner registration");
    registry
        .register(
            WorkerKind::OAuthRefresh,
            "xai",
            task.clone(),
            test_schedule(),
        )
        .expect("second owner registration");
    assert!(matches!(
        registry.register(
            WorkerKind::OAuthRefresh,
            "openai",
            task,
            test_schedule(),
        ),
        Err(WorkerRegistryError::Duplicate(id)) if id.owner() == "openai"
    ));
}

#[test]
fn only_database_fact_reasons_can_disable_the_two_nonexistent_workers() {
    let task = Arc::new(CountingTask::new(0));
    let mut disabled = WorkerRegistry::new();
    disabled
        .disable(WorkerDisabledReason::NoPersistentNativeClaimState)
        .expect("native claim terminal fact");
    assert_eq!(
        disabled.register(
            WorkerKind::NativeClaimRecovery,
            "store",
            task.clone(),
            test_schedule(),
        ),
        Err(WorkerRegistryError::KindDisabled(
            WorkerKind::NativeClaimRecovery
        )),
    );

    let mut active = WorkerRegistry::new();
    active
        .register(WorkerKind::OpsFlush, "store", task, test_schedule())
        .expect("real owner registration");
    assert_eq!(
        active.disable(WorkerDisabledReason::NoBufferedOpsEvents),
        Err(WorkerRegistryError::KindHasTasks(WorkerKind::OpsFlush)),
    );
}

#[test]
fn registry_refuses_start_when_any_real_owner_kind_is_missing() {
    let task = Arc::new(CountingTask::new(0));
    let mut registry = WorkerRegistry::new();
    for kind in ACTIVE_KINDS
        .into_iter()
        .filter(|kind| *kind != WorkerKind::Retention)
    {
        registry
            .register(kind, kind.as_str(), task.clone(), test_schedule())
            .expect("unique active worker");
    }
    disable_nonexistent_workers(&mut registry);
    let lease_port: Arc<dyn WorkerLeaderLeasePort> = Arc::new(FakeLeasePort::default());
    assert!(matches!(
        registry.start(lease_port),
        Err(WorkerRegistryError::Missing(missing)) if missing == vec![WorkerKind::Retention]
    ));
}

#[tokio::test]
async fn disabled_database_facts_never_execute_or_acquire_a_lease() {
    let task = Arc::new(CountingTask::new(0));
    let lease_port = Arc::new(FakeLeasePort::default());
    let supervisor = complete_registry(task)
        .start(lease_port.clone())
        .expect("complete registry starts");

    wait_until(|| lease_port.requests().len() >= ACTIVE_KINDS.len()).await;
    assert!(lease_port.requests().iter().all(|request| {
        !matches!(
            request.worker().kind(),
            WorkerKind::NativeClaimRecovery | WorkerKind::OpsFlush
        )
    }));
    let snapshot = supervisor.health().snapshot();
    assert_eq!(
        snapshot
            .get(&WorkerHealthKey::Disabled(WorkerKind::NativeClaimRecovery))
            .map(|health| health.state),
        Some(WorkerRuntimeState::Disabled(
            WorkerDisabledReason::NoPersistentNativeClaimState
        )),
    );
    assert_eq!(
        snapshot
            .get(&WorkerHealthKey::Disabled(WorkerKind::OpsFlush))
            .map(|health| health.state),
        Some(WorkerRuntimeState::Disabled(
            WorkerDisabledReason::NoBufferedOpsEvents
        )),
    );
    assert!(supervisor.health().all_healthy());
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn one_kind_runs_all_registered_owner_tasks() {
    let common = Arc::new(CountingTask::new(0));
    let second_oauth_owner = Arc::new(CountingTask::new(0));
    let mut registry = complete_registry(common);
    registry
        .register(
            WorkerKind::OAuthRefresh,
            "xai",
            second_oauth_owner.clone(),
            test_schedule(),
        )
        .expect("second OAuth owner");
    let supervisor = registry
        .start(Arc::new(FakeLeasePort::default()))
        .expect("complete registry starts");

    wait_until(|| second_oauth_owner.runs.load(Ordering::SeqCst) >= 1).await;
    assert_eq!(
        supervisor
            .health()
            .snapshot()
            .keys()
            .filter(|key| matches!(key, WorkerHealthKey::Task(id) if id.kind() == WorkerKind::OAuthRefresh))
            .count(),
        2,
    );
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn leader_guard_lives_through_cycle_and_fence_reaches_owner() {
    let expected_worker =
        WorkerId::new(WorkerKind::OAuthRefresh, "openai").expect("valid expected worker identity");
    let lease_port = Arc::new(FakeLeasePort::default());
    let lease_task = Arc::new(LeaseAwareTask {
        entered: Notify::new(),
        release: Notify::new(),
        completed: AtomicBool::new(false),
        saw_live_guard: AtomicBool::new(false),
        expected_worker: expected_worker.clone(),
        active_guards: Arc::clone(&lease_port.active_guards),
        fence: AtomicU64::new(0),
    });
    let other = Arc::new(CountingTask::new(0));
    let mut registry = WorkerRegistry::new();
    registry
        .register(
            WorkerKind::OAuthRefresh,
            "openai",
            lease_task.clone(),
            long_interval_schedule(),
        )
        .expect("lease-aware worker");
    for kind in ACTIVE_KINDS
        .into_iter()
        .filter(|kind| *kind != WorkerKind::OAuthRefresh)
    {
        registry
            .register(kind, kind.as_str(), other.clone(), long_interval_schedule())
            .expect("other active worker");
    }
    disable_nonexistent_workers(&mut registry);
    let supervisor = registry
        .start(lease_port.clone())
        .expect("complete registry starts");

    lease_task.entered.notified().await;
    assert!(lease_task.saw_live_guard.load(Ordering::SeqCst));
    assert!(
        lock_unpoisoned(&lease_port.active_guards).contains(&expected_worker),
        "guard must remain alive while owner task is running"
    );
    lease_task.release.notify_one();
    wait_until(|| lease_task.completed.load(Ordering::SeqCst)).await;
    wait_until(|| !lock_unpoisoned(&lease_port.active_guards).contains(&expected_worker)).await;
    assert!(lease_task.fence.load(Ordering::SeqCst) > 0);
    let request = lease_port
        .requests()
        .into_iter()
        .find(|request| request.worker() == &expected_worker)
        .expect("leader lease request recorded");
    assert_eq!(request.ttl(), Duration::from_secs(30));
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn long_cycle_renews_leader_lease_before_ttl() {
    let expected_worker = target_worker();
    let lease_port = Arc::new(FakeLeasePort::default());
    let lease_task = Arc::new(LeaseAwareTask {
        entered: Notify::new(),
        release: Notify::new(),
        completed: AtomicBool::new(false),
        saw_live_guard: AtomicBool::new(false),
        expected_worker: expected_worker.clone(),
        active_guards: Arc::clone(&lease_port.active_guards),
        fence: AtomicU64::new(0),
    });
    let supervisor = registry_with_target(lease_task.clone(), renewal_schedule())
        .start(lease_port.clone())
        .expect("complete registry starts");

    lease_task.entered.notified().await;
    tokio::time::advance(Duration::from_millis(10)).await;
    yield_until(|| lease_port.renewal_count(&expected_worker) >= 1).await;
    assert!(lock_unpoisoned(&lease_port.active_guards).contains(&expected_worker));
    lease_task.release.notify_one();
    yield_until(|| lease_task.completed.load(Ordering::SeqCst)).await;
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn renewal_failure_cancels_old_cycle_before_another_leader_cycle_starts() {
    let task = Arc::new(ConcurrentCycleTask {
        starts: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        maximum_active: AtomicUsize::new(0),
    });
    let lease_port = Arc::new(FakeLeasePort::default());
    lease_port.renew_script(target_worker(), [RenewStep::Error]);
    let supervisor = registry_with_target(task.clone(), renewal_schedule())
        .start(lease_port)
        .expect("complete registry starts");

    yield_until(|| task.starts.load(Ordering::SeqCst) == 1).await;
    tokio::time::advance(Duration::from_millis(10)).await;
    yield_until(|| task.active.load(Ordering::SeqCst) == 0).await;
    assert_eq!(
        task_health(&supervisor.health().snapshot(), target_worker()).state,
        WorkerRuntimeState::BackingOff,
    );
    tokio::time::advance(Duration::from_millis(5)).await;
    yield_until(|| task.starts.load(Ordering::SeqCst) == 2).await;
    assert_eq!(task.maximum_active.load(Ordering::SeqCst), 1);
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn supervisor_uses_exponential_backoff_and_recovers_health() {
    let recovering = Arc::new(CountingTask::new(2));
    let supervisor = registry_with_target(recovering.clone(), backoff_schedule())
        .start(Arc::new(FakeLeasePort::default()))
        .expect("complete registry starts");

    yield_until(|| recovering.runs.load(Ordering::SeqCst) == 1).await;
    tokio::time::advance(Duration::from_millis(9)).await;
    yield_many().await;
    assert_eq!(recovering.runs.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_millis(1)).await;
    yield_until(|| recovering.runs.load(Ordering::SeqCst) == 2).await;
    tokio::time::advance(Duration::from_millis(19)).await;
    yield_many().await;
    assert_eq!(recovering.runs.load(Ordering::SeqCst), 2);
    tokio::time::advance(Duration::from_millis(1)).await;
    yield_until(|| recovering.runs.load(Ordering::SeqCst) == 3).await;

    let health = task_health(&supervisor.health().snapshot(), target_worker());
    assert_eq!(health.consecutive_failures, 0);
    assert_eq!(health.completed_cycles, 1);
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn task_and_lease_port_panics_are_isolated_and_retried() {
    let panic_task = Arc::new(PanicOnceTask {
        runs: AtomicUsize::new(0),
    });
    let lease_port = Arc::new(FakeLeasePort::default());
    let quota_worker = WorkerId::new(WorkerKind::QuotaCatalogHealth, "quota_catalog_health")
        .expect("valid quota worker");
    lease_port.script(
        quota_worker,
        [LeaseStep::Error, LeaseStep::Panic, LeaseStep::Acquired],
    );
    let supervisor = registry_with_target(panic_task.clone(), backoff_schedule())
        .start(lease_port)
        .expect("complete registry starts");

    yield_until(|| panic_task.runs.load(Ordering::SeqCst) == 1).await;
    tokio::time::advance(Duration::from_millis(10)).await;
    yield_until(|| panic_task.runs.load(Ordering::SeqCst) == 2).await;
    tokio::time::advance(Duration::from_millis(20)).await;
    yield_many().await;
    assert!(panic_task.runs.load(Ordering::SeqCst) >= 2);
    let health = task_health(&supervisor.health().snapshot(), target_worker());
    assert_eq!(health.consecutive_failures, 0);
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn lease_busy_is_healthy_standby_and_retries_without_running_owner() {
    let task = Arc::new(CountingTask::new(0));
    let lease_port = Arc::new(FakeLeasePort::default());
    lease_port.script(
        target_worker(),
        [
            LeaseStep::Busy(Some(Duration::from_millis(7))),
            LeaseStep::Acquired,
        ],
    );
    let supervisor = registry_with_target(task.clone(), backoff_schedule())
        .start(lease_port)
        .expect("complete registry starts");

    yield_until(|| {
        task_health(&supervisor.health().snapshot(), target_worker()).state
            == WorkerRuntimeState::Standby
    })
    .await;
    assert_eq!(task.runs.load(Ordering::SeqCst), 0);
    assert!(supervisor.health().all_healthy());
    tokio::time::advance(Duration::from_millis(7)).await;
    yield_until(|| task.runs.load(Ordering::SeqCst) == 1).await;
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn cooperative_shutdown_propagates_cancel_before_dropping_all_guards() {
    let task = Arc::new(CancellationTask {
        entered: AtomicUsize::new(0),
        observed: AtomicUsize::new(0),
        notification: Notify::new(),
    });
    let lease_port = Arc::new(FakeLeasePort::default());
    let supervisor = complete_registry(task.clone())
        .start(lease_port.clone())
        .expect("complete registry starts");

    wait_until(|| task.entered.load(Ordering::SeqCst) == ACTIVE_KINDS.len()).await;
    let health = supervisor.health().clone();
    supervisor.shutdown(Duration::from_secs(1)).await;
    assert_eq!(task.observed.load(Ordering::SeqCst), ACTIVE_KINDS.len());
    assert!(lock_unpoisoned(&lease_port.active_guards).is_empty());
    assert!(health.snapshot().iter().all(|(key, value)| {
        matches!(key, WorkerHealthKey::Disabled(_)) || value.state == WorkerRuntimeState::Stopped
    }));
}

#[tokio::test]
async fn shutdown_timeout_aborts_uncooperative_task_and_drops_guard() {
    let hung = Arc::new(HungTask {
        entered: AtomicBool::new(false),
        notification: Notify::new(),
    });
    let lease_port = Arc::new(FakeLeasePort::default());
    let supervisor = registry_with_target(hung.clone(), long_interval_schedule())
        .start(lease_port.clone())
        .expect("complete registry starts");

    hung.notification.notified().await;
    assert!(hung.entered.load(Ordering::SeqCst));
    assert!(
        !supervisor.health().all_healthy(),
        "a running worker without one successful cycle is not healthy"
    );
    let health = supervisor.health().clone();
    supervisor.shutdown(Duration::from_millis(10)).await;
    assert!(lock_unpoisoned(&lease_port.active_guards).is_empty());
    assert_eq!(
        task_health(&health.snapshot(), target_worker()).state,
        WorkerRuntimeState::Stopped,
    );
}

#[tokio::test]
async fn running_worker_becomes_unhealthy_when_last_success_is_stale() {
    let task = Arc::new(SucceedsThenHangsTask {
        runs: AtomicUsize::new(0),
        second_cycle: Notify::new(),
    });
    let schedule = WorkerSchedule::new(
        Duration::from_millis(10),
        Duration::from_millis(5),
        Duration::from_millis(10),
        Duration::from_secs(30),
    )
    .expect("freshness schedule");
    let supervisor = registry_with_target(task.clone(), schedule)
        .start(Arc::new(FakeLeasePort::default()))
        .expect("complete registry starts");

    task.second_cycle.notified().await;
    assert_eq!(task.runs.load(Ordering::SeqCst), 2);
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(
        !supervisor.health().all_healthy(),
        "running cannot mask an expired last-success timestamp"
    );
    supervisor.shutdown(Duration::from_millis(10)).await;
}

fn complete_registry(task: Arc<dyn WorkerTask>) -> WorkerRegistry {
    let mut registry = WorkerRegistry::new();
    for kind in ACTIVE_KINDS {
        registry
            .register(kind, kind.as_str(), task.clone(), test_schedule())
            .expect("unique real worker registration");
    }
    disable_nonexistent_workers(&mut registry);
    registry
}

fn registry_with_target(target: Arc<dyn WorkerTask>, schedule: WorkerSchedule) -> WorkerRegistry {
    let other = Arc::new(CountingTask::new(0));
    let mut registry = WorkerRegistry::new();
    registry
        .register(WorkerKind::OAuthRefresh, "openai", target, schedule)
        .expect("target worker registration");
    for kind in ACTIVE_KINDS
        .into_iter()
        .filter(|kind| *kind != WorkerKind::OAuthRefresh)
    {
        registry
            .register(kind, kind.as_str(), other.clone(), schedule)
            .expect("other worker registration");
    }
    disable_nonexistent_workers(&mut registry);
    registry
}

fn disable_nonexistent_workers(registry: &mut WorkerRegistry) {
    registry
        .disable(WorkerDisabledReason::NoPersistentNativeClaimState)
        .expect("native claim has no persistent state");
    registry
        .disable(WorkerDisabledReason::NoBufferedOpsEvents)
        .expect("ops events have no buffer");
}

fn target_worker() -> WorkerId {
    WorkerId::new(WorkerKind::OAuthRefresh, "openai").expect("valid target worker")
}

fn task_health(
    snapshot: &BTreeMap<WorkerHealthKey, codex_proxy_rs::workers::WorkerHealth>,
    id: WorkerId,
) -> codex_proxy_rs::workers::WorkerHealth {
    snapshot
        .get(&WorkerHealthKey::Task(id))
        .expect("worker health exists")
        .clone()
}

fn test_schedule() -> WorkerSchedule {
    WorkerSchedule::new(
        Duration::from_millis(20),
        Duration::from_millis(5),
        Duration::from_millis(20),
        Duration::from_secs(30),
    )
    .expect("valid test schedule")
}

fn long_interval_schedule() -> WorkerSchedule {
    WorkerSchedule::new(
        Duration::from_secs(60),
        Duration::from_millis(5),
        Duration::from_millis(20),
        Duration::from_secs(30),
    )
    .expect("valid long-interval schedule")
}

fn backoff_schedule() -> WorkerSchedule {
    WorkerSchedule::new(
        Duration::from_secs(60),
        Duration::from_millis(10),
        Duration::from_millis(20),
        Duration::from_secs(30),
    )
    .expect("valid backoff schedule")
}

fn renewal_schedule() -> WorkerSchedule {
    WorkerSchedule::new(
        Duration::from_secs(60),
        Duration::from_millis(5),
        Duration::from_millis(20),
        Duration::from_millis(30),
    )
    .expect("valid renewal schedule")
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker test timeout");
}

async fn yield_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert!(condition(), "condition did not become true after yielding");
}

async fn yield_many() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
