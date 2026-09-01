use std::sync::Arc;
use std::time::Duration;

use gateway_core::lifecycle::CancellationToken;
use gateway_core::lifecycle::ConnectionLifecycle as _;
use gateway_host::HostBundle;
use gateway_host::serve::{ConnectionTracker, bind_listener};

#[test]
fn host_bundle_serve_is_a_consuming_process_entrypoint() {
    let _serve = HostBundle::serve;

    assert_eq!(std::mem::size_of_val(&_serve), 0);
}

#[tokio::test(start_paused = true)]
async fn wait_until_idle_should_return_immediately_without_active_connections() {
    let tracker = ConnectionTracker::new(CancellationToken::new());
    let started = tokio::time::Instant::now();

    tracker.wait_until_idle(Duration::from_secs(30)).await;

    assert_eq!(started.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn wait_until_idle_should_give_up_at_timeout_while_connections_remain() {
    let tracker = ConnectionTracker::new(CancellationToken::new());
    let _guard = tracker.try_register().expect("register connection");
    let started = tokio::time::Instant::now();

    tracker.wait_until_idle(Duration::from_secs(30)).await;

    assert_eq!(started.elapsed(), Duration::from_secs(30));
}

#[tokio::test]
async fn wait_until_idle_should_wake_when_last_guard_drops_after_first_poll() {
    let tracker = ConnectionTracker::new(CancellationToken::new());
    let guard = tracker.try_register().expect("register connection");
    let mut wait = Box::pin(tracker.wait_until_idle(Duration::from_secs(30)));
    assert!(futures::poll!(wait.as_mut()).is_pending());

    drop(guard);

    tokio::time::timeout(Duration::from_secs(2), wait)
        .await
        .expect("woken by last guard drop instead of waiting out the timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wait_until_idle_should_observe_guard_drop_racing_the_idle_check() {
    // 回归约束：notified 必须在读取活跃计数前完成注册（enable），否则最后
    // 一个 guard 在计数检查与首次 poll 之间 drop 时唤醒丢失，只能等满整个
    // 超时。多线程反复交错，任何一次丢唤醒都会撞上 2s 超时并触发断言。
    for _ in 0..256 {
        let tracker = Arc::new(ConnectionTracker::new(CancellationToken::new()));
        let guard = tracker.try_register().expect("register connection");
        let waiter = tokio::spawn({
            let tracker = Arc::clone(&tracker);
            async move { tracker.wait_until_idle(Duration::from_secs(2)).await }
        });
        let dropper = std::thread::spawn(move || drop(guard));
        let started = std::time::Instant::now();
        waiter.await.expect("waiter completes");
        dropper.join().expect("dropper completes");
        assert!(
            started.elapsed() < Duration::from_millis(1_900),
            "idle wakeup was lost and the waiter slept until the drain timeout"
        );
    }
}

#[tokio::test]
async fn bind_listener_should_retry_until_previous_listener_releases_port() {
    let holder = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("holder listener");
    let address = holder.local_addr().expect("holder address").to_string();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(holder);
    });

    let listener = tokio::time::timeout(Duration::from_secs(8), bind_listener(&address))
        .await
        .expect("bound within the retry window")
        .expect("bound after the previous listener released the port");

    assert_eq!(
        listener.local_addr().expect("bound address").to_string(),
        address
    );
}
