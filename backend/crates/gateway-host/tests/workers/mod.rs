use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::pending;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use gateway_core::engine::CancellationToken;
use gateway_core::health::{WorkerHealthKey, WorkerHealthSnapshot, WorkerRuntimeState};
use gateway_core::task::{
    ScheduledTask, WorkerContribution, WorkerCycleContext, WorkerDisabledReason,
    WorkerFencingToken, WorkerId, WorkerKind, WorkerLeaderLeaseGuard, WorkerLeaderLeasePort,
    WorkerLeaseAcquisition, WorkerLeaseError, WorkerLeaseRequest, WorkerRegistration,
    WorkerRunnable, WorkerSchedule, WorkerTaskError,
};
use gateway_host::workers::{WorkerStartError, WorkerSupervisor};
use tokio::sync::Notify;

const ACTIVE_KINDS: [WorkerKind; 6] = [
    WorkerKind::OAuthRefresh,
    WorkerKind::QuotaCatalogHealth,
    WorkerKind::RuntimeSnapshotReconciliation,
    WorkerKind::RuntimeChangeSubscription,
    WorkerKind::StaleModelRequestRecovery,
    WorkerKind::Retention,
];

#[derive(Clone)]
struct CountingTask {
    runs: Arc<AtomicUsize>,
    fail_until: usize,
}

impl CountingTask {
    fn new(fail_until: usize) -> Self {
        Self {
            runs: Arc::new(AtomicUsize::new(0)),
            fail_until,
        }
    }
}

impl ScheduledTask for CountingTask {
    fn run_cycle(
        &self,
        _context: WorkerCycleContext,
    ) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let run = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
            if run <= self.fail_until {
                Err(WorkerTaskError::safe("expected test failure"))
            } else {
                Ok(())
            }
        })
    }
}

#[derive(Clone)]
struct PanicOnceTask {
    runs: Arc<AtomicUsize>,
}

impl ScheduledTask for PanicOnceTask {
    fn run_cycle(
        &self,
        _context: WorkerCycleContext,
    ) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if self.runs.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("expected isolated worker panic");
            }
            Ok(())
        })
    }
}

#[derive(Clone)]
struct CancellationTask {
    entered: Arc<AtomicUsize>,
    observed: Arc<AtomicUsize>,
}

impl ScheduledTask for CancellationTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            self.entered.fetch_add(1, Ordering::SeqCst);
            context.cancellation().cancelled().await;
            self.observed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[derive(Clone)]
struct HungTask {
    entered: Arc<AtomicBool>,
    notification: Arc<Notify>,
}

impl ScheduledTask for HungTask {
    fn run_cycle(
        &self,
        _context: WorkerCycleContext,
    ) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            self.entered.store(true, Ordering::SeqCst);
            self.notification.notify_one();
            pending::<()>().await;
            Ok(())
        })
    }
}

#[derive(Clone)]
struct SucceedsThenHangsTask {
    runs: Arc<AtomicUsize>,
    second_cycle: Arc<Notify>,
}

impl ScheduledTask for SucceedsThenHangsTask {
    fn run_cycle(
        &self,
        _context: WorkerCycleContext,
    ) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if self.runs.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(());
            }
            self.second_cycle.notify_one();
            pending::<()>().await;
            Ok(())
        })
    }
}

#[derive(Clone)]
struct LeaseAwareTask {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    completed: Arc<AtomicBool>,
    saw_live_guard: Arc<AtomicBool>,
    expected_worker: WorkerId,
    active_guards: Arc<Mutex<BTreeSet<WorkerId>>>,
    fence: Arc<AtomicU64>,
}

impl ScheduledTask for LeaseAwareTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            self.saw_live_guard.store(
                lock_unpoisoned(&self.active_guards).contains(context.worker()),
                Ordering::SeqCst,
            );
            if context.worker() != &self.expected_worker {
                return Err(WorkerTaskError::safe("unexpected worker identity"));
            }
            self.fence.store(
                context
                    .fencing_token()
                    .expect("leased task fencing token")
                    .get(),
                Ordering::SeqCst,
            );
            self.entered.notify_one();
            self.release.notified().await;
            self.completed.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[derive(Clone)]
struct ConcurrentCycleTask {
    starts: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    maximum_active: Arc<AtomicUsize>,
}

impl ScheduledTask for ConcurrentCycleTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum_active.fetch_max(active, Ordering::SeqCst);
            let _active = ActiveCycle(Arc::clone(&self.active));
            context.cancellation().cancelled().await;
            Ok(())
        })
    }
}

struct ActiveCycle(Arc<AtomicUsize>);

impl Drop for ActiveCycle {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
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

impl WorkerLeaderLeaseGuard for FakeLeaseGuard {
    fn fencing_token(&self) -> WorkerFencingToken {
        self.fencing_token
    }

    fn renew(&mut self) -> BoxFuture<'_, Result<(), WorkerLeaseError>> {
        Box::pin(async move {
            lock_unpoisoned(&self.renewals).push(self.worker.clone());
            match lock_unpoisoned(&self.renew_scripts)
                .get_mut(&self.worker)
                .and_then(VecDeque::pop_front)
                .unwrap_or(RenewStep::Success)
            {
                RenewStep::Success => Ok(()),
                RenewStep::Error => Err(WorkerLeaseError::safe("expected renewal failure")),
            }
        })
    }

    fn release(self: Box<Self>) -> BoxFuture<'static, Result<(), WorkerLeaseError>> {
        Box::pin(async move {
            drop(self);
            Ok(())
        })
    }
}

impl Drop for FakeLeaseGuard {
    fn drop(&mut self) {
        lock_unpoisoned(&self.active_guards).remove(&self.worker);
    }
}

impl WorkerLeaderLeasePort for FakeLeasePort {
    fn try_acquire(
        &self,
        request: WorkerLeaseRequest,
    ) -> BoxFuture<'_, Result<WorkerLeaseAcquisition, WorkerLeaseError>> {
        Box::pin(async move {
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
                            NonZeroU64::new(fence).expect("positive fence"),
                        ),
                        active_guards: Arc::clone(&self.active_guards),
                        renew_scripts: Arc::clone(&self.renew_scripts),
                        renewals: Arc::clone(&self.renewals),
                    })))
                }
                LeaseStep::Busy(retry_after) => Ok(WorkerLeaseAcquisition::Busy { retry_after }),
                LeaseStep::Error => Err(WorkerLeaseError::safe("expected lease failure")),
                LeaseStep::Panic => panic!("expected isolated lease panic"),
            }
        })
    }
}

#[tokio::test]
async fn registry_composes_multiple_owners_but_rejects_duplicate_identity() {
    let duplicate = registration(
        WorkerKind::OAuthRefresh,
        "openai",
        CountingTask::new(0),
        test_schedule(),
        true,
    );
    let mut plan = complete_plan(vec![registration(
        WorkerKind::OAuthRefresh,
        "openai",
        CountingTask::new(0),
        test_schedule(),
        true,
    )]);
    plan.push(duplicate);
    let supervisor = supervisor();

    assert!(matches!(
        supervisor.start(plan, Arc::new(FakeLeasePort::default())),
        Err(WorkerStartError::Duplicate(id)) if id.owner() == "openai"
    ));
}

#[tokio::test]
async fn only_database_fact_reasons_can_disable_nonexistent_workers() {
    let mut plan = complete_plan(Vec::new());
    plan.push(WorkerContribution::Registration(registration_value(
        WorkerKind::OpsFlush,
        "store",
        CountingTask::new(0),
        test_schedule(),
        true,
    )));

    assert!(matches!(
        supervisor().start(plan, Arc::new(FakeLeasePort::default())),
        Err(WorkerStartError::KindDisabled(WorkerKind::OpsFlush))
    ));
}

#[tokio::test]
async fn registry_refuses_start_when_any_real_owner_kind_is_missing() {
    let plan = complete_plan(Vec::new())
        .into_iter()
        .filter(|item| item.kind() != WorkerKind::Retention)
        .collect();

    assert!(matches!(
        supervisor().start(plan, Arc::new(FakeLeasePort::default())),
        Err(WorkerStartError::Missing(missing)) if missing == vec![WorkerKind::Retention]
    ));
}

#[tokio::test]
async fn disabled_database_facts_never_execute_or_acquire_a_lease() {
    let leases = Arc::new(FakeLeasePort::default());
    let supervisor = supervisor();
    supervisor
        .start(complete_plan(Vec::new()), leases.clone())
        .expect("complete plan");
    wait_until(|| leases.requests().len() >= ACTIVE_KINDS.len()).await;
    let snapshot = supervisor.health_source().snapshot();

    assert!(snapshot.iter().any(|state| {
        state.key == WorkerHealthKey::Disabled(WorkerKind::OpsFlush)
            && state.state == WorkerRuntimeState::Disabled
    }));
    assert!(
        leases
            .requests()
            .iter()
            .all(|request| { request.worker().kind() != WorkerKind::OpsFlush })
    );
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn one_kind_runs_all_registered_owner_tasks() {
    let second = CountingTask::new(0);
    let plan = complete_plan(vec![
        registration(
            WorkerKind::OAuthRefresh,
            "openai",
            CountingTask::new(0),
            test_schedule(),
            true,
        ),
        registration(
            WorkerKind::OAuthRefresh,
            "xai",
            second.clone(),
            test_schedule(),
            true,
        ),
    ]);
    let supervisor = supervisor();
    supervisor
        .start(plan, Arc::new(FakeLeasePort::default()))
        .expect("complete plan");
    wait_until(|| second.runs.load(Ordering::SeqCst) >= 1).await;

    assert_eq!(
        supervisor
            .health_source()
            .snapshot()
            .iter()
            .filter(|state| matches!(&state.key, WorkerHealthKey::Task(id) if id.kind() == WorkerKind::OAuthRefresh))
            .count(),
        2
    );
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn leader_guard_lives_through_cycle_and_fence_reaches_owner() {
    let worker = target_worker();
    let leases = Arc::new(FakeLeasePort::default());
    let task = LeaseAwareTask {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
        completed: Arc::new(AtomicBool::new(false)),
        saw_live_guard: Arc::new(AtomicBool::new(false)),
        expected_worker: worker.clone(),
        active_guards: Arc::clone(&leases.active_guards),
        fence: Arc::new(AtomicU64::new(0)),
    };
    let supervisor = supervisor();
    supervisor
        .start(
            target_plan(task.clone(), long_interval_schedule()),
            leases.clone(),
        )
        .expect("complete plan");
    task.entered.notified().await;

    assert!(task.saw_live_guard.load(Ordering::SeqCst));
    assert!(lock_unpoisoned(&leases.active_guards).contains(&worker));
    task.release.notify_one();
    wait_until(|| task.completed.load(Ordering::SeqCst)).await;
    wait_until(|| !lock_unpoisoned(&leases.active_guards).contains(&worker)).await;
    assert!(task.fence.load(Ordering::SeqCst) > 0);
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn long_cycle_renews_leader_lease_before_ttl() {
    let leases = Arc::new(FakeLeasePort::default());
    let task = lease_aware_task(&leases);
    let supervisor = supervisor();
    supervisor
        .start(
            target_plan(task.clone(), renewal_schedule()),
            leases.clone(),
        )
        .expect("complete plan");
    task.entered.notified().await;
    tokio::time::advance(Duration::from_millis(10)).await;
    yield_until(|| leases.renewal_count(&target_worker()) >= 1).await;

    assert!(lock_unpoisoned(&leases.active_guards).contains(&target_worker()));
    task.release.notify_one();
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn renewal_failure_cancels_old_cycle_before_another_leader_cycle_starts() {
    let task = ConcurrentCycleTask {
        starts: Arc::new(AtomicUsize::new(0)),
        active: Arc::new(AtomicUsize::new(0)),
        maximum_active: Arc::new(AtomicUsize::new(0)),
    };
    let leases = Arc::new(FakeLeasePort::default());
    leases.renew_script(target_worker(), [RenewStep::Error]);
    let supervisor = supervisor();
    supervisor
        .start(target_plan(task.clone(), renewal_schedule()), leases)
        .expect("complete plan");
    yield_until(|| task.starts.load(Ordering::SeqCst) == 1).await;
    tokio::time::advance(Duration::from_millis(10)).await;
    yield_until(|| task.active.load(Ordering::SeqCst) == 0).await;
    tokio::time::advance(Duration::from_millis(5)).await;
    yield_until(|| task.starts.load(Ordering::SeqCst) == 2).await;

    assert_eq!(task.maximum_active.load(Ordering::SeqCst), 1);
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn supervisor_uses_exponential_backoff_and_recovers_health() {
    let task = CountingTask::new(2);
    let supervisor = supervisor();
    supervisor
        .start(
            target_plan(task.clone(), backoff_schedule()),
            Arc::new(FakeLeasePort::default()),
        )
        .expect("complete plan");
    yield_until(|| task.runs.load(Ordering::SeqCst) == 1).await;
    tokio::time::advance(Duration::from_millis(10)).await;
    yield_until(|| task.runs.load(Ordering::SeqCst) == 2).await;
    tokio::time::advance(Duration::from_millis(20)).await;
    yield_until(|| task.runs.load(Ordering::SeqCst) == 3).await;
    let health = task_health(&supervisor, &target_worker());

    assert_eq!(
        (health.consecutive_failures, health.completed_cycles),
        (0, 1)
    );
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn task_and_lease_port_panics_are_isolated_and_retried() {
    let task = PanicOnceTask {
        runs: Arc::new(AtomicUsize::new(0)),
    };
    let leases = Arc::new(FakeLeasePort::default());
    leases.script(
        target_worker(),
        [LeaseStep::Error, LeaseStep::Panic, LeaseStep::Acquired],
    );
    let supervisor = supervisor();
    supervisor
        .start(target_plan(task.clone(), backoff_schedule()), leases)
        .expect("complete plan");
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(20)).await;
    yield_until(|| task.runs.load(Ordering::SeqCst) == 1).await;
    tokio::time::advance(Duration::from_millis(20)).await;
    yield_until(|| task.runs.load(Ordering::SeqCst) >= 2).await;

    assert_eq!(
        task_health(&supervisor, &target_worker()).consecutive_failures,
        0
    );
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn lease_busy_is_healthy_standby_and_retries_without_running_owner() {
    let task = CountingTask::new(0);
    let leases = Arc::new(FakeLeasePort::default());
    leases.script(
        target_worker(),
        [
            LeaseStep::Busy(Some(Duration::from_millis(7))),
            LeaseStep::Acquired,
        ],
    );
    let supervisor = supervisor();
    supervisor
        .start(target_plan(task.clone(), backoff_schedule()), leases)
        .expect("complete plan");
    yield_until(|| task_health(&supervisor, &target_worker()).state == WorkerRuntimeState::Standby)
        .await;

    assert_eq!(task.runs.load(Ordering::SeqCst), 0);
    tokio::time::advance(Duration::from_millis(7)).await;
    yield_until(|| task.runs.load(Ordering::SeqCst) == 1).await;
    supervisor.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn cooperative_shutdown_propagates_cancel_before_dropping_all_guards() {
    let task = CancellationTask {
        entered: Arc::new(AtomicUsize::new(0)),
        observed: Arc::new(AtomicUsize::new(0)),
    };
    let leases = Arc::new(FakeLeasePort::default());
    let supervisor = supervisor();
    supervisor
        .start(all_active_plan(task.clone()), leases.clone())
        .expect("complete plan");
    wait_until(|| task.entered.load(Ordering::SeqCst) == ACTIVE_KINDS.len()).await;
    supervisor.shutdown(Duration::from_secs(1)).await;

    assert_eq!(task.observed.load(Ordering::SeqCst), ACTIVE_KINDS.len());
    assert!(lock_unpoisoned(&leases.active_guards).is_empty());
}

#[tokio::test]
async fn shutdown_timeout_aborts_uncooperative_task_and_drops_guard() {
    let task = HungTask {
        entered: Arc::new(AtomicBool::new(false)),
        notification: Arc::new(Notify::new()),
    };
    let leases = Arc::new(FakeLeasePort::default());
    let supervisor = supervisor();
    supervisor
        .start(
            target_plan(task.clone(), long_interval_schedule()),
            leases.clone(),
        )
        .expect("complete plan");
    task.notification.notified().await;
    supervisor.shutdown(Duration::from_millis(10)).await;

    assert!(lock_unpoisoned(&leases.active_guards).is_empty());
    assert_eq!(
        task_health(&supervisor, &target_worker()).state,
        WorkerRuntimeState::Stopped
    );
}

#[tokio::test]
async fn running_worker_becomes_unhealthy_when_last_success_is_stale() {
    let task = SucceedsThenHangsTask {
        runs: Arc::new(AtomicUsize::new(0)),
        second_cycle: Arc::new(Notify::new()),
    };
    let supervisor = supervisor();
    supervisor
        .start(
            target_plan(task.clone(), freshness_schedule()),
            Arc::new(FakeLeasePort::default()),
        )
        .expect("complete plan");
    task.second_cycle.notified().await;
    tokio::time::sleep(Duration::from_millis(45)).await;

    assert_eq!(
        task_health(&supervisor, &target_worker()).state,
        WorkerRuntimeState::BackingOff
    );
    supervisor.shutdown(Duration::from_millis(10)).await;
}

fn supervisor() -> WorkerSupervisor {
    WorkerSupervisor::new(CancellationToken::new())
}

fn registration(
    kind: WorkerKind,
    owner: &str,
    task: impl ScheduledTask + 'static,
    schedule: WorkerSchedule,
    leased: bool,
) -> WorkerContribution {
    WorkerContribution::Registration(registration_value(kind, owner, task, schedule, leased))
}

fn registration_value(
    kind: WorkerKind,
    owner: &str,
    task: impl ScheduledTask + 'static,
    schedule: WorkerSchedule,
    leased: bool,
) -> WorkerRegistration {
    let id = WorkerId::try_new(kind, owner).expect("valid worker id");
    let lease = leased
        .then(|| WorkerLeaseRequest::try_new(id.clone(), schedule.leader_lease_ttl()))
        .transpose()
        .expect("valid lease");
    WorkerRegistration::try_new(
        id,
        WorkerRunnable::Scheduled {
            schedule,
            lease,
            task: Box::new(task),
        },
    )
    .expect("valid registration")
}

fn complete_plan(mut custom: Vec<WorkerContribution>) -> Vec<WorkerContribution> {
    let present = custom
        .iter()
        .map(WorkerContribution::kind)
        .collect::<BTreeSet<_>>();
    for kind in ACTIVE_KINDS {
        if !present.contains(&kind) {
            custom.push(registration(
                kind,
                kind.as_str(),
                CountingTask::new(0),
                long_interval_schedule(),
                true,
            ));
        }
    }
    custom.push(WorkerContribution::Disabled {
        kind: WorkerKind::OpsFlush,
        reason: WorkerDisabledReason::NoBufferedOpsEvents,
    });
    custom
}

fn target_plan(
    task: impl ScheduledTask + 'static,
    schedule: WorkerSchedule,
) -> Vec<WorkerContribution> {
    complete_plan(vec![registration(
        WorkerKind::OAuthRefresh,
        "openai",
        task,
        schedule,
        true,
    )])
}

fn all_active_plan(task: impl ScheduledTask + Clone + 'static) -> Vec<WorkerContribution> {
    let mut plan = ACTIVE_KINDS
        .into_iter()
        .map(|kind| registration(kind, kind.as_str(), task.clone(), test_schedule(), true))
        .collect::<Vec<_>>();
    plan.push(WorkerContribution::Disabled {
        kind: WorkerKind::OpsFlush,
        reason: WorkerDisabledReason::NoBufferedOpsEvents,
    });
    plan
}

fn lease_aware_task(leases: &Arc<FakeLeasePort>) -> LeaseAwareTask {
    LeaseAwareTask {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
        completed: Arc::new(AtomicBool::new(false)),
        saw_live_guard: Arc::new(AtomicBool::new(false)),
        expected_worker: target_worker(),
        active_guards: Arc::clone(&leases.active_guards),
        fence: Arc::new(AtomicU64::new(0)),
    }
}

fn target_worker() -> WorkerId {
    WorkerId::try_new(WorkerKind::OAuthRefresh, "openai").expect("valid target worker")
}

fn task_health(supervisor: &WorkerSupervisor, id: &WorkerId) -> WorkerHealthSnapshot {
    supervisor
        .health_source()
        .snapshot()
        .into_iter()
        .find(|state| state.key == WorkerHealthKey::Task(id.clone()))
        .expect("worker health")
}

fn schedule(
    interval: Duration,
    initial: Duration,
    maximum: Duration,
    ttl: Duration,
    renewal: Duration,
) -> WorkerSchedule {
    WorkerSchedule::try_new(interval, initial, maximum, ttl, renewal).expect("valid schedule")
}

fn test_schedule() -> WorkerSchedule {
    schedule(
        Duration::from_millis(20),
        Duration::from_millis(5),
        Duration::from_millis(20),
        Duration::from_secs(30),
        Duration::from_secs(10),
    )
}

fn long_interval_schedule() -> WorkerSchedule {
    schedule(
        Duration::from_secs(60),
        Duration::from_millis(5),
        Duration::from_millis(20),
        Duration::from_secs(30),
        Duration::from_secs(10),
    )
}

fn backoff_schedule() -> WorkerSchedule {
    schedule(
        Duration::from_secs(60),
        Duration::from_millis(10),
        Duration::from_millis(20),
        Duration::from_secs(30),
        Duration::from_secs(10),
    )
}

fn renewal_schedule() -> WorkerSchedule {
    schedule(
        Duration::from_secs(60),
        Duration::from_millis(5),
        Duration::from_millis(20),
        Duration::from_millis(30),
        Duration::from_millis(10),
    )
}

fn freshness_schedule() -> WorkerSchedule {
    schedule(
        Duration::from_millis(10),
        Duration::from_millis(5),
        Duration::from_millis(10),
        Duration::from_millis(30),
        Duration::from_millis(10),
    )
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

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
