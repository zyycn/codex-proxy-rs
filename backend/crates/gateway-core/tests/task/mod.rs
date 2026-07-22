use std::num::NonZeroU64;
use std::time::Duration;

use futures::future::BoxFuture;
use gateway_core::engine::CancellationToken;
use gateway_core::task::{
    DaemonRestartPolicy, DaemonTask, ScheduledTask, WorkerCycleContext, WorkerDefinitionError,
    WorkerFencingToken, WorkerId, WorkerKind, WorkerLeaseRequest, WorkerRegistration,
    WorkerRunnable, WorkerSchedule, WorkerTaskError,
};

struct NoopScheduledTask;

impl ScheduledTask for NoopScheduledTask {
    fn run_cycle(
        &self,
        _context: WorkerCycleContext,
    ) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async { Ok(()) })
    }
}

struct NoopDaemonTask;

impl DaemonTask for NoopDaemonTask {
    fn run(&self, _cancellation: CancellationToken) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn schedule_rejects_every_invalid_duration_boundary() {
    let one = Duration::from_nanos(1);
    let two = Duration::from_nanos(2);
    let three = Duration::from_nanos(3);
    for values in [
        (Duration::ZERO, one, one, three, one),
        (one, Duration::ZERO, one, three, one),
        (one, one, Duration::ZERO, three, one),
        (one, one, one, Duration::ZERO, one),
        (one, one, one, three, Duration::ZERO),
        (one, two, one, three, one),
        (one, one, one, three, three),
        (one, one, one, two, three),
    ] {
        assert_eq!(
            WorkerSchedule::try_new(values.0, values.1, values.2, values.3, values.4),
            Err(WorkerDefinitionError::InvalidSchedule),
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
        assert_eq!(
            WorkerId::try_new(WorkerKind::OAuthRefresh, owner),
            Err(WorkerDefinitionError::InvalidOwner),
        );
    }
}

#[test]
fn schedule_accepts_renewal_strictly_before_lease_expiry() {
    let schedule = WorkerSchedule::try_new(
        Duration::from_secs(60),
        Duration::from_secs(1),
        Duration::from_secs(30),
        Duration::from_secs(15),
        Duration::from_secs(5),
    )
    .expect("valid schedule");

    assert_eq!(
        schedule.leader_lease_renewal_interval(),
        Duration::from_secs(5)
    );
}

#[test]
fn registration_rejects_lease_for_a_different_worker() {
    let registration_id = worker_id("openai");
    let lease = WorkerLeaseRequest::try_new(worker_id("xai"), Duration::from_secs(15))
        .expect("valid lease");
    let runnable = WorkerRunnable::Scheduled {
        schedule: schedule(),
        lease: Some(lease),
        task: Box::new(NoopScheduledTask),
    };

    assert!(matches!(
        WorkerRegistration::try_new(registration_id, runnable),
        Err(WorkerDefinitionError::LeaseWorkerMismatch)
    ));
}

#[test]
fn registration_rejects_lease_ttl_that_differs_from_schedule() {
    let id = worker_id("openai");
    let lease =
        WorkerLeaseRequest::try_new(id.clone(), Duration::from_secs(16)).expect("valid lease");
    let runnable = WorkerRunnable::Scheduled {
        schedule: schedule(),
        lease: Some(lease),
        task: Box::new(NoopScheduledTask),
    };

    assert!(matches!(
        WorkerRegistration::try_new(id, runnable),
        Err(WorkerDefinitionError::LeaseTtlMismatch)
    ));
}

#[test]
fn daemon_restart_policy_rejects_reverse_backoff_range() {
    assert_eq!(
        DaemonRestartPolicy::try_new(Duration::from_secs(2), Duration::from_secs(1)),
        Err(WorkerDefinitionError::InvalidDaemonRestartPolicy)
    );
}

#[test]
fn worker_cycle_context_preserves_optional_fencing_token() {
    let token = WorkerFencingToken::new(NonZeroU64::new(7).expect("non-zero fencing token"));
    let context =
        WorkerCycleContext::new(worker_id("openai"), Some(token), CancellationToken::new());

    assert_eq!(context.fencing_token(), Some(token));
}

#[test]
fn scheduled_and_daemon_tasks_are_object_safe() {
    fn accept_scheduled(_task: &dyn ScheduledTask) {}
    fn accept_daemon(_task: &dyn DaemonTask) {}

    accept_scheduled(&NoopScheduledTask);
    accept_daemon(&NoopDaemonTask);
}

fn worker_id(owner: &str) -> WorkerId {
    WorkerId::try_new(WorkerKind::OAuthRefresh, owner).expect("valid worker identity")
}

fn schedule() -> WorkerSchedule {
    WorkerSchedule::try_new(
        Duration::from_secs(60),
        Duration::from_secs(1),
        Duration::from_secs(30),
        Duration::from_secs(15),
        Duration::from_secs(5),
    )
    .expect("valid schedule")
}
