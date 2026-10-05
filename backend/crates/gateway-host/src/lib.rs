//! 网关进程与操作系统能力：配置发现、日志、任务、自更新与 serve/drain

pub mod client_distribution;
mod command_line;
pub mod config;
mod logging;
pub mod official_plugins;
pub mod outbound;
pub mod plugin_distribution;
pub mod pricing;
pub mod process;
pub mod proxy_probe;
pub mod retention;
pub mod serve;
pub mod system_update;
pub mod workers;

use std::sync::Arc;

use axum::Router;
use gateway_admin::ports::{
    client_distribution::ClientDistributionResolver, plugin_release::OfficialPluginReleaseFiles,
    system::SystemOperations,
};
use gateway_core::health::{HealthProbe, WorkerHealthSource};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::lifecycle::ConnectionLifecycle;
use gateway_core::task::{WorkerContribution, WorkerLeaderLeasePort};

pub use config::{ConfigError, HostConfig, LoadableConfig, load_config};

use self::client_distribution::RgAdguardClientDistribution;
use self::logging::{LogGuard, initialize_logging};
use self::serve::{ConnectionTracker, serve_router};
use self::system_update::ProcessSystemOperations;
use self::workers::WorkerSupervisor;

/// Host 初始化的能力集；字段全部私有，不暴露内部监督器或进程状态
pub struct HostBundle {
    config: HostConfig,
    log_guard: LogGuard,
    cancellation: CancellationToken,
    connections: Arc<ConnectionTracker>,
    workers: WorkerSupervisor,
    system: Arc<ProcessSystemOperations>,
    client_distribution: Arc<RgAdguardClientDistribution>,
    official_plugins: Arc<official_plugins::FileOfficialPluginRelease>,
    command_signal: Option<command_line::SignalGuard>,
}

/// 在启动其他包之前初始化进程级能力
pub async fn initialize(config: HostConfig) -> Result<HostBundle, HostError> {
    let official_plugins = Arc::new(official_plugins::FileOfficialPluginRelease::new(
        config
            .system_update
            .official_plugins_dir()
            .map_err(|_| HostError::OfficialPluginRelease)?,
    ));
    let log_guard = initialize_logging(&config.logging, config.timezone)?;
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
        log_guard,
        cancellation,
        connections,
        workers,
        system,
        client_distribution,
        official_plugins,
        command_signal: None,
    })
}

/// CLI 的 stdout/stderr 属于命令结果；宿主诊断只保留已配置的文件日志
pub async fn initialize_command_line(mut config: HostConfig) -> Result<HostBundle, HostError> {
    config.logging.stdout = false;
    let mut host = initialize(config).await?;
    host.command_signal = Some(command_line::SignalGuard::start(host.cancellation()));
    Ok(host)
}

impl HostBundle {
    /// 向启动控制台报告一个组装阶段已就绪
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
    /// 返回惰性下载解析能力；网络请求只会在管理 API 调用时发生
    pub fn client_distribution_resolver(&self) -> Arc<dyn ClientDistributionResolver> {
        self.client_distribution.clone()
    }

    /// 返回与当前可执行文件一同部署的只读官方插件发行目录
    #[must_use]
    pub fn official_plugin_release_files(&self) -> Arc<dyn OfficialPluginReleaseFiles> {
        self.official_plugins.clone()
    }

    /// 身份来自构建脚本写入二进制的常量，不采信运行时配置或环境覆盖
    #[must_use]
    pub fn official_plugin_release_identity(
        &self,
    ) -> gateway_admin::model::plugins::official::OfficialPluginReleaseIdentity {
        gateway_admin::model::plugins::official::OfficialPluginReleaseIdentity {
            gateway_version: env!("CPR_VERSION").to_owned(),
            gateway_git_sha: env!("CPR_GIT_SHA").to_owned(),
        }
    }

    #[must_use]
    pub fn proxy_probe<E>(
        &self,
        build_client: impl Fn(reqwest::ClientBuilder) -> Result<reqwest::Client, E>
        + Send
        + Sync
        + 'static,
    ) -> Arc<dyn gateway_admin::ports::proxy::ProxyProbe> {
        Arc::new(proxy_probe::HttpProxyProbe::default().with_client_builder(build_client))
    }

    #[must_use]
    pub fn connection_lifecycle(&self) -> Arc<dyn ConnectionLifecycle> {
        self.connections.clone()
    }

    /// 报告文件日志写入或归档失败，避免将缺失日志误报为完整
    #[must_use]
    pub fn logging_health_probe(&self) -> Arc<dyn HealthProbe> {
        self.log_guard.health_probe()
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

    /// 进程唯一阻塞点；返回前完成 HTTP drain 与 worker join
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
        match &result {
            Ok(()) => {
                tracing::info!(target: "gateway_shutdown", pid = std::process::id(), "HTTP 服务与后台任务已停止")
            }
            Err(error) => {
                tracing::error!(target: "gateway_shutdown", pid = std::process::id(), %error, "HTTP 服务异常停止，后台任务已清理")
            }
        }
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
    #[error("官方插件发行目录不可用")]
    OfficialPluginRelease,
}
