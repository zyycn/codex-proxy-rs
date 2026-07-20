//! `/healthz` 对 Core、Store 与 Host worker 健康事实的聚合。

use std::sync::Arc;
use std::time::Duration;

use axum::{extract::State, http::StatusCode};
use futures::future::join_all;
use gateway_core::health::{
    HealthProbe, HealthState, WorkerHealthSnapshot, WorkerHealthSource, WorkerRuntimeState,
};

use crate::ApiState;

const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(crate) struct HealthStatus {
    probes: Arc<[Arc<dyn HealthProbe>]>,
    workers: Arc<dyn WorkerHealthSource>,
}

impl HealthStatus {
    #[must_use]
    pub(crate) fn new(
        probes: Vec<Arc<dyn HealthProbe>>,
        workers: Arc<dyn WorkerHealthSource>,
    ) -> Self {
        Self {
            probes: probes.into(),
            workers,
        }
    }

    pub(crate) async fn healthy(&self) -> bool {
        if !self.workers.snapshot().iter().all(worker_is_healthy) {
            return false;
        }
        let checks = self.probes.iter().map(|probe| probe.check());
        tokio::time::timeout(HEALTH_CHECK_TIMEOUT, join_all(checks))
            .await
            .is_ok_and(|states| {
                states
                    .into_iter()
                    .all(|state| matches!(state, HealthState::Healthy))
            })
    }
}

pub(crate) async fn healthz(State(state): State<ApiState>) -> StatusCode {
    if state.health().healthy().await {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
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
