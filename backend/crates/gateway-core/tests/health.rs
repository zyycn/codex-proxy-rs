use futures::future::BoxFuture;
use gateway_core::health::{
    HealthProbe, HealthState, WorkerHealthKey, WorkerHealthSnapshot, WorkerHealthSource,
    WorkerRuntimeState,
};
use gateway_core::task::{WorkerId, WorkerKind};

struct HealthyProbe;

impl HealthProbe for HealthyProbe {
    fn name(&self) -> &'static str {
        "test"
    }

    fn check(&self) -> BoxFuture<'_, HealthState> {
        Box::pin(async { HealthState::Healthy })
    }
}

struct SingleSnapshot;

impl WorkerHealthSource for SingleSnapshot {
    fn snapshot(&self) -> Vec<WorkerHealthSnapshot> {
        vec![WorkerHealthSnapshot {
            key: WorkerHealthKey::Task(
                WorkerId::try_new(WorkerKind::Retention, "store").expect("valid worker"),
            ),
            state: WorkerRuntimeState::Idle,
            consecutive_failures: 0,
            completed_cycles: 3,
            last_fencing_token: None,
            last_success_at: None,
            last_failure_at: None,
            last_error: None,
        }]
    }
}

#[test]
fn health_probe_contract_is_object_safe() {
    let probe: &dyn HealthProbe = &HealthyProbe;

    assert_eq!(probe.name(), "test");
}

#[test]
fn worker_health_source_preserves_worker_identity() {
    let source: &dyn WorkerHealthSource = &SingleSnapshot;

    assert!(matches!(
        source.snapshot().as_slice(),
        [WorkerHealthSnapshot {
            key: WorkerHealthKey::Task(id),
            completed_cycles: 3,
            ..
        }] if id.owner() == "store"
    ));
}
