#[tokio::test]
async fn task_coordinator_should_shutdown_without_panicking() {
    let coordinator = codex_proxy_rs::runtime::tasks::coordinator::TaskCoordinator::default();
    coordinator.shutdown().await;
}
