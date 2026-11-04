//! 请求就绪检查与综合健康诊断

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{extract::State, http::StatusCode};
use futures::future::join_all;
use gateway_core::diagnostics::{OperationalDiagnostics, OperationalFailure};
use gateway_core::error::{ErrorDetails, ErrorSource};
use gateway_core::health::{
    HealthProbe, HealthState, WorkerHealthKey, WorkerHealthSnapshot, WorkerHealthSource,
    WorkerRuntimeState,
};
use gateway_core::task::WorkerKind;

use crate::ApiState;

const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(crate) struct HealthStatus {
    probes: Arc<[Arc<dyn HealthProbe>]>,
    workers: Arc<dyn WorkerHealthSource>,
    diagnostics: Arc<dyn OperationalDiagnostics>,
    failures: Arc<Mutex<Vec<HealthFailure>>>,
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
#[error("{component} is {state}: {reason}")]
struct HealthFailure {
    component: String,
    state: &'static str,
    reason: String,
}

impl HealthStatus {
    #[must_use]
    pub(crate) fn new(
        probes: Vec<Arc<dyn HealthProbe>>,
        workers: Arc<dyn WorkerHealthSource>,
        diagnostics: Arc<dyn OperationalDiagnostics>,
    ) -> Self {
        Self {
            probes: probes.into(),
            workers,
            diagnostics,
            failures: Arc::default(),
        }
    }

    pub(crate) async fn healthy(&self) -> bool {
        let mut failures: Vec<_> = self
            .workers
            .snapshot()
            .into_iter()
            .filter(|worker| worker_affects_healthz(worker) && !worker_is_healthy(worker))
            .map(|worker| HealthFailure {
                component: match worker.key {
                    WorkerHealthKey::Task(id) => format!("worker:{id}"),
                    WorkerHealthKey::Disabled(kind) => format!("worker:{kind:?}"),
                },
                state: "unhealthy",
                reason: format!(
                    "state={:?}; last_error={}",
                    worker.state,
                    worker.last_error.as_deref().unwrap_or("none")
                ),
            })
            .collect();
        failures.extend(probe_failures(self.probes.iter()).await);
        let healthy = failures.is_empty();
        let changed = {
            let mut previous = self
                .failures
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let changed = failures
                .iter()
                .filter(|failure| !previous.contains(failure))
                .cloned()
                .collect::<Vec<_>>();
            for recovered in previous
                .iter()
                .filter(|old| !failures.iter().any(|new| new.component == old.component))
            {
                tracing::info!(component = %recovered.component, "health component recovered");
            }
            *previous = failures;
            changed
        };
        // 只记录故障变化；使用既有有界观测队列，文件日志失效时仍有独立的运维证据
        for failure in changed {
            tracing::warn!(component = %failure.component, state = failure.state, "health check failed");
            let mut event = OperationalFailure::new(
                "health",
                "check",
                "health_check_failed",
                "Health check failed",
            );
            event.correlation_id = Some(failure.component.clone());
            event.details =
                ErrorDetails::capture(Some(&ErrorSource::new(failure.clone())), None, false);
            if self.diagnostics.record_failure(event).await.is_err() {
                tracing::warn!("health diagnostic could not be recorded");
                // 入队失败不能抑制下次探测的记录机会，也不改变本次健康判定
                self.failures
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .retain(|previous| previous != &failure);
            }
        }
        healthy
    }
}

async fn probe_failures<'a>(
    probes: impl Iterator<Item = &'a Arc<dyn HealthProbe>>,
) -> Vec<HealthFailure> {
    // 探针并行共用截止时间，保留已完成结果并指出具体超时组件
    let deadline = tokio::time::Instant::now() + HEALTH_CHECK_TIMEOUT;
    let checks = probes.map(|probe| async move {
        let (state, reason) = match tokio::time::timeout_at(deadline, probe.check()).await {
            Ok(HealthState::Healthy) => return None,
            Ok(HealthState::Degraded(reason)) => ("degraded", reason),
            Ok(HealthState::Unhealthy(reason)) => ("unhealthy", reason),
            Err(_) => ("timed_out", "health probe exceeded 2s deadline".to_owned()),
        };
        Some(HealthFailure {
            component: format!("probe:{}", probe.name()),
            state,
            reason,
        })
    });
    join_all(checks).await.into_iter().flatten().collect()
}

pub(crate) async fn readyz(State(state): State<ApiState>) -> StatusCode {
    let required = state
        .health()
        .probes
        .iter()
        .filter(|probe| probe.required_for_readiness());
    status_code(probe_failures(required).await.is_empty())
}

pub(crate) async fn healthz(State(state): State<ApiState>) -> StatusCode {
    status_code(state.health().healthy().await)
}

fn status_code(healthy: bool) -> StatusCode {
    if healthy {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

fn worker_affects_healthz(worker: &WorkerHealthSnapshot) -> bool {
    let WorkerHealthKey::Task(id) = &worker.key else {
        return false;
    };
    !matches!(
        id.kind(),
        WorkerKind::OAuthRefresh | WorkerKind::QuotaCatalogHealth
    )
}

fn worker_is_healthy(worker: &WorkerHealthSnapshot) -> bool {
    match worker.state {
        WorkerRuntimeState::Disabled | WorkerRuntimeState::Standby => true,
        WorkerRuntimeState::Running => worker.consecutive_failures == 0,
        WorkerRuntimeState::AcquiringLease | WorkerRuntimeState::Idle => {
            worker.consecutive_failures == 0 && worker.last_success_at.is_some()
        }
        WorkerRuntimeState::Starting
        | WorkerRuntimeState::BackingOff
        | WorkerRuntimeState::Stopped => false,
    }
}
