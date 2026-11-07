//! 验证账号租约关闭与申请交错、异步释放完成和清理超时

#[path = "../../../src/redis/credential_leases/cleanup.rs"]
mod implementation;

use gateway_core::lifecycle::CancellationToken;
use gateway_core::task::{DaemonTask, WorkerDaemonContext};
use implementation::SchedulingLeaseCleanup;
use std::time::Duration;
use tokio::sync::oneshot;

#[tokio::test]
async fn live_guard_keeps_its_lease_task_alive_after_the_port_is_dropped() {
    let cleanup = SchedulingLeaseCleanup::default();
    let (released, mut completed) = oneshot::channel();
    let guard = cleanup.track(CancellationToken::new(), async move {
        released.send(()).unwrap();
        Ok(true)
    });
    drop(cleanup);
    tokio::task::yield_now().await;
    assert!(matches!(
        completed.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    drop(guard);
}

#[tokio::test]
async fn shutdown_waits_for_pending_acquisitions_and_releases_without_waiting_for_guards() {
    let cleanup = SchedulingLeaseCleanup::default();
    let acquisition = cleanup.acquire().await.unwrap();
    let stopping = CancellationToken::new();
    let request = CancellationToken::new();
    let (released, completed) = oneshot::channel();
    let worker = {
        let cleanup = cleanup.clone();
        let stopping = stopping.clone();
        tokio::spawn(async move { cleanup.run(WorkerDaemonContext::new(stopping)).await })
    };
    stopping.cancel();
    tokio::task::yield_now().await;
    assert!(!worker.is_finished());
    // Redis 申请在停机开始后才返回，登记仍必须被本次清理包含
    let guard = cleanup.track(request.clone(), async move {
        released.send(()).unwrap();
        Ok(true)
    });
    drop(acquisition);
    tokio::time::timeout(Duration::from_secs(1), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    completed.await.unwrap();
    assert!(request.is_cancelled());
    assert!(cleanup.acquire().await.is_none());
    drop(guard);
}

#[tokio::test]
async fn shutdown_joins_a_release_already_started_by_guard_drop() {
    let cleanup = SchedulingLeaseCleanup::default();
    let acquisition = cleanup.acquire().await.unwrap();
    let request = CancellationToken::new();
    let (started, entered) = oneshot::channel();
    let (finish, finished) = oneshot::channel();
    let guard = cleanup.track(request.clone(), async move {
        started.send(()).unwrap();
        finished.await.unwrap();
        Ok(true)
    });
    drop(acquisition);
    drop(guard);
    entered.await.unwrap();
    assert!(!request.is_cancelled());
    let stopping = CancellationToken::new();
    stopping.cancel();
    let worker = tokio::spawn(async move { cleanup.run(WorkerDaemonContext::new(stopping)).await });
    tokio::task::yield_now().await;
    assert!(!worker.is_finished());
    finish.send(()).unwrap();
    worker.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn unavailable_release_does_not_hold_shutdown_past_its_budget() {
    let cleanup = SchedulingLeaseCleanup::default();
    let request = CancellationToken::new();
    let guard = cleanup.track(request.clone(), std::future::pending());
    let stopping = CancellationToken::new();
    stopping.cancel();
    tokio::time::timeout(
        Duration::from_secs(3),
        cleanup.run(WorkerDaemonContext::new(stopping)),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(request.is_cancelled());
    assert!(cleanup.acquire().await.is_none());
    drop(guard);
}

#[tokio::test(start_paused = true)]
async fn acquisition_returning_after_shutdown_timeout_is_still_released() {
    let cleanup = SchedulingLeaseCleanup::default();
    let acquisition = cleanup.acquire().await.unwrap();
    let stopping = CancellationToken::new();
    stopping.cancel();
    tokio::time::timeout(
        Duration::from_secs(3),
        cleanup.run(WorkerDaemonContext::new(stopping)),
    )
    .await
    .unwrap()
    .unwrap();
    let request = CancellationToken::new();
    let (released, completed) = oneshot::channel();
    let guard = cleanup.track(request.clone(), async move {
        released.send(()).unwrap();
        Ok(true)
    });
    drop(acquisition);
    tokio::time::timeout(Duration::from_secs(1), completed)
        .await
        .unwrap()
        .unwrap();
    assert!(request.is_cancelled());
    assert!(cleanup.acquire().await.is_none());
    drop(guard);
}
