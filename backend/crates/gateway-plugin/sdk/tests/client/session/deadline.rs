//! 验证续体等待不计入预算，恢复、到期和资源流切换不会重置已消耗时间

use std::time::Duration;

use gateway_plugin_sdk::client::CallDeadline;
use tokio::time::{Instant, advance};

#[tokio::test(start_paused = true)]
async fn suspension_preserves_remaining_budget_and_wakes_existing_waiters() {
    let deadline = CallDeadline::new(Instant::now() + Duration::from_secs(10));
    let waiter = tokio::spawn({
        let deadline = deadline.clone();
        async move { deadline.wait().await }
    });
    tokio::task::yield_now().await;
    advance(Duration::from_secs(3)).await;
    let pause = deadline.pause().unwrap();
    advance(Duration::from_secs(30)).await;
    assert!(!waiter.is_finished());
    assert_eq!(deadline.remaining(), Some(Duration::from_secs(7)));
    drop(pause);
    advance(Duration::from_secs(6)).await;
    assert!(!waiter.is_finished());
    advance(Duration::from_secs(1)).await;
    waiter.await.unwrap();
    assert!(deadline.is_elapsed());
}

#[tokio::test(start_paused = true)]
async fn exhausted_budget_cannot_be_suspended_or_revived() {
    let deadline = CallDeadline::new(Instant::now() + Duration::from_secs(1));
    advance(Duration::from_secs(1)).await;
    assert!(deadline.pause().is_none());
    deadline.wait().await;
    assert!(deadline.is_elapsed());
}

#[tokio::test(start_paused = true)]
async fn resource_stream_clear_survives_an_outstanding_pause_guard() {
    let deadline = CallDeadline::new(Instant::now() + Duration::from_secs(1));
    let pause = deadline.pause().unwrap();
    assert!(deadline.pause().is_none());
    deadline.clear();
    drop(pause);
    advance(Duration::from_secs(10)).await;
    assert_eq!(deadline.remaining(), None);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), deadline.wait())
            .await
            .is_err()
    );
}
