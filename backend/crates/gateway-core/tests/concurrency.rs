use std::time::{Duration, Instant, SystemTime};

use futures::{FutureExt, executor::block_on};
use gateway_core::concurrency::{
    CapacityWait, ConcurrencyQueuePolicy, ConcurrencyWaitBudget, ConcurrencyWaitQueue,
    QueueRejection,
};

#[test]
fn waiting_is_bounded_per_owner_and_cancelled_head_wakes_the_next_request() {
    block_on(async {
        let queue = ConcurrencyWaitQueue::default();
        let deadline = Instant::now() + Duration::from_secs(2);
        let first = queue.enqueue(&["key_a"], 2, deadline).unwrap();
        let second = queue.enqueue(&["key_a"], 2, deadline).unwrap();
        assert!(matches!(
            queue.enqueue(&["key_a"], 2, deadline),
            Err(QueueRejection::Full)
        ));
        let independent = queue.enqueue(&["key_b"], 2, deadline).unwrap();
        assert!(second.turn().now_or_never().is_none());
        independent.turn().await.unwrap();
        first.turn().await.unwrap();
        drop(first);
        second.turn().await.unwrap();
        drop(second);
        assert!(!queue.has_waiters(&"key_a"));
    });
}

#[test]
fn cancelling_a_middle_waiter_preserves_fifo_and_reclaims_capacity() {
    block_on(async {
        let queue = ConcurrencyWaitQueue::default();
        let deadline = Instant::now() + Duration::from_secs(2);
        let head = queue.enqueue(&["account"], 3, deadline).unwrap();
        let cancelled = queue.enqueue(&["account"], 3, deadline).unwrap();
        let next = queue.enqueue(&["account"], 3, deadline).unwrap();
        drop(cancelled);
        let tail = queue.enqueue(&["account"], 3, deadline).unwrap();
        drop(head);
        next.turn().await.unwrap();
        assert!(tail.turn().now_or_never().is_none());
        drop(next);
        tail.turn().await.unwrap();
    });
}

#[test]
fn wait_deadline_applies_before_and_after_becoming_head() {
    block_on(async {
        let queue = ConcurrencyWaitQueue::default();
        let deadline = Instant::now() + Duration::from_millis(20);
        let head = queue.enqueue(&["account"], 2, deadline).unwrap();
        let tail = queue.enqueue(&["account"], 2, deadline).unwrap();
        assert_eq!(tail.turn().await, Err(QueueRejection::Timeout));
        assert_eq!(head.turn().await, Err(QueueRejection::Timeout));
        drop((head, tail));
        assert!(!queue.has_waiters(&"account"));
    });
}

#[test]
fn a_full_account_queue_does_not_prevent_waiting_on_another_eligible_account() {
    let queue = ConcurrencyWaitQueue::default();
    let deadline = Instant::now() + Duration::from_secs(2);
    let _first = queue.enqueue(&["a"], 1, deadline).unwrap();
    let second = queue.enqueue(&["a", "b"], 1, deadline).unwrap();
    assert_eq!(*second.key(), "b");
    assert!(matches!(
        queue.enqueue(&["a", "b"], 1, deadline),
        Err(QueueRejection::Full)
    ));
}

#[test]
fn request_deadline_bounds_waiting_and_dropped_future_releases_its_ticket() {
    block_on(async {
        let queue = ConcurrencyWaitQueue::default();
        let policy = ConcurrencyQueuePolicy {
            max_waiting: 1,
            timeout: Duration::from_secs(30),
        };
        let budget = ConcurrencyWaitBudget::default();
        let mut waiting = CapacityWait::new(
            &queue,
            policy,
            SystemTime::now() + Duration::from_millis(20),
            &budget,
        );
        assert_eq!(waiting.wait(&["a"]).await, Err(QueueRejection::Timeout));
        drop(waiting);
        assert!(!queue.has_waiters(&"a"));

        let budget = ConcurrencyWaitBudget::default();
        let mut waiting = CapacityWait::new(
            &queue,
            policy,
            SystemTime::now() + Duration::from_secs(2),
            &budget,
        );
        assert!(waiting.wait(&["a"]).now_or_never().is_none());
        assert!(queue.has_waiters(&"a"));
        drop(waiting);
        assert!(!queue.has_waiters(&"a"));
    });
}

#[test]
fn new_layers_and_retries_cannot_restart_a_requests_wait_budget() {
    block_on(async {
        let keys = ConcurrencyWaitQueue::default();
        let accounts = ConcurrencyWaitQueue::default();
        let policy = ConcurrencyQueuePolicy {
            max_waiting: 1,
            timeout: Duration::from_millis(300),
        };
        let request_deadline = SystemTime::now() + Duration::from_secs(5);
        let budget = ConcurrencyWaitBudget::default();
        let mut key_wait = CapacityWait::new(&keys, policy, request_deadline, &budget);
        key_wait.wait(&["key"]).await.unwrap();
        drop(key_wait);

        // 第一层已取得重试机会，后续准备跨过总时限后不能在账号层重新排队。
        futures_timer::Delay::new(policy.timeout).await;
        let next_attempt_budget = budget.clone();
        let mut account_wait =
            CapacityWait::new(&accounts, policy, request_deadline, &next_attempt_budget);
        assert_eq!(
            account_wait.wait(&["account"]).now_or_never(),
            Some(Err(QueueRejection::Timeout))
        );
        assert!(!accounts.has_waiters(&"account"));

        // 另一个请求仍有完整预算，不能受上一请求超时影响。
        let independent_budget = ConcurrencyWaitBudget::default();
        let mut independent =
            CapacityWait::new(&accounts, policy, request_deadline, &independent_budget);
        independent.wait(&["account"]).await.unwrap();
    });
}

#[test]
fn creating_a_waiter_does_not_start_the_budget_before_queueing() {
    block_on(async {
        let queue = ConcurrencyWaitQueue::default();
        let policy = ConcurrencyQueuePolicy {
            max_waiting: 1,
            timeout: Duration::from_millis(300),
        };
        let budget = ConcurrencyWaitBudget::default();
        let mut waiting = CapacityWait::new(
            &queue,
            policy,
            SystemTime::now() + Duration::from_secs(5),
            &budget,
        );
        futures_timer::Delay::new(policy.timeout).await;
        waiting.wait(&["account"]).await.unwrap();
    });
}
