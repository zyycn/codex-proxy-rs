//! 验证取消树的唤醒、父子隔离、竞态处理与资源释放

use std::future::Future as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::task::{Context, Wake, Waker};

use futures::{FutureExt as _, pin_mut};
use gateway_core::lifecycle::CancellationToken;

#[test]
fn discarded_cancellation_branches_release_allocations() {
    let token = CancellationToken::new();
    // 预热一次，允许取消信号保留固定大小的通知状态
    discard_cancellation_branch(&token);

    let allocations = allocation_counter::measure(|| {
        for _ in 0..10_000 {
            discard_cancellation_branch(&token);
        }
    });

    assert_eq!(allocations.count_current, 0, "{allocations:?}");
    assert_eq!(allocations.bytes_current, 0, "{allocations:?}");
    assert!(!token.is_cancelled());
}

#[test]
fn dropped_descendant_waits_release_ancestor_allocations() {
    let parent = CancellationToken::new();
    let child = parent.child_token();
    let grandchild = child.child_token();
    discard_cancellation_branch(&grandchild);

    let allocations = allocation_counter::measure(|| {
        for _ in 0..10_000 {
            discard_cancellation_branch(&grandchild);
        }
    });

    assert_eq!(allocations.count_current, 0, "{allocations:?}");
    assert_eq!(allocations.bytes_current, 0, "{allocations:?}");
    assert!(!parent.is_cancelled());
}

#[test]
fn completing_child_cancellation_releases_ancestor_allocations() {
    let parent = CancellationToken::new();
    discard_cancellation_branch(&parent);

    let allocations = allocation_counter::measure(|| {
        for _ in 0..1_000 {
            let child = parent.child_token();
            let mut waiting = Box::pin(child.cancelled());
            assert!(waiting.as_mut().now_or_never().is_none());
            child.cancel();
            assert!(waiting.now_or_never().is_some());
        }
    });

    assert_eq!(allocations.count_current, 0, "{allocations:?}");
    assert_eq!(allocations.bytes_current, 0, "{allocations:?}");
    assert!(!parent.is_cancelled());
}

fn discard_cancellation_branch(token: &CancellationToken) {
    // select 先轮询取消分支，再由另一个就绪分支获胜并丢弃取消等待
    let selected = futures::future::select(Box::pin(token.cancelled()), futures::future::ready(()))
        .now_or_never();
    assert!(matches!(selected, Some(futures::future::Either::Right(_))));
}

#[derive(Default)]
struct WakeCounter(AtomicUsize);

impl Wake for WakeCounter {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn cancellation_wakes_all_live_waiters_after_other_waits_are_dropped() {
    let parent = CancellationToken::new();
    let child = parent.child_token();
    let grandchild = child.child_token();
    let sibling = parent.child_token();
    let tokens = [&parent, &child, &grandchild, &sibling];
    let mut waiters = Vec::new();

    for token in tokens.into_iter().cycle().take(64) {
        let counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&counter));
        let mut waiting = Box::pin(token.cancelled());
        assert!(
            waiting
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        waiters.push((waiting, counter));
    }
    for _ in 0..100 {
        discard_cancellation_branch(&grandchild);
    }

    parent.clone().cancel();
    parent.cancel();

    for (waiting, counter) in waiters {
        assert!(counter.0.load(Ordering::Relaxed) > 0);
        assert!(waiting.now_or_never().is_some());
        assert_eq!(Arc::strong_count(&counter), 1);
    }
}

#[test]
fn cancelling_child_only_wakes_its_descendants() {
    let parent = CancellationToken::new();
    let child = parent.child_token();
    let grandchild = child.child_token();
    let sibling = parent.child_token();
    let mut parent_wait = Box::pin(parent.cancelled());
    let mut grandchild_wait = Box::pin(grandchild.cancelled());
    let mut sibling_wait = Box::pin(sibling.cancelled());
    assert!(parent_wait.as_mut().now_or_never().is_none());
    assert!(grandchild_wait.as_mut().now_or_never().is_none());
    assert!(sibling_wait.as_mut().now_or_never().is_none());

    child.cancel();

    assert!(grandchild_wait.now_or_never().is_some());
    assert!(parent_wait.now_or_never().is_none());
    assert!(sibling_wait.now_or_never().is_none());
    assert!(!parent.is_cancelled());
    assert!(!sibling.is_cancelled());
}

#[test]
fn cancellation_racing_with_wait_registration_and_drop_does_not_lose_wakeup() {
    for _ in 0..256 {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        let grandchild = child.child_token();
        let mut dropped_wait = Box::pin(grandchild.cancelled());
        assert!(dropped_wait.as_mut().now_or_never().is_none());
        let start = Barrier::new(2);
        let finished = Barrier::new(2);
        let counter = Arc::new(WakeCounter::default());
        let waker = Waker::from(Arc::clone(&counter));

        std::thread::scope(|scope| {
            scope.spawn(|| {
                start.wait();
                parent.cancel();
                finished.wait();
            });

            start.wait();
            drop(dropped_wait);
            let mut waiting = Box::pin(grandchild.cancelled());
            let pending = waiting
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending();
            finished.wait();

            // 取消完成后再检查，既验证就绪结果，也验证 Pending 路径实际收到唤醒
            if pending {
                assert!(counter.0.load(Ordering::Relaxed) > 0);
                assert!(waiting.now_or_never().is_some());
            }
        });
        assert!(grandchild.is_cancelled());
        assert_eq!(Arc::strong_count(&counter), 2);
    }
}

#[test]
fn descendants_created_after_cancellation_are_immediately_ready() {
    let parent = CancellationToken::new();
    parent.cancel();
    let child = parent.child_token();
    let grandchild = child.child_token();
    drop(parent);
    drop(child);

    assert!(grandchild.is_cancelled());
    assert!(grandchild.cancelled().now_or_never().is_some());
}

#[test]
fn cancellation_token_should_wake_current_state() {
    let token = CancellationToken::new();
    token.cancel();

    assert!(token.is_cancelled());
    assert!(token.cancelled().now_or_never().is_some());
}

#[test]
fn child_cancellation_inherits_parent_without_cancelling_parent() {
    futures::executor::block_on(async {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        child.cancel();

        child.cancelled().await;
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    });
}

#[test]
fn parent_cancellation_wakes_nested_descendants() {
    futures::executor::block_on(async {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        let grandchild = child.child_token();
        let waiting = grandchild.cancelled();
        pin_mut!(waiting);
        assert!(waiting.as_mut().now_or_never().is_none());

        parent.cancel();
        waiting.await;
        assert!(child.is_cancelled());
        assert!(grandchild.is_cancelled());
    });
}
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
