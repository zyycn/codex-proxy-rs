use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gateway_core::engine::CancellationToken;
use gateway_core::lifecycle::{ConnectionDraining, ConnectionGuard, ConnectionLifecycle};

#[derive(Default)]
struct LifecycleState {
    draining: AtomicBool,
    active: AtomicUsize,
}

struct TestGuard {
    state: Arc<LifecycleState>,
}

impl ConnectionGuard for TestGuard {}

impl Drop for TestGuard {
    fn drop(&mut self) {
        self.state.active.fetch_sub(1, Ordering::AcqRel);
    }
}

struct TestLifecycle {
    state: Arc<LifecycleState>,
    cancellation: CancellationToken,
}

impl TestLifecycle {
    fn new() -> Self {
        Self {
            state: Arc::new(LifecycleState::default()),
            cancellation: CancellationToken::new(),
        }
    }

    fn begin_draining(&self) {
        self.state.draining.store(true, Ordering::Release);
        self.cancellation.cancel();
    }

    fn active(&self) -> usize {
        self.state.active.load(Ordering::Acquire)
    }
}

impl ConnectionLifecycle for TestLifecycle {
    fn try_register(&self) -> Result<Box<dyn ConnectionGuard>, ConnectionDraining> {
        if self.state.draining.load(Ordering::Acquire) {
            return Err(ConnectionDraining);
        }
        self.state.active.fetch_add(1, Ordering::AcqRel);
        if self.state.draining.load(Ordering::Acquire) {
            self.state.active.fetch_sub(1, Ordering::AcqRel);
            return Err(ConnectionDraining);
        }
        Ok(Box::new(TestGuard {
            state: Arc::clone(&self.state),
        }))
    }

    fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    fn is_draining(&self) -> bool {
        self.state.draining.load(Ordering::Acquire)
    }
}

#[test]
fn connection_guard_drop_releases_active_registration() {
    let lifecycle = TestLifecycle::new();
    let guard = lifecycle.try_register().expect("registration before drain");
    drop(guard);

    assert_eq!(lifecycle.active(), 0);
}

#[test]
fn connection_registration_rejects_after_drain_linearization() {
    let lifecycle = TestLifecycle::new();
    lifecycle.begin_draining();

    assert!(matches!(lifecycle.try_register(), Err(ConnectionDraining)));
}

#[test]
fn connection_lifecycle_contract_is_object_safe() {
    let lifecycle = TestLifecycle::new();
    let object: &dyn ConnectionLifecycle = &lifecycle;

    assert!(!object.is_draining());
}
