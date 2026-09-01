//! 网关进程与操作系统能力：配置发现、日志、任务、自更新与 serve/drain。

pub mod client_distribution;
pub mod config;
mod logging;
pub mod serve;
pub mod system_update;
pub mod workers;

use std::sync::Arc;

use axum::Router;
use gateway_admin::ports::{
    client_distribution::ClientDistributionResolver, system::SystemOperations,
};
use gateway_core::health::WorkerHealthSource;
use gateway_core::lifecycle::CancellationToken;
use gateway_core::lifecycle::ConnectionLifecycle;
use gateway_core::task::{WorkerContribution, WorkerLeaderLeasePort};

pub use config::{ConfigError, HostConfig, LoadableConfig, load_config};

use self::client_distribution::RgAdguardClientDistribution;
use self::logging::{LogGuard, initialize_logging};
use self::serve::{ConnectionTracker, serve_router};
use self::system_update::ProcessSystemOperations;
use self::workers::WorkerSupervisor;

/// Host 初始化的能力集；字段全部私有，不暴露内部监督器或进程状态。
pub struct HostBundle {
    config: HostConfig,
    _log_guard: LogGuard,
    cancellation: CancellationToken,
    connections: Arc<ConnectionTracker>,
    workers: WorkerSupervisor,
    system: Arc<ProcessSystemOperations>,
    client_distribution: Arc<RgAdguardClientDistribution>,
}

/// 在启动其他包之前初始化进程级能力。
pub async fn initialize(config: HostConfig) -> Result<HostBundle, HostError> {
    let log_guard = initialize_logging(&config.logging)?;
    let cancellation = CancellationToken::new();
    let connections = Arc::new(ConnectionTracker::new(cancellation.clone()));
    let workers = WorkerSupervisor::new(cancellation.clone());
    let system = Arc::new(ProcessSystemOperations::new(
        cancellation.clone(),
        config.system_update.clone(),
    ));
    let client_distribution = Arc::new(RgAdguardClientDistribution::new());
    Ok(HostBundle {
        config,
        _log_guard: log_guard,
        cancellation,
        connections,
        workers,
        system,
        client_distribution,
    })
}

impl HostBundle {
    /// 向启动控制台报告一个组装阶段已就绪。
    pub fn report_startup_ready(&self, service: &'static str) {
        tracing::info!(target: "gateway_startup", service, "服务启动正常");
    }

    #[must_use]
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    #[must_use]
    pub fn system_operations(&self) -> Arc<dyn SystemOperations> {
        self.system.clone()
    }

    #[must_use]
    /// 返回惰性下载解析能力；网络请求只会在管理 API 调用时发生。
    pub fn client_distribution_resolver(&self) -> Arc<dyn ClientDistributionResolver> {
        self.client_distribution.clone()
    }

    #[must_use]
    pub fn connection_lifecycle(&self) -> Arc<dyn ConnectionLifecycle> {
        self.connections.clone()
    }

    #[must_use]
    pub fn worker_health(&self) -> Arc<dyn WorkerHealthSource> {
        self.workers.health_source()
    }

    pub fn start_workers(
        &self,
        plan: Vec<WorkerContribution>,
        lease: Arc<dyn WorkerLeaderLeasePort>,
    ) -> Result<(), HostError> {
        self.workers.start(plan, lease)?;
        Ok(())
    }

    /// 进程唯一阻塞点；返回前完成 HTTP drain 与 worker join。
    pub async fn serve(self, router: Router) -> Result<(), HostError> {
        let result = serve_router(
            router,
            &self.config.listen.host,
            self.config.listen.port,
            self.cancellation.clone(),
            Arc::clone(&self.connections),
            self.config.drain_timeout(),
        )
        .await;
        self.cancellation.cancel();
        self.workers
            .shutdown(self.config.worker_shutdown_timeout())
            .await;
        result?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error(transparent)]
    Logging(#[from] logging::LogError),
    #[error(transparent)]
    Workers(#[from] workers::WorkerStartError),
    #[error(transparent)]
    Serve(#[from] serve::ServeError),
}
