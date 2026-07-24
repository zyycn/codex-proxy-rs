//! 多平台 AI 网关的数据面核心。
//!
//! 本 crate 只描述协议与 Provider 无关的业务语义。HTTP、数据库、Redis、
//! 具体客户端协议和具体 Provider 都通过外层 adapter 接入。

pub mod accounting;
pub mod engine;
pub mod error;
pub mod event;
pub mod health;
pub mod lifecycle;
pub mod operation;
pub mod policy;
pub mod provider_ports;
pub mod routing;
pub mod task;

use std::sync::Arc;
use std::time::SystemTime;

use engine::ExecutionStore;
use engine::admission::{
    ClientAdmissionPort, ClientAdmissionRecoveryPort, restore_client_admission_startup,
};
use engine::continuation::NativeContinuationPort;
use engine::execution::{
    ClientApiKeyUsageSink, DefaultExecutionService, ExecutionService, ProviderCircuitPort,
};
use engine::probe::AccountProbe;
use engine::provider::ProviderRegistry;
use health::HealthProbe;
use routing::snapshot::{
    RuntimeSnapshotCompiler, RuntimeSnapshotHandle, RuntimeSnapshotPublisher, SnapshotControl,
    SnapshotStorePort, SnapshotSubscriptionPort,
};
use task::{WorkerContribution, WorkerDefinitionError};

/// Store 提供给数据面 Core 的封闭能力集合。
#[derive(Clone)]
pub struct CoreStorePorts {
    execution: Arc<dyn ExecutionStore>,
    admissions: Arc<dyn ClientAdmissionPort>,
    admission_recovery: Arc<dyn ClientAdmissionRecoveryPort>,
    circuits: Arc<dyn ProviderCircuitPort>,
    continuation: Arc<dyn NativeContinuationPort>,
    snapshots: Arc<dyn SnapshotStorePort>,
    snapshot_subscriptions: Arc<dyn SnapshotSubscriptionPort>,
    client_api_key_usage: Arc<dyn ClientApiKeyUsageSink>,
}

impl CoreStorePorts {
    #[must_use]
    pub fn new(
        execution: Arc<dyn ExecutionStore>,
        (admissions, admission_recovery): (
            Arc<dyn ClientAdmissionPort>,
            Arc<dyn ClientAdmissionRecoveryPort>,
        ),
        circuits: Arc<dyn ProviderCircuitPort>,
        continuation: Arc<dyn NativeContinuationPort>,
        (snapshots, snapshot_subscriptions): (
            Arc<dyn SnapshotStorePort>,
            Arc<dyn SnapshotSubscriptionPort>,
        ),
        client_api_key_usage: Arc<dyn ClientApiKeyUsageSink>,
    ) -> Self {
        Self {
            execution,
            admissions,
            admission_recovery,
            circuits,
            continuation,
            snapshots,
            snapshot_subscriptions,
            client_api_key_usage,
        }
    }
}

pub struct CoreBundle {
    execution: Arc<dyn ExecutionService>,
    snapshot_control: Arc<dyn SnapshotControl>,
    account_probe: Arc<dyn AccountProbe>,
    health_probes: Vec<Arc<dyn HealthProbe>>,
    worker_contributions: Vec<WorkerContribution>,
}

impl CoreBundle {
    #[must_use]
    pub fn execution_service(&self) -> Arc<dyn ExecutionService> {
        Arc::clone(&self.execution)
    }

    #[must_use]
    pub fn snapshot_control(&self) -> Arc<dyn SnapshotControl> {
        Arc::clone(&self.snapshot_control)
    }

    #[must_use]
    pub fn account_probe(&self) -> Arc<dyn AccountProbe> {
        Arc::clone(&self.account_probe)
    }

    #[must_use]
    pub fn health_probes(&self) -> Vec<Arc<dyn HealthProbe>> {
        self.health_probes.clone()
    }

    pub fn take_worker_contributions(&mut self) -> Vec<WorkerContribution> {
        std::mem::take(&mut self.worker_contributions)
    }
}

/// 首个快照与准入恢复均为监听前 fail-closed 屏障。
pub async fn initialize(
    ports: CoreStorePorts,
    providers: ProviderRegistry,
) -> Result<CoreBundle, CoreError> {
    restore_client_admission_startup(
        ports.execution.as_ref(),
        ports.admission_recovery.as_ref(),
        ports.admissions.as_ref(),
        SystemTime::now(),
    )
    .await
    .map_err(|_| CoreError::AdmissionRecoveryUnavailable)?;
    let compiler = Arc::new(RuntimeSnapshotCompiler::new(
        Arc::clone(&ports.snapshots),
        providers.clone(),
    ));
    let initial = compiler
        .compile()
        .await
        .map_err(|_| CoreError::SnapshotUnavailable)?;
    let snapshots = RuntimeSnapshotHandle::new(initial);
    let publisher = Arc::new(RuntimeSnapshotPublisher::new(
        compiler,
        snapshots.clone(),
        Arc::clone(&ports.snapshot_subscriptions),
    ));
    let worker_contributions = publisher.worker_contributions()?;
    let service = Arc::new(DefaultExecutionService::new(
        snapshots.clone(),
        ports.execution,
        providers,
        ports.admissions,
        ports.circuits,
        ports.continuation,
        ports.client_api_key_usage,
    ));
    let execution: Arc<dyn ExecutionService> = service.clone();
    let account_probe: Arc<dyn AccountProbe> = service;
    let snapshot_control: Arc<dyn SnapshotControl> = publisher;
    let health_probes: Vec<Arc<dyn HealthProbe>> = vec![Arc::new(snapshots)];
    Ok(CoreBundle {
        execution,
        snapshot_control,
        account_probe,
        health_probes,
        worker_contributions,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("initial runtime snapshot is unavailable")]
    SnapshotUnavailable,
    #[error("client admission startup recovery is unavailable")]
    AdmissionRecoveryUnavailable,
    #[error(transparent)]
    WorkerDefinition(#[from] WorkerDefinitionError),
}
