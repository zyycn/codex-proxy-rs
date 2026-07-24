//! 多 Provider 网关的 PostgreSQL 持久化与 Redis 协调 adapter。
//!
//! 业务规则与 port 由 `gateway-core` 拥有。本 crate 只负责把终态的七张业务表
//! 和可丢失 Redis 状态映射为明确的基础设施操作。

use std::sync::Arc;
use std::time::{Duration, SystemTime};
use std::{fmt, num::NonZeroU64, str::FromStr};

use gateway_admin::model::auth::{AdminAuditEvent as AdminAuditModel, AdminSession};
use gateway_admin::model::settings::{
    AdminApiKey, AdminApiKeyMutation, ModelMappings, ReplaceRuntimeSettings,
    RotationStrategy as AdminRotationStrategy, RuntimeSettings as AdminRuntimeSettings,
};
use gateway_admin::model::{MutationActor, MutationContext, Revision as AdminRevision};
use gateway_admin::ports::store::{
    AdminStoreError, AdminStoreErrorKind, AdminStorePorts, AdminStoreResult, AuthStore,
    SettingsStore,
};
use gateway_core::CoreStorePorts;
use gateway_core::health::{HealthProbe, HealthState};
use gateway_core::provider_ports::ProviderStorePorts;
use gateway_core::task::{
    ScheduledTask, WorkerContribution, WorkerCycleContext, WorkerDisabledReason, WorkerId,
    WorkerKind, WorkerLeaderLeasePort, WorkerLeaseRequest, WorkerRegistration, WorkerRunnable,
    WorkerSchedule, WorkerTaskError,
};
use serde::Deserialize;
use serde_json::{Map, Value};

const DATABASE_URL_ENV: &str = "CPR_DATABASE_URL";
const REDIS_URL_ENV: &str = "CPR_REDIS_URL";
const DATABASE_PASSWORD_ENV: &str = "CPR_DATABASE_PASSWORD";
const REDIS_PASSWORD_ENV: &str = "CPR_REDIS_PASSWORD";

pub mod postgres;
pub mod redis;

/// 发生错误的基础设施边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreBackend {
    PostgreSql,
    Redis,
}

/// 上层状态机需要区分的稳定冲突类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    StaleRevision,
    AlreadyFinalized,
    DownstreamAlreadyCommitted,
    RequestNotRunning,
    InvalidTransition,
    LeaseLost,
    FencingTokenStale,
}

/// Store adapter 的稳定错误边界。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("{backend:?} store is unavailable: {message}")]
    Unavailable {
        backend: StoreBackend,
        message: String,
    },
    #[error("{entity} {id} was not found")]
    NotFound { entity: &'static str, id: String },
    #[error("store conflict for {entity} {id}: {kind:?}")]
    Conflict {
        entity: &'static str,
        id: String,
        kind: ConflictKind,
    },
    #[error("invalid persisted {entity}: {message}")]
    InvalidData {
        entity: &'static str,
        message: String,
    },
}

pub type StoreResult<T> = Result<T, StoreError>;

/// Store 自己拥有并校验的启动配置。
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    database: StoreConnectionConfig,
    redis: StoreConnectionConfig,
}

impl StoreConfig {
    pub fn resolve_and_validate(&mut self, _source_dir: &std::path::Path) -> StoreResult<()> {
        if let Some(url) = optional_environment_value(DATABASE_URL_ENV)? {
            self.database.url = url;
        }
        if let Some(url) = optional_environment_value(REDIS_URL_ENV)? {
            self.redis.url = url;
        }
        if let Some(password) = optional_environment_value(DATABASE_PASSWORD_ENV)? {
            self.database.password = password;
        }
        if let Some(password) = optional_environment_value(REDIS_PASSWORD_ENV)? {
            self.redis.password = password;
        }
        self.database.validate("database")?;
        self.redis.validate("redis")?;
        Ok(())
    }

    fn database_url(&self) -> StoreResult<String> {
        self.database.connection_url("database")
    }

    fn redis_url(&self) -> StoreResult<String> {
        self.redis.connection_url("redis")
    }
}

fn optional_environment_value(name: &'static str) -> StoreResult<Option<String>> {
    match std::env::var(name) {
        Ok(value) if value.trim().is_empty() => Err(StoreError::InvalidData {
            entity: "store config",
            message: format!("environment variable {name} is empty"),
        }),
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(StoreError::InvalidData {
            entity: "store config",
            message: format!("environment variable {name} is not Unicode"),
        }),
    }
}

impl fmt::Debug for StoreConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoreConfig")
            .field("database", &"[REDACTED]")
            .field("redis", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreConnectionConfig {
    url: String,
    password: String,
}

impl StoreConnectionConfig {
    fn validate(&self, field: &'static str) -> StoreResult<()> {
        require_nonempty("store config", field, &self.url)?;
        if self.password.len() != 48 || !self.password.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(StoreError::InvalidData {
                entity: "store config",
                message: format!("{field}.password must be exactly 48 hexadecimal characters"),
            });
        }
        self.connection_url(field).map(|_| ())
    }

    fn connection_url(&self, field: &'static str) -> StoreResult<String> {
        let mut url = url::Url::parse(&self.url).map_err(|_| StoreError::InvalidData {
            entity: "store config",
            message: format!("{field}.url is invalid"),
        })?;
        if url.password().is_some() {
            return Err(StoreError::InvalidData {
                entity: "store config",
                message: format!("{field}.url must not contain a password"),
            });
        }
        url.set_password(Some(&self.password))
            .map_err(|()| StoreError::InvalidData {
                entity: "store config",
                message: format!("{field}.url cannot carry credentials"),
            })?;
        Ok(url.to_string())
    }
}

/// 已完成连接、迁移与 cooldown hydration 的 Store 能力集合。
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
    let pool = postgres::connect_and_migrate(&config.database_url()?).await?;
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
    let account_store = Arc::new(redis::CooldownCachingProviderAccountStore::new(
        provider_accounts,
        cooldowns.clone(),
    ));
    let hydrated = account_store.hydrate(std::time::SystemTime::now()).await;
    tracing::info!(hydrated, "账号 cooldown Redis 热缓存重建完成");

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
        Arc::new(postgres::PgAdminAccountStore::new(pool.clone())),
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
        )),
        Arc::new(AdminSettingsStoreAdapter {
            control_plane: postgres::PgControlPlaneRepository::new(pool.clone()),
        }),
    );

    let execution = Arc::new(postgres::PgExecutionStore::new(pool.clone()));
    let retention = Arc::new(postgres::PgRetentionRepository::new(pool.clone()));
    let core_ports = CoreStorePorts::new(
        execution.clone(),
        (
            Arc::new(redis::RedisClientAdmissionRepository::new(
                redis_connection.clone(),
                REDIS_NAMESPACE,
            )?),
            Arc::new(postgres::PgClientAdmissionRecoveryRepository::new(
                pool.clone(),
            )),
        ),
        Arc::new(redis::RedisProviderCircuitRepository::new(
            redis_connection.clone(),
            REDIS_NAMESPACE,
            redis::ProviderCircuitPolicy::default(),
        )?),
        Arc::new(postgres::PgHistoryRepository::new(pool.clone())),
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
    let worker_contributions = store_worker_contributions(execution, retention)?;
    Ok(StoreBundle {
        admin_ports,
        core_ports,
        provider_ports,
        worker_leader_lease,
        health_probes,
        worker_contributions,
    })
}

fn store_worker_contributions(
    execution: Arc<postgres::PgExecutionStore>,
    retention: Arc<postgres::PgRetentionRepository>,
) -> StoreResult<Vec<WorkerContribution>> {
    let stale_id = WorkerId::try_new(WorkerKind::StaleModelRequestRecovery, "postgres")
        .map_err(worker_definition_error)?;
    let retention_id =
        WorkerId::try_new(WorkerKind::Retention, "postgres").map_err(worker_definition_error)?;
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
        WorkerContribution::Disabled {
            kind: WorkerKind::OpsFlush,
            reason: WorkerDisabledReason::NoBufferedOpsEvents,
        },
    ])
}

fn scheduled_worker(
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

fn worker_definition_error(error: gateway_core::task::WorkerDefinitionError) -> StoreError {
    StoreError::InvalidData {
        entity: "store worker plan",
        message: error.to_string(),
    }
}

struct StaleModelRequestRecoveryTask {
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

struct RetentionTask {
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

struct PostgresHealthProbe {
    pool: sqlx::PgPool,
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

struct RedisHealthProbe {
    connection: ::redis::aio::ConnectionManager,
}

struct AdminAuthStoreAdapter {
    security: postgres::PgAdminSecurityAuditRepository,
    settings: postgres::PgRuntimeSettingsRepository,
    state: redis::RedisAdminAuthStateRepository,
}

struct AdminSettingsStoreAdapter {
    control_plane: postgres::PgControlPlaneRepository,
}

#[async_trait::async_trait]
impl SettingsStore for AdminSettingsStoreAdapter {
    async fn load_runtime_settings(&self) -> AdminStoreResult<AdminRuntimeSettings> {
        let snapshot = postgres::ControlPlaneRepository::load_control_plane(&self.control_plane)
            .await
            .map_err(|error| admin_store_error("runtime settings", error))?;
        admin_runtime_settings(snapshot.settings)
    }

    async fn admin_api_key_exists(&self) -> AdminStoreResult<bool> {
        postgres::ControlPlaneRepository::load_control_plane(&self.control_plane)
            .await
            .map(|snapshot| snapshot.settings.admin_api_key.is_some())
            .map_err(|error| admin_store_error("admin API key", error))
    }

    async fn replace_runtime_settings(
        &self,
        command: ReplaceRuntimeSettings,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminRuntimeSettings> {
        let current = postgres::ControlPlaneRepository::load_control_plane(&self.control_plane)
            .await
            .map_err(|error| admin_store_error("runtime settings", error))?;
        let replacement = postgres::ControlPlaneReplacement {
            settings: postgres::RuntimeSettingsUpdate {
                admin_api_key: current.settings.admin_api_key,
                refresh_margin_seconds: command.refresh_margin_seconds,
                refresh_concurrency: command.refresh_concurrency,
                max_concurrent_per_account: command.max_concurrent_per_account,
                request_interval_ms: command.request_interval_ms,
                rotation_strategy: admin_rotation_name(command.rotation_strategy).to_owned(),
                model_mappings: store_model_mappings(command.model_mappings),
                usage_retention_days: command.usage_retention_days,
                ops_event_retention_days: command.ops_event_retention_days,
                audit_retention_days: command.audit_retention_days,
            },
            audit: mutation_audit(
                context,
                "settings.replace",
                "runtime_settings",
                "1",
                vec![
                    "model_mappings_json".to_owned(),
                    "refresh_margin_seconds".to_owned(),
                    "refresh_concurrency".to_owned(),
                    "max_concurrent_per_account".to_owned(),
                    "request_interval_ms".to_owned(),
                    "rotation_strategy".to_owned(),
                    "retention".to_owned(),
                ],
            ),
        };
        let snapshot = postgres::ControlPlaneRepository::replace_control_plane(
            &self.control_plane,
            store_revision(command.expected_config_revision)?,
            replacement,
        )
        .await
        .map_err(|error| admin_store_error("runtime settings", error))?;
        admin_runtime_settings(snapshot.settings)
    }

    async fn replace_admin_api_key(
        &self,
        expected_config_revision: AdminRevision,
        key: AdminApiKey,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        self.replace_admin_api_key_value(
            expected_config_revision,
            Some(key.expose_for_auth().to_owned()),
            context,
        )
        .await
    }

    async fn delete_admin_api_key(
        &self,
        expected_config_revision: AdminRevision,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        self.replace_admin_api_key_value(expected_config_revision, None, context)
            .await
    }
}

impl AdminSettingsStoreAdapter {
    async fn replace_admin_api_key_value(
        &self,
        expected_config_revision: AdminRevision,
        admin_api_key: Option<String>,
        context: &MutationContext,
    ) -> AdminStoreResult<AdminApiKeyMutation> {
        let current = postgres::ControlPlaneRepository::load_control_plane(&self.control_plane)
            .await
            .map_err(|error| admin_store_error("admin API key", error))?;
        let exists = admin_api_key.is_some();
        let update = postgres::RuntimeSettingsUpdate {
            admin_api_key,
            refresh_margin_seconds: current.settings.refresh_margin_seconds,
            refresh_concurrency: current.settings.refresh_concurrency,
            max_concurrent_per_account: current.settings.max_concurrent_per_account,
            request_interval_ms: current.settings.request_interval_ms,
            rotation_strategy: current.settings.rotation_strategy,
            model_mappings: current.settings.model_mappings,
            usage_retention_days: current.settings.usage_retention_days,
            ops_event_retention_days: current.settings.ops_event_retention_days,
            audit_retention_days: current.settings.audit_retention_days,
        };
        let snapshot = postgres::ControlPlaneRepository::replace_control_plane(
            &self.control_plane,
            store_revision(expected_config_revision)?,
            postgres::ControlPlaneReplacement {
                settings: update,
                audit: mutation_audit(
                    context,
                    if exists {
                        "admin_api_key.replace"
                    } else {
                        "admin_api_key.delete"
                    },
                    "runtime_settings",
                    "1",
                    vec!["admin_api_key".to_owned()],
                ),
            },
        )
        .await
        .map_err(|error| admin_store_error("admin API key", error))?;
        Ok(AdminApiKeyMutation {
            config_revision: admin_revision(snapshot.settings.config_revision)?,
            exists,
        })
    }
}

fn admin_runtime_settings(
    settings: postgres::RuntimeSettings,
) -> AdminStoreResult<AdminRuntimeSettings> {
    let rotation_strategy = match settings.rotation_strategy.as_str() {
        "smart" => AdminRotationStrategy::Smart,
        "quota_reset_priority" => AdminRotationStrategy::QuotaResetPriority,
        "round_robin" => AdminRotationStrategy::RoundRobin,
        "sticky" => AdminRotationStrategy::Sticky,
        _ => {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Invalid,
                "runtime settings",
                "rotation strategy is invalid",
            ));
        }
    };
    let model_mappings = settings
        .model_mappings
        .into_iter()
        .map(|(public, upstream)| {
            let public = gateway_core::routing::PublicModelId::new(public).map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Invalid,
                    "runtime settings",
                    "public model mapping is invalid",
                )
            })?;
            let upstream = gateway_core::routing::UpstreamModelId::new(upstream).map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Invalid,
                    "runtime settings",
                    "upstream model mapping is invalid",
                )
            })?;
            Ok((public, upstream))
        })
        .collect::<AdminStoreResult<ModelMappings>>()?;
    Ok(AdminRuntimeSettings {
        config_revision: admin_revision(settings.config_revision)?,
        model_mappings,
        refresh_margin_seconds: settings.refresh_margin_seconds,
        refresh_concurrency: settings.refresh_concurrency,
        max_concurrent_per_account: settings.max_concurrent_per_account,
        request_interval_ms: settings.request_interval_ms,
        rotation_strategy,
        usage_retention_days: settings.usage_retention_days,
        ops_event_retention_days: settings.ops_event_retention_days,
        audit_retention_days: settings.audit_retention_days,
        updated_at: settings.updated_at,
    })
}

fn store_model_mappings(mappings: ModelMappings) -> std::collections::BTreeMap<String, String> {
    mappings
        .into_iter()
        .map(|(public, upstream)| (public.as_str().to_owned(), upstream.as_str().to_owned()))
        .collect()
}

const fn admin_rotation_name(strategy: AdminRotationStrategy) -> &'static str {
    match strategy {
        AdminRotationStrategy::Smart => "smart",
        AdminRotationStrategy::QuotaResetPriority => "quota_reset_priority",
        AdminRotationStrategy::RoundRobin => "round_robin",
        AdminRotationStrategy::Sticky => "sticky",
    }
}

pub(crate) fn store_revision(revision: AdminRevision) -> AdminStoreResult<Revision> {
    Revision::new(revision.get()).map_err(|error| admin_store_error("config revision", error))
}

pub(crate) fn admin_revision(revision: Revision) -> AdminStoreResult<AdminRevision> {
    AdminRevision::new(revision.get()).map_err(|_| {
        AdminStoreError::new(
            AdminStoreErrorKind::Invalid,
            "config revision",
            "config revision is invalid",
        )
    })
}

pub(crate) fn mutation_audit(
    context: &MutationContext,
    action: &str,
    entity_kind: &str,
    entity_ref: &str,
    changed_fields: Vec<String>,
) -> postgres::AdminAuditEvent {
    let (actor_kind, actor_admin_user_id, actor_ref) = match &context.actor {
        MutationActor::AdminSession { admin_user_id } => (
            postgres::AdminAuditActorKind::AdminSession,
            Some(admin_user_id.clone()),
            admin_user_id.clone(),
        ),
        MutationActor::AdminApiKey => (
            postgres::AdminAuditActorKind::AdminApiKey,
            None,
            "admin_api_key".to_owned(),
        ),
        MutationActor::System => (
            postgres::AdminAuditActorKind::System,
            None,
            "system".to_owned(),
        ),
    };
    postgres::AdminAuditEvent {
        id: format!("audit_{}", uuid::Uuid::now_v7().simple()),
        actor_kind,
        actor_admin_user_id,
        actor_ref,
        admin_request_id: Some(context.request_id.clone()),
        action: action.to_owned(),
        entity_kind: entity_kind.to_owned(),
        entity_ref: entity_ref.to_owned(),
        config_revision: None,
        changed_fields,
        created_at: chrono::Utc::now(),
    }
}

#[async_trait::async_trait]
impl AuthStore for AdminAuthStoreAdapter {
    async fn load_password_hash(&self, admin_user_id: &str) -> AdminStoreResult<Option<String>> {
        postgres::AdminSecurityAuditRepository::password_hash(&self.security, admin_user_id)
            .await
            .map_err(|error| admin_store_error("admin authentication", error))
    }

    async fn create_password_hash_if_absent(
        &self,
        admin_user_id: &str,
        password_hash: &str,
    ) -> AdminStoreResult<bool> {
        postgres::AdminSecurityAuditRepository::create_password_hash_if_absent(
            &self.security,
            admin_user_id,
            password_hash,
        )
        .await
        .map_err(|error| admin_store_error("admin authentication", error))
    }

    async fn load_admin_api_key(&self) -> AdminStoreResult<Option<AdminApiKey>> {
        postgres::RuntimeSettingsRepository::load_runtime_settings(&self.settings)
            .await
            .map(|settings| settings.admin_api_key.map(AdminApiKey::new))
            .map_err(|error| admin_store_error("admin API key", error))
    }

    async fn load_session(&self, session_id: &str) -> AdminStoreResult<Option<AdminSession>> {
        redis::AdminAuthStateRepository::load_admin_session(&self.state, session_id)
            .await
            .map(|session| {
                session.map(|record| AdminSession {
                    admin_user_id: record.admin_user_id,
                    expires_at: record.expires_at,
                })
            })
            .map_err(|error| admin_store_error("admin session", error))
    }

    async fn store_session(
        &self,
        session_id: &str,
        session: &AdminSession,
    ) -> AdminStoreResult<()> {
        redis::AdminAuthStateRepository::store_admin_session(
            &self.state,
            session_id,
            &redis::AdminSessionRecord {
                admin_user_id: session.admin_user_id.clone(),
                expires_at: session.expires_at,
            },
        )
        .await
        .map_err(|error| admin_store_error("admin session", error))
    }

    async fn delete_session(&self, session_id: &str) -> AdminStoreResult<Option<AdminSession>> {
        redis::AdminAuthStateRepository::delete_admin_session(&self.state, session_id)
            .await
            .map(|session| {
                session.map(|record| AdminSession {
                    admin_user_id: record.admin_user_id,
                    expires_at: record.expires_at,
                })
            })
            .map_err(|error| admin_store_error("admin session", error))
    }

    async fn login_source_is_throttled(
        &self,
        source: &str,
        failure_limit: u32,
        window_seconds: u64,
    ) -> AdminStoreResult<bool> {
        redis::AdminAuthStateRepository::login_source_is_throttled(
            &self.state,
            source,
            failure_limit,
            window_seconds,
        )
        .await
        .map_err(|error| admin_store_error("admin login throttle", error))
    }

    async fn record_login_failure(
        &self,
        source: &str,
        failure_limit: u32,
        window_seconds: u64,
    ) -> AdminStoreResult<bool> {
        redis::AdminAuthStateRepository::record_login_failure(
            &self.state,
            source,
            failure_limit,
            window_seconds,
        )
        .await
        .map_err(|error| admin_store_error("admin login throttle", error))
    }

    async fn clear_login_failures(&self, source: &str) -> AdminStoreResult<()> {
        redis::AdminAuthStateRepository::clear_login_failures(&self.state, source)
            .await
            .map_err(|error| admin_store_error("admin login throttle", error))
    }

    async fn append_audit_event(&self, event: AdminAuditModel) -> AdminStoreResult<()> {
        let config_revision = event
            .config_revision
            .map(|revision| i64::try_from(revision.get()))
            .transpose()
            .map_err(|_| {
                AdminStoreError::new(
                    AdminStoreErrorKind::Invalid,
                    "admin audit",
                    "config revision is outside the supported range",
                )
            })?;
        let actor_kind = match event.actor_kind {
            gateway_admin::model::auth::AuditActorKind::AdminSession => {
                postgres::AdminAuditActorKind::AdminSession
            }
            gateway_admin::model::auth::AuditActorKind::AdminApiKey => {
                postgres::AdminAuditActorKind::AdminApiKey
            }
            gateway_admin::model::auth::AuditActorKind::System => {
                postgres::AdminAuditActorKind::System
            }
            gateway_admin::model::auth::AuditActorKind::Anonymous => {
                postgres::AdminAuditActorKind::Anonymous
            }
        };
        postgres::AdminSecurityAuditRepository::append_admin_audit_event(
            &self.security,
            postgres::AdminAuditEvent {
                id: event.id,
                actor_kind,
                actor_admin_user_id: event.actor_admin_user_id,
                actor_ref: event.actor_ref,
                admin_request_id: event.request_id,
                action: event.action,
                entity_kind: event.entity_kind,
                entity_ref: event.entity_ref,
                config_revision,
                changed_fields: event.changed_fields,
                created_at: event.occurred_at,
            },
        )
        .await
        .map_err(|error| admin_store_error("admin audit", error))
    }
}

pub(crate) fn admin_store_error(resource: &'static str, error: StoreError) -> AdminStoreError {
    let kind = match error {
        StoreError::NotFound { .. } => AdminStoreErrorKind::NotFound,
        StoreError::Conflict {
            kind: ConflictKind::StaleRevision,
            ..
        } => AdminStoreErrorKind::StaleRevision,
        StoreError::Conflict { .. } => AdminStoreErrorKind::Conflict,
        StoreError::InvalidData { .. } => AdminStoreErrorKind::Invalid,
        StoreError::Unavailable { .. } => AdminStoreErrorKind::Unavailable,
    };
    AdminStoreError::new(kind, resource, "store operation failed")
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

/// 正整数 revision 或 fencing token。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(NonZeroU64);

impl Revision {
    pub fn new(value: u64) -> StoreResult<Self> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or_else(|| StoreError::InvalidData {
                entity: "revision",
                message: "must be greater than zero".to_owned(),
            })
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// `numeric(20,10)` 可无损表达的非负金额。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DecimalAmount(String);

impl DecimalAmount {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DecimalAmount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for DecimalAmount {
    type Err = StoreError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        let mut parts = input.split('.');
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next();
        let valid = !whole.is_empty()
            && whole.len() <= 10
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && parts.next().is_none()
            && fraction.is_none_or(|value| {
                !value.is_empty()
                    && value.len() <= 10
                    && value.bytes().all(|byte| byte.is_ascii_digit())
            });
        if !valid {
            return Err(StoreError::InvalidData {
                entity: "decimal amount",
                message: "expected a non-negative numeric(20,10) value".to_owned(),
            });
        }

        let whole = whole.trim_start_matches('0');
        let whole = if whole.is_empty() { "0" } else { whole };
        let fraction = fraction.unwrap_or_default().trim_end_matches('0');
        let canonical = if fraction.is_empty() {
            whole.to_owned()
        } else {
            format!("{whole}.{fraction}")
        };
        Ok(Self(canonical))
    }
}

/// Provider-owned JSON object。Store 只验证 object 与大小，不解释内部 key。
#[derive(Clone, PartialEq)]
pub struct JsonObject(Map<String, Value>);

impl JsonObject {
    pub fn try_from_value(
        entity: &'static str,
        value: Value,
        max_serialized_bytes: usize,
    ) -> StoreResult<Self> {
        let serialized_bytes = serde_json::to_vec(&value)
            .map_err(|error| StoreError::InvalidData {
                entity,
                message: error.to_string(),
            })?
            .len();
        let Value::Object(fields) = value else {
            return Err(StoreError::InvalidData {
                entity,
                message: "top-level JSON value must be an object".to_owned(),
            });
        };
        if serialized_bytes > max_serialized_bytes {
            return Err(StoreError::InvalidData {
                entity,
                message: format!("serialized JSON exceeds {max_serialized_bytes} bytes"),
            });
        }
        Ok(Self(fields))
    }

    #[must_use]
    pub fn as_value(&self) -> Value {
        Value::Object(self.0.clone())
    }

    #[must_use]
    pub fn fields(&self) -> &Map<String, Value> {
        &self.0
    }
}

impl fmt::Debug for JsonObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JsonObject([REDACTED])")
    }
}

pub(crate) fn require_nonempty(
    entity: &'static str,
    field: &'static str,
    value: &str,
) -> StoreResult<()> {
    if value.trim().is_empty() {
        Err(StoreError::InvalidData {
            entity,
            message: format!("{field} must not be empty"),
        })
    } else {
        Ok(())
    }
}

pub(crate) fn postgres_unavailable(operation: &'static str) -> StoreError {
    StoreError::Unavailable {
        backend: StoreBackend::PostgreSql,
        message: operation.to_owned(),
    }
}

pub(crate) fn redis_unavailable(operation: &'static str) -> StoreError {
    StoreError::Unavailable {
        backend: StoreBackend::Redis,
        message: operation.to_owned(),
    }
}
