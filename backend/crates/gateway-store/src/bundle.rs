//! 完成连接、迁移与 hydration 的 Store 能力集合与启动屏障。

use gateway_core::engine::credential::ProviderAccountStore;
use gateway_core::provider_ports::ProviderCooldownPort;

use super::*;

/// 已完成连接、迁移与 hydration 的 Store 能力集合。
pub struct StoreBundle {
    admin_ports: AdminStorePorts,
    core_ports: CoreStorePorts,
    provider_ports: ProviderStorePorts,
    worker_leader_lease: Arc<dyn WorkerLeaderLeasePort>,
    health_probes: Vec<Arc<dyn HealthProbe>>,
    worker_contributions: Vec<WorkerContribution>,
}

impl StoreBundle {
    #[must_use]
    pub fn admin_ports(&self) -> AdminStorePorts {
        self.admin_ports.clone()
    }

    #[must_use]
    pub fn core_ports(&self) -> CoreStorePorts {
        self.core_ports.clone()
    }

    #[must_use]
    pub fn provider_ports(&self) -> ProviderStorePorts {
        self.provider_ports.clone()
    }

    #[must_use]
    pub fn worker_leader_lease(&self) -> Arc<dyn WorkerLeaderLeasePort> {
        Arc::clone(&self.worker_leader_lease)
    }

    #[must_use]
    pub fn health_probes(&self) -> Vec<Arc<dyn HealthProbe>> {
        self.health_probes.clone()
    }

    pub fn take_worker_contributions(&mut self) -> Vec<WorkerContribution> {
        std::mem::take(&mut self.worker_contributions)
    }
}

/// 在返回 Bundle 前完成全部 Store 启动屏障。
pub async fn initialize(mut config: StoreConfig) -> StoreResult<StoreBundle> {
    const REDIS_NAMESPACE: &str = "codex-proxy-rs";

    config.resolve_and_validate(std::path::Path::new("."))?;
    let pool = postgres::connect_and_migrate(&config.database_url()?, config.pool).await?;
    let redis_client = ::redis::Client::open(config.redis_url()?)
        .map_err(|_| redis_unavailable("create Redis client"))?;
    let redis_connection = redis_client
        .get_connection_manager()
        .await
        .map_err(|_| redis_unavailable("connect Redis manager"))?;

    let provider_accounts = Arc::new(postgres::PgProviderAccountRepository::new(pool.clone()));
    let cooldowns = Arc::new(redis::RedisCredentialCooldownRepository::new(
        redis_connection.clone(),
        REDIS_NAMESPACE,
    )?);
    let account_store: Arc<dyn ProviderAccountStore> = provider_accounts;

    let credential_leases =
        redis::RedisCredentialLeaseRepository::new(redis_connection.clone(), REDIS_NAMESPACE)?;
    let provider_leases = Arc::new(redis::RedisProviderLeaseCoordinator::new(
        credential_leases.clone(),
    ));
    let provider_session_affinity = Arc::new(redis::RedisProviderSessionAffinityRepository::new(
        redis_connection.clone(),
        REDIS_NAMESPACE,
    )?);
    let credential_state = Arc::new(redis::RedisCredentialStateRepository::new(
        redis_connection.clone(),
        REDIS_NAMESPACE,
    )?);
    let runtime_policy = Arc::new(postgres::PgRuntimeSettingsRepository::new(pool.clone()));
    let oauth_pending = Arc::new(redis::RedisOAuthPendingFlowRepository::new(
        redis_connection.clone(),
        REDIS_NAMESPACE,
    )?);

    let admin_ports = AdminStorePorts::new(
        Arc::new(postgres::PgAdminAccountStore::new(
            pool.clone(),
            Some(Arc::clone(&cooldowns) as Arc<dyn ProviderCooldownPort>),
        )),
        Arc::new(postgres::PgAccountGroupRepository::with_runtime_state(
            pool.clone(),
            credential_leases.clone(),
            Arc::clone(&cooldowns) as Arc<dyn ProviderCooldownPort>,
        )),
        Arc::new(AdminAuthStoreAdapter {
            security: postgres::PgAdminSecurityAuditRepository::new(pool.clone()),
            settings: postgres::PgRuntimeSettingsRepository::new(pool.clone()),
            state: redis::RedisAdminAuthStateRepository::new(
                redis_connection.clone(),
                REDIS_NAMESPACE,
            )?,
        }),
        Arc::new(postgres::PgAdminClientKeyStore::new(pool.clone())),
        Arc::new(postgres::PgAdminObservabilityStore::new(
            pool.clone(),
            Some(credential_leases.clone()),
            Some(Arc::clone(&cooldowns) as Arc<dyn ProviderCooldownPort>),
        )),
        Arc::new(AdminSettingsStoreAdapter {
            control_plane: postgres::PgControlPlaneRepository::new(pool.clone()),
        }),
        backup_ports(pool.clone(), &config)?,
    );

    let execution_repository = Arc::new(postgres::PgExecutionStore::new(pool.clone()));
    let (execution, execution_writer) =
        postgres::BufferedExecutionStore::new(Arc::clone(&execution_repository));
    let execution = Arc::new(execution);
    let retention = Arc::new(postgres::PgRetentionRepository::new(pool.clone()));
    let admissions: Arc<dyn gateway_core::engine::admission::ClientAdmissionPort> = Arc::new(
        redis::RedisClientAdmissionRepository::new(redis_connection.clone(), REDIS_NAMESPACE)?,
    );
    let circuits: Arc<dyn gateway_core::engine::execution::ProviderCircuitPort> =
        Arc::new(redis::RedisProviderCircuitRepository::new(
            redis_connection.clone(),
            REDIS_NAMESPACE,
            gateway_core::engine::execution::ProviderCircuitPolicy::default(),
        )?);
    // Continuation affinity 是下一轮请求的路由事实，Core 必须直接等待 Redis 确认。
    let continuation: Arc<dyn gateway_core::engine::continuation::NativeContinuationPort> =
        Arc::new(redis::RedisNativeContinuationRepository::new(
            redis_connection.clone(),
            REDIS_NAMESPACE,
        )?);
    let (admissions, admission_release_writer) =
        redis::BufferedClientAdmissionPort::new(admissions);
    let (circuits, circuit_feedback_writer) = redis::BufferedProviderCircuitPort::new(circuits);
    let core_ports = CoreStorePorts::new(
        execution,
        (
            Arc::new(admissions),
            Arc::new(postgres::PgClientAdmissionRecoveryRepository::new(
                pool.clone(),
            )),
        ),
        Arc::new(circuits),
        continuation,
        (
            Arc::new(postgres::PgRuntimeSnapshotRepository::new(pool.clone())),
            Arc::new(redis::RedisRuntimeChangeRepository::new(
                redis_client,
                REDIS_NAMESPACE,
            )?),
        ),
        Arc::new(postgres::PgClientApiKeyUsageSink::new(pool.clone())),
    );

    let provider_ports = ProviderStorePorts::new(
        account_store,
        provider_leases,
        provider_session_affinity,
        Arc::new(redis::RedisProviderSessionExclusionRepository::new(
            redis_connection.clone(),
            REDIS_NAMESPACE,
        )?),
        credential_state.clone(),
        credential_state,
        cooldowns,
        runtime_policy,
        oauth_pending,
    );
    let worker_leader_lease = Arc::new(redis::worker_lease::RedisWorkerLeaderLeasePort::new(
        credential_leases,
    ));
    let health_probes: Vec<Arc<dyn HealthProbe>> = vec![
        Arc::new(PostgresHealthProbe { pool: pool.clone() }),
        Arc::new(RedisHealthProbe {
            connection: redis_connection,
        }),
    ];
    let worker_contributions = store_worker_contributions(
        execution_repository,
        execution_writer,
        admission_release_writer,
        circuit_feedback_writer,
        retention,
    )?;
    Ok(StoreBundle {
        admin_ports,
        core_ports,
        provider_ports,
        worker_leader_lease,
        health_probes,
        worker_contributions,
    })
}

/// 构造备份控制面的仓储、导出器与对象存储适配器。
pub(crate) fn backup_ports(
    pool: sqlx::PgPool,
    config: &StoreConfig,
) -> StoreResult<BackupStorePorts> {
    let staging = Arc::new(backup::staging::StagingArea::open(
        std::path::PathBuf::from(".runtime/data/backup-staging"),
        backup::staging::DEFAULT_MAX_ARCHIVE_BYTES,
    )?);
    let repository = Arc::new(postgres::PgBackupRepository::new(pool));
    let dump = Arc::new(backup::pg_dump::PgDumpAdapter::new(
        staging,
        config.database.url.clone(),
        config.database.password.clone(),
    ));
    let object_store = Arc::new(backup::s3::S3ObjectStoreAdapter::new());
    Ok(BackupStorePorts::new(repository, dump, object_store))
}
