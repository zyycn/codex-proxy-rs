//! Worker 贡献、调度定义与健康探针

use super::*;
use gateway_core::task::DaemonTask;

const COMMAND_DRAIN_TIMEOUT: Duration = Duration::from_secs(4);

pub(crate) struct CommandStoreWriters {
    pub(crate) execution: postgres::ExecutionObservationWriter<postgres::PgExecutionStore>,
    pub(crate) client_key_usage: postgres::PgClientApiKeyUsageWriter,
    pub(crate) admission_release: redis::ClientAdmissionReleaseWriter,
}

/// 短生命周期 CLI 只运行数据面必需的三个写泵，不注册恢复、保留或维护 Worker
pub struct CommandStoreDrain {
    cancellation: gateway_core::lifecycle::CancellationToken,
    tasks: Vec<tokio::task::JoinHandle<Result<(), WorkerTaskError>>>,
    execution_idle: postgres::ExecutionBufferIdle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("command store writes could not be drained")]
pub struct CommandStoreDrainError;

impl CommandStoreWriters {
    pub(crate) fn start(self) -> CommandStoreDrain {
        let cancellation = gateway_core::lifecycle::CancellationToken::new();
        let Self {
            execution,
            client_key_usage,
            admission_release,
        } = self;
        let execution_idle = execution.idle();
        let tasks = vec![
            spawn_command_writer(execution, cancellation.child_token()),
            spawn_command_writer(client_key_usage, cancellation.child_token()),
            spawn_command_writer(admission_release, cancellation.child_token()),
        ];
        CommandStoreDrain {
            cancellation,
            tasks,
            execution_idle,
        }
    }
}

fn spawn_command_writer<T>(
    writer: T,
    cancellation: gateway_core::lifecycle::CancellationToken,
) -> tokio::task::JoinHandle<Result<(), WorkerTaskError>>
where
    T: DaemonTask + Send + 'static,
{
    tokio::spawn(async move { writer.run(cancellation).await })
}

impl CommandStoreDrain {
    pub(crate) async fn shutdown(mut self) -> Result<(), CommandStoreDrainError> {
        let deadline = std::time::Instant::now() + COMMAND_DRAIN_TIMEOUT;
        // CLI 调用方已结束数据面会话；先让已接收的 execution 写入在正常 writer
        // 路径完成，避免立即取消后落入更短的常驻进程关闭丢弃窗口
        let mut failed = !self.execution_idle.wait_until(deadline).await;
        self.cancellation.cancel();
        for mut task in self.tasks.drain(..) {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                task.abort();
                failed = true;
                continue;
            }
            match tokio::time::timeout(remaining, &mut task).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(_))) | Ok(Err(_)) => failed = true,
                Err(_) => {
                    task.abort();
                    failed = true;
                }
            }
        }
        if failed {
            Err(CommandStoreDrainError)
        } else {
            Ok(())
        }
    }
}

impl Drop for CommandStoreDrain {
    fn drop(&mut self) {
        self.cancellation.cancel();
        for task in &self.tasks {
            task.abort();
        }
    }
}

pub(crate) fn store_worker_contributions(
    execution: Arc<postgres::PgExecutionStore>,
    execution_writer: postgres::ExecutionObservationWriter<postgres::PgExecutionStore>,
    client_key_usage_writer: postgres::PgClientApiKeyUsageWriter,
    admission_release_writer: redis::ClientAdmissionReleaseWriter,
) -> StoreResult<Vec<WorkerContribution>> {
    let stale_id = WorkerId::try_new(WorkerKind::StaleModelRequestRecovery, "postgres")
        .map_err(worker_definition_error)?;
    let ops_flush_id =
        WorkerId::try_new(WorkerKind::OpsFlush, "postgres").map_err(worker_definition_error)?;
    let client_key_usage_flush_id =
        WorkerId::try_new(WorkerKind::OpsFlush, "postgres_client_key_usage")
            .map_err(worker_definition_error)?;
    let admission_flush_id = WorkerId::try_new(WorkerKind::OpsFlush, "redis_admission")
        .map_err(worker_definition_error)?;
    let ops_flush_restart =
        DaemonRestartPolicy::try_new(Duration::from_secs(1), Duration::from_secs(60))
            .map_err(worker_definition_error)?;
    Ok(vec![
        WorkerContribution::Registration(scheduled_worker(
            stale_id,
            Duration::from_secs(30),
            Box::new(StaleModelRequestRecoveryTask { execution }),
        )?),
        WorkerContribution::Registration(
            WorkerRegistration::try_new(
                ops_flush_id,
                WorkerRunnable::Daemon {
                    restart: ops_flush_restart,
                    task: Box::new(execution_writer),
                },
            )
            .map_err(worker_definition_error)?,
        ),
        WorkerContribution::Registration(
            WorkerRegistration::try_new(
                client_key_usage_flush_id,
                WorkerRunnable::Daemon {
                    restart: ops_flush_restart,
                    task: Box::new(client_key_usage_writer),
                },
            )
            .map_err(worker_definition_error)?,
        ),
        WorkerContribution::Registration(
            WorkerRegistration::try_new(
                admission_flush_id,
                WorkerRunnable::Daemon {
                    restart: ops_flush_restart,
                    task: Box::new(admission_release_writer),
                },
            )
            .map_err(worker_definition_error)?,
        ),
    ])
}

pub(crate) fn scheduled_worker(
    id: WorkerId,
    interval: Duration,
    task: Box<dyn ScheduledTask>,
) -> StoreResult<WorkerRegistration> {
    let schedule = WorkerSchedule::try_new(
        interval,
        Duration::from_secs(1),
        Duration::from_secs(60),
        Duration::from_secs(15 * 60),
        Duration::from_secs(5 * 60),
    )
    .map_err(worker_definition_error)?;
    let lease = WorkerLeaseRequest::try_new(id.clone(), schedule.leader_lease_ttl())
        .map_err(worker_definition_error)?;
    WorkerRegistration::try_new(
        id,
        WorkerRunnable::Scheduled {
            schedule,
            lease: Some(lease),
            task,
        },
    )
    .map_err(worker_definition_error)
}

pub(crate) fn worker_definition_error(
    error: gateway_core::task::WorkerDefinitionError,
) -> StoreError {
    StoreError::InvalidData {
        entity: "store worker plan",
        message: error.to_string(),
    }
}

pub(crate) struct StaleModelRequestRecoveryTask {
    execution: Arc<postgres::PgExecutionStore>,
}

impl ScheduledTask for StaleModelRequestRecoveryTask {
    fn run_cycle(
        &self,
        _context: WorkerCycleContext,
    ) -> futures::future::BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            gateway_core::engine::ExecutionStore::recover_expired(
                self.execution.as_ref(),
                SystemTime::now(),
            )
            .await
            .map(|_| ())
            .map_err(|_| WorkerTaskError::safe("stale request recovery failed"))
        })
    }
}

pub struct PostgresHealthProbe {
    pool: sqlx::PgPool,
    max_connections: u32,
}

impl PostgresHealthProbe {
    #[must_use]
    pub const fn new(pool: sqlx::PgPool, max_connections: u32) -> Self {
        Self {
            pool,
            max_connections,
        }
    }

    async fn check_once(&self) -> HealthState {
        let deadline = tokio::time::Instant::now() + POSTGRES_HEALTH_ATTEMPT_TIMEOUT;
        let mut connection = match tokio::time::timeout_at(deadline, self.pool.acquire()).await {
            Ok(Ok(connection)) => connection,
            Ok(Err(_)) => {
                return HealthState::Unhealthy("PostgreSQL is unavailable".to_owned());
            }
            Err(_) => {
                return postgres_acquire_timeout_state(
                    self.pool.size(),
                    self.pool.num_idle(),
                    self.max_connections,
                );
            }
        };
        match tokio::time::timeout_at(
            deadline,
            sqlx::query_scalar::<_, i32>("select 1").fetch_one(&mut *connection),
        )
        .await
        {
            Ok(Ok(1)) => HealthState::Healthy,
            Ok(Ok(_)) => HealthState::Unhealthy("PostgreSQL health result is invalid".to_owned()),
            Ok(Err(_)) => HealthState::Unhealthy("PostgreSQL is unavailable".to_owned()),
            Err(_) => HealthState::Unhealthy("PostgreSQL health query timed out".to_owned()),
        }
    }
}

impl HealthProbe for PostgresHealthProbe {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn check(&self) -> futures::future::BoxFuture<'_, HealthState> {
        Box::pin(health_state_with_one_retry(
            || self.check_once(),
            POSTGRES_HEALTH_RETRY_DELAY,
        ))
    }
}

async fn health_state_with_one_retry<F, Fut>(mut check: F, retry_delay: Duration) -> HealthState
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = HealthState>,
{
    let first = check().await;
    if first == HealthState::Healthy {
        return first;
    }
    tokio::time::sleep(retry_delay).await;
    check().await
}

fn postgres_acquire_timeout_state(
    pool_size: u32,
    idle_connections: usize,
    max_connections: u32,
) -> HealthState {
    if pool_size >= max_connections && idle_connections == 0 {
        HealthState::Degraded(format!(
            "PostgreSQL pool is saturated ({pool_size}/{max_connections} connections in use)"
        ))
    } else {
        HealthState::Unhealthy("PostgreSQL connection acquisition timed out".to_owned())
    }
}

pub(crate) struct RedisHealthProbe {
    pub(crate) connection: ::redis::aio::ConnectionManager,
}
impl HealthProbe for RedisHealthProbe {
    fn name(&self) -> &'static str {
        "redis"
    }

    fn check(&self) -> futures::future::BoxFuture<'_, HealthState> {
        Box::pin(async move {
            let mut connection = self.connection.clone();
            match ::redis::cmd("PING")
                .query_async::<String>(&mut connection)
                .await
            {
                Ok(response) if response == "PONG" => HealthState::Healthy,
                Ok(_) => HealthState::Unhealthy("Redis health result is invalid".to_owned()),
                Err(_) => HealthState::Unhealthy("Redis is unavailable".to_owned()),
            }
        })
    }
}
