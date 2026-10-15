//! 验证 Worker 健康状态的身份投影

use gateway_core::health::{
    WorkerHealthKey, WorkerHealthSnapshot, WorkerHealthSource, WorkerRuntimeState,
};
use gateway_core::task::{WorkerId, WorkerKind};

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
