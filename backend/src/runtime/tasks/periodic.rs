//! 周期性后台任务运行器。

use std::{future::Future, pin::Pin, time::Duration};

use tokio::sync::mpsc;
use tokio::time::interval;
use tracing::info;

use super::coordinator::SchedulerHandle;

pub(crate) type TaskFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 周期任务进入循环前的决策。
pub(crate) enum PeriodicTaskStartup {
    /// 继续进入周期循环。
    Continue,
    /// 任务已自行结束，不再进入周期循环。
    Stop,
}

/// 周期任务调度配置。
pub(crate) struct PeriodicTaskConfig {
    interval_secs: u64,
    started_message: &'static str,
    stopped_message: &'static str,
    run_immediately: bool,
}

impl PeriodicTaskConfig {
    pub(crate) fn new(
        interval_secs: u64,
        started_message: &'static str,
        stopped_message: &'static str,
    ) -> Self {
        Self {
            interval_secs,
            started_message,
            stopped_message,
            run_immediately: true,
        }
    }

    pub(crate) fn wait_first_interval(mut self) -> Self {
        self.run_immediately = false;
        self
    }
}

/// 周期任务行为。
pub(crate) trait PeriodicTaskRunner: Send + 'static {
    fn before_loop<'a>(
        &'a mut self,
        _shutdown_rx: &'a mut mpsc::Receiver<()>,
    ) -> TaskFuture<'a, PeriodicTaskStartup> {
        Box::pin(std::future::ready(PeriodicTaskStartup::Continue))
    }

    fn tick(&mut self) -> TaskFuture<'_, ()>;

    fn shutdown(&mut self) -> TaskFuture<'_, ()> {
        Box::pin(std::future::ready(()))
    }
}

/// 启动周期任务，并统一处理关闭信号与 JoinHandle。
pub(crate) fn spawn_periodic_task<T>(mut task: T, config: PeriodicTaskConfig) -> SchedulerHandle
where
    T: PeriodicTaskRunner,
{
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);

    let handle = tokio::spawn(async move {
        info!(
            interval_secs = config.interval_secs,
            "{}", config.started_message
        );

        if matches!(
            task.before_loop(&mut shutdown_rx).await,
            PeriodicTaskStartup::Stop
        ) {
            return;
        }

        let mut ticker = interval(Duration::from_secs(config.interval_secs));
        if !config.run_immediately {
            ticker.tick().await;
        }

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    task.tick().await;
                }
                _ = shutdown_rx.recv() => {
                    task.shutdown().await;
                    info!("{}", config.stopped_message);
                    break;
                }
            }
        }
    });

    SchedulerHandle::new(shutdown_tx, handle)
}
