//! 周期性后台任务运行器。

use std::{future::Future, pin::Pin, time::Duration};

use tokio::sync::mpsc;
use tokio::time::interval;
use tracing::info;

use super::coordinator::SchedulerHandle;

pub(crate) type TaskFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 周期任务调度配置。
pub(crate) struct PeriodicTaskConfig {
    interval_secs: u64,
    started_message: &'static str,
    stopped_message: &'static str,
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
        }
    }
}

/// 周期任务行为。
pub(crate) trait PeriodicTaskRunner: Send + 'static {
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

        let mut ticker = interval(Duration::from_secs(config.interval_secs));

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
