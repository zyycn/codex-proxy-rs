//! 验证请求就绪、综合健康状态与故障诊断

use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures::future::BoxFuture;
use gateway_core::{
    engine::execution::{
        AuthenticatedClient, ClientAuthenticationError, ExecutionService, StartExecution,
        StartProviderExecution, StartedExecution,
    },
    error::GatewayError,
    health::{
        HealthProbe, HealthState, WorkerHealthKey, WorkerHealthSnapshot, WorkerHealthSource,
        WorkerRuntimeState,
    },
    routing::PublicModelId,
    task::{WorkerId, WorkerKind},
};
use tower::ServiceExt;

use crate::support::RecordingDiagnostics;

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

#[tokio::test]
async fn readiness_ignores_freeze_recovery_and_logging_failures_without_clearing_alerts() {
    let diagnostics = Arc::new(RecordingDiagnostics::default());
    let router = health_router(
        vec![Arc::new(MutableProbe(Mutex::new(HealthState::Degraded(
            "file logging maintenance failed".to_owned(),
        ))))],
        vec![worker_snapshot(
            WorkerKind::AccountFreezeRecovery,
            WorkerRuntimeState::BackingOff,
        )],
        diagnostics.clone(),
    )
    .await;

    for (path, expected) in [
        ("/healthz", StatusCode::SERVICE_UNAVAILABLE),
        ("/readyz", StatusCode::NO_CONTENT),
        ("/healthz", StatusCode::SERVICE_UNAVAILABLE),
    ] {
        assert_eq!(
            health_response(router.clone(), path).await.status(),
            expected
        );
    }
    assert_eq!(diagnostics.0.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn readiness_rejects_an_unavailable_runtime_snapshot() {
    let router = health_router(
        vec![Arc::new(
            gateway_core::runtime::RuntimeSnapshotHandle::default(),
        )],
        Vec::new(),
        Arc::new(RecordingDiagnostics::default()),
    )
    .await;
    let response = health_response(router, "/readyz").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn healthz_records_probe_changes_once_and_records_recurrence_after_recovery() {
    let probe = Arc::new(MutableProbe(Mutex::new(HealthState::Unhealthy(
        "file logging rejected records".to_owned(),
    ))));
    let diagnostics = Arc::new(RecordingDiagnostics::default());
    let router = health_router(vec![probe.clone()], Vec::new(), diagnostics.clone()).await;

    let responses =
        futures::future::join_all((0..8).map(|_| health_response(router.clone(), "/healthz")))
            .await;
    for response in responses {
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .is_empty()
        );
    }
    {
        let events = diagnostics.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].component, "health");
        assert_eq!(
            events[0].correlation_id.as_deref(),
            Some("probe:file_logging")
        );
        assert_eq!(events[0].message, "Health check failed");
        assert!(
            events[0]
                .details
                .as_ref()
                .unwrap()
                .as_str()
                .contains("file logging rejected records")
        );
    }

    *probe.0.lock().unwrap() = HealthState::Degraded("file logging maintenance failed".to_owned());
    assert_eq!(
        health_response(router.clone(), "/healthz").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(diagnostics.0.lock().unwrap().len(), 2);
    *probe.0.lock().unwrap() = HealthState::Healthy;
    assert_eq!(
        health_response(router.clone(), "/healthz").await.status(),
        StatusCode::NO_CONTENT
    );
    *probe.0.lock().unwrap() = HealthState::Degraded("file logging maintenance failed".to_owned());
    assert_eq!(
        health_response(router, "/healthz").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(diagnostics.0.lock().unwrap().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn healthz_retains_worker_and_completed_probe_failures_when_another_probe_times_out() {
    let diagnostics = Arc::new(RecordingDiagnostics::default());
    let router = health_router(
        vec![
            Arc::new(MutableProbe(Mutex::new(HealthState::Unhealthy(
                "file output failed".to_owned(),
            )))),
            Arc::new(PendingProbe),
        ],
        vec![
            worker_snapshot(
                WorkerKind::RuntimeSnapshotReconciliation,
                WorkerRuntimeState::BackingOff,
            ),
            worker_snapshot(
                WorkerKind::QuotaCatalogHealth,
                WorkerRuntimeState::BackingOff,
            ),
        ],
        diagnostics.clone(),
    )
    .await;

    assert_eq!(
        health_response(router.clone(), "/readyz").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        health_response(router, "/healthz").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let events = diagnostics.0.lock().unwrap();
    assert_eq!(events.len(), 3);
    for (component, reason) in [
        (
            "worker:runtime_snapshot_reconciliation:health-test",
            "test worker failure",
        ),
        ("probe:file_logging", "file output failed"),
        ("probe:pending", "timed_out"),
    ] {
        let event = events
            .iter()
            .find(|event| event.correlation_id.as_deref() == Some(component))
            .expect(component);
        assert!(event.details.as_ref().unwrap().as_str().contains(reason));
    }
}

#[tokio::test]
async fn healthz_retries_failed_diagnostic_admission_without_changing_health_result() {
    let diagnostics = Arc::new(FailingOnceDiagnostics::default());
    let router = health_router(
        vec![Arc::new(MutableProbe(Mutex::new(HealthState::Unhealthy(
            "file output failed".to_owned(),
        ))))],
        Vec::new(),
        diagnostics.clone(),
    )
    .await;

    assert_eq!(
        health_response(router.clone(), "/healthz").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(diagnostics.recorded.0.lock().unwrap().is_empty());
    assert_eq!(
        health_response(router, "/healthz").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(diagnostics.recorded.0.lock().unwrap().len(), 1);
}

#[derive(Default)]
struct FailingOnceDiagnostics {
    failed: std::sync::atomic::AtomicBool,
    recorded: RecordingDiagnostics,
}

#[async_trait::async_trait]
impl gateway_core::diagnostics::OperationalDiagnostics for FailingOnceDiagnostics {
    async fn record_failure(
        &self,
        failure: gateway_core::diagnostics::OperationalFailure,
    ) -> Result<(), gateway_core::error::StoreError> {
        if !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(gateway_core::error::StoreError::new(
                gateway_core::error::StoreErrorKind::Unavailable,
            ));
        }
        self.recorded.record_failure(failure).await
    }
}

async fn health_router(
    probes: Vec<Arc<dyn HealthProbe>>,
    workers: Vec<WorkerHealthSnapshot>,
    diagnostics: Arc<dyn gateway_core::diagnostics::OperationalDiagnostics>,
) -> axum::Router {
    let admin = crate::admin::AdminTestFixture::new().await;
    gateway_api::initialize(
        uuid::Uuid::nil(),
        gateway_api::ApiConfig {
            asset_directory: std::env::temp_dir(),
            cors_allowed_origins: Vec::new(),
            request_timeout_seconds: None,
            request_id_header: "x-request-id".to_owned(),
        },
        Arc::new(UnusedExecution),
        admin.services,
        probes,
        Arc::new(StaticWorkerHealth(workers)),
        Arc::new(crate::openai::TestLifecycle::default()),
        diagnostics,
    )
    .unwrap()
    .router()
}

async fn health_response(router: axum::Router, path: &str) -> axum::response::Response {
    router
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

struct MutableProbe(Mutex<HealthState>);

impl HealthProbe for MutableProbe {
    fn name(&self) -> &'static str {
        "file_logging"
    }

    fn check(&self) -> BoxFuture<'_, HealthState> {
        Box::pin(std::future::ready(self.0.lock().unwrap().clone()))
    }
}

struct PendingProbe;

impl HealthProbe for PendingProbe {
    fn name(&self) -> &'static str {
        "pending"
    }

    fn required_for_readiness(&self) -> bool {
        true
    }

    fn check(&self) -> BoxFuture<'_, HealthState> {
        Box::pin(std::future::pending())
    }
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

    fn start_provider_endpoint(
        &self,
        _: StartProviderExecution,
    ) -> BoxFuture<'_, Result<StartedExecution, GatewayError>> {
        Box::pin(async { unreachable!("health check does not execute provider endpoints") })
    }
}
