//! Worker 贡献、调度定义与健康探针。

use super::*;

pub(crate) fn store_worker_contributions(
    execution: Arc<postgres::PgExecutionStore>,
    execution_writer: postgres::ExecutionObservationWriter<postgres::PgExecutionStore>,
    admission_release_writer: redis::ClientAdmissionReleaseWriter,
    circuit_feedback_writer: redis::ProviderCircuitFeedbackWriter,
    retention: Arc<postgres::PgRetentionRepository>,
) -> StoreResult<Vec<WorkerContribution>> {
    let stale_id = WorkerId::try_new(WorkerKind::StaleModelRequestRecovery, "postgres")
        .map_err(worker_definition_error)?;
    let retention_id =
        WorkerId::try_new(WorkerKind::Retention, "postgres").map_err(worker_definition_error)?;
    let ops_flush_id =
        WorkerId::try_new(WorkerKind::OpsFlush, "postgres").map_err(worker_definition_error)?;
    let admission_flush_id = WorkerId::try_new(WorkerKind::OpsFlush, "redis_admission")
        .map_err(worker_definition_error)?;
    let circuit_flush_id = WorkerId::try_new(WorkerKind::OpsFlush, "redis_circuit")
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
        WorkerContribution::Registration(scheduled_worker(
            retention_id,
            Duration::from_secs(60 * 60),
            Box::new(RetentionTask { retention }),
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
                admission_flush_id,
                WorkerRunnable::Daemon {
                    restart: ops_flush_restart,
                    task: Box::new(admission_release_writer),
                },
            )
            .map_err(worker_definition_error)?,
        ),
        WorkerContribution::Registration(
            WorkerRegistration::try_new(
                circuit_flush_id,
                WorkerRunnable::Daemon {
                    restart: ops_flush_restart,
                    task: Box::new(circuit_feedback_writer),
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

pub(crate) struct RetentionTask {
    retention: Arc<postgres::PgRetentionRepository>,
}

impl ScheduledTask for RetentionTask {
    fn run_cycle(
        &self,
        _context: WorkerCycleContext,
    ) -> futures::future::BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let settings =
                postgres::RetentionRepository::load_retention_settings(self.retention.as_ref())
                    .await
                    .map_err(|_| WorkerTaskError::safe("retention settings read failed"))?;
            postgres::RetentionRepository::apply_retention(
                self.retention.as_ref(),
                chrono::Utc::now(),
                settings,
            )
            .await
            .map(|_| ())
            .map_err(|_| WorkerTaskError::safe("retention cleanup failed"))
        })
    }
}

pub(crate) struct PostgresHealthProbe {
    pub(crate) pool: sqlx::PgPool,
}

impl HealthProbe for PostgresHealthProbe {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn check(&self) -> futures::future::BoxFuture<'_, HealthState> {
        Box::pin(async move {
            match sqlx::query_scalar::<_, i32>("select 1")
                .fetch_one(&self.pool)
                .await
            {
                Ok(1) => HealthState::Healthy,
                Ok(_) => HealthState::Unhealthy("PostgreSQL health result is invalid".to_owned()),
                Err(_) => HealthState::Unhealthy("PostgreSQL is unavailable".to_owned()),
            }
        })
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
