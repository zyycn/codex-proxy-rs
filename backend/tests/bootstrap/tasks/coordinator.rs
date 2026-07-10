use std::{sync::Arc, time::Duration};

use codex_proxy_rs::{
    bootstrap::tasks::coordinator::SchedulerHandle,
    upstream::openai::transport::{CodexWebSocketPool, CodexWebSocketPoolConfig},
};

#[tokio::test]
async fn task_coordinator_should_shutdown_without_panicking() {
    let coordinator = codex_proxy_rs::bootstrap::tasks::coordinator::TaskCoordinator::default();
    coordinator.shutdown().await;
}

#[tokio::test]
async fn scheduler_handle_should_shutdown_websocket_pool() {
    let pool = Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
        maintenance_interval: None,
        ping_interval: None,
        liveness_timeout: None,
        max_age: Duration::from_secs(60),
        max_per_account: 1,
        enabled: true,
        ping_timeout: Duration::ZERO,
        first_token_timeout: None,
    }));

    SchedulerHandle::from_websocket_pool(pool.clone())
        .shutdown()
        .await;

    assert!(pool.is_shutdown().await);
}
