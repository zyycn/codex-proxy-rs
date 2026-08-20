use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures::future::BoxFuture;
use gateway_core::{
    engine::execution::{
        AuthenticatedClient, ClientAuthenticationError, ExecutionService, StartExecution,
        StartedExecution,
    },
    error::GatewayError,
    health::{WorkerHealthKey, WorkerHealthSnapshot, WorkerHealthSource, WorkerRuntimeState},
    routing::PublicModelId,
    task::{WorkerId, WorkerKind},
};
use tower::ServiceExt;

#[tokio::test]
async fn healthz_should_return_no_content_when_all_inputs_are_healthy() {
    let response = crate::openai::api_router(Arc::new(UnusedExecution))
        .await
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .expect("health request"),
        )
        .await
        .expect("health response");

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn healthz_should_ignore_provider_maintenance_worker_failure() {
    let worker_health = StaticWorkerHealth(vec![worker_snapshot(
        WorkerKind::QuotaCatalogHealth,
        WorkerRuntimeState::BackingOff,
    )]);
    let response = crate::openai::api_router_with_worker_health(
        Arc::new(UnusedExecution),
        Arc::new(worker_health),
    )
    .await
    .oneshot(
        Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .expect("health request"),
    )
    .await
    .expect("health response");

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn healthz_should_reject_critical_worker_failure() {
    let worker_health = StaticWorkerHealth(vec![worker_snapshot(
        WorkerKind::RuntimeSnapshotReconciliation,
        WorkerRuntimeState::BackingOff,
    )]);
    let response = crate::openai::api_router_with_worker_health(
        Arc::new(UnusedExecution),
        Arc::new(worker_health),
    )
    .await
    .oneshot(
        Request::builder()
            .uri("/healthz")
            .body(Body::empty())
            .expect("health request"),
    )
    .await
    .expect("health response");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[derive(Clone)]
struct StaticWorkerHealth(Vec<WorkerHealthSnapshot>);

impl WorkerHealthSource for StaticWorkerHealth {
    fn snapshot(&self) -> Vec<WorkerHealthSnapshot> {
        self.0.clone()
    }
}

fn worker_snapshot(kind: WorkerKind, state: WorkerRuntimeState) -> WorkerHealthSnapshot {
    let id = WorkerId::try_new(kind, "health-test").expect("worker ID");
    WorkerHealthSnapshot {
        key: WorkerHealthKey::Task(id),
        state,
        consecutive_failures: 1,
        completed_cycles: 0,
        last_fencing_token: None,
        last_success_at: None,
        last_failure_at: None,
        last_error: Some("test worker failure".to_owned()),
    }
}

struct UnusedExecution;

impl ExecutionService for UnusedExecution {
    fn authenticate(&self, _: &str) -> Result<AuthenticatedClient, ClientAuthenticationError> {
        unreachable!("health check does not authenticate")
    }

    fn public_models(&self, _: &AuthenticatedClient) -> Vec<PublicModelId> {
        unreachable!("health check does not list models")
    }

    fn contains_public_model(&self, _: &AuthenticatedClient, _: &PublicModelId) -> bool {
        unreachable!("health check does not inspect models")
    }

    fn start(&self, _: StartExecution) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async { unreachable!("health check does not execute requests") })
    }
}
