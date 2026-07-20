//! Store、Core 与 Host 向 HTTP 健康探针暴露的中立契约。

use std::time::SystemTime;

use futures::future::BoxFuture;

use crate::task::{WorkerFencingToken, WorkerId, WorkerKind};

/// 一个健康探针的语义状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthState {
    Healthy,
    Degraded(String),
    Unhealthy(String),
}

/// 可被 API 异质聚合的异步健康探针。
pub trait HealthProbe: Send + Sync {
    fn name(&self) -> &'static str;

    fn check(&self) -> BoxFuture<'_, HealthState>;
}

/// Host 监督器对一个 worker 观测到的当前状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerRuntimeState {
    Starting,
    AcquiringLease,
    Standby,
    Running,
    Idle,
    BackingOff,
    Stopped,
    Disabled,
}

/// worker 健康快照的稳定 key。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WorkerHealthKey {
    Task(WorkerId),
    Disabled(WorkerKind),
}

/// 一个 worker 在某一时刻的完整可观测快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerHealthSnapshot {
    pub key: WorkerHealthKey,
    pub state: WorkerRuntimeState,
    pub consecutive_failures: u32,
    pub completed_cycles: u64,
    pub last_fencing_token: Option<WorkerFencingToken>,
    pub last_success_at: Option<SystemTime>,
    pub last_failure_at: Option<SystemTime>,
    pub last_error: Option<String>,
}

/// Host 提供、API 消费的 worker 健康快照来源。
pub trait WorkerHealthSource: Send + Sync {
    fn snapshot(&self) -> Vec<WorkerHealthSnapshot>;
}
