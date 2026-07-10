//! 运行时共享状态。

use async_trait::async_trait;

use crate::{
    api::router::HealthProbe, bootstrap::config::AppConfig, infra::redis::RedisConnection,
};

/// 运行时配置镜像（从 AppConfig 衍生）。
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub admin: crate::bootstrap::config::AdminConfig,
    pub auth: crate::bootstrap::config::AuthConfig,
    pub quota: crate::bootstrap::config::QuotaConfig,
}

/// PostgreSQL 与 Redis 的进程健康检查实现。
#[derive(Clone)]
pub struct RuntimeHealthProbe {
    database: sqlx::PgPool,
    redis: RedisConnection,
}

impl RuntimeHealthProbe {
    pub fn new(database: sqlx::PgPool, redis: RedisConnection) -> Self {
        Self { database, redis }
    }
}

#[async_trait]
impl HealthProbe for RuntimeHealthProbe {
    async fn check(&self) -> Result<(), String> {
        crate::infra::database::ping(&self.database)
            .await
            .map_err(|error| format!("PostgreSQL: {error}"))?;
        self.redis
            .ping()
            .await
            .map_err(|error| format!("Redis: {error}"))
    }
}

impl From<AppConfig> for RuntimeConfig {
    fn from(config: AppConfig) -> Self {
        Self {
            admin: config.admin.clone(),
            auth: config.auth.clone(),
            quota: config.quota,
        }
    }
}
