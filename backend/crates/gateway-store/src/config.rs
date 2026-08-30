//! Store 启动配置、环境变量解析与校验。

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use super::*;

pub(crate) const DATABASE_URL_ENV: &str = "CPR_DATABASE_URL";
pub(crate) const REDIS_URL_ENV: &str = "CPR_REDIS_URL";
pub(crate) const DATABASE_PASSWORD_ENV: &str = "CPR_DATABASE_PASSWORD";
pub(crate) const REDIS_PASSWORD_ENV: &str = "CPR_REDIS_PASSWORD";
pub(crate) const POSTGRES_STATEMENT_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const POSTGRES_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const POSTGRES_IDLE_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const POSTGRES_HEALTH_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(450);
pub(crate) const POSTGRES_HEALTH_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Store 自己拥有并校验的启动配置。
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    pub(crate) database: StoreConnectionConfig,
    pub(crate) redis: StoreConnectionConfig,
    #[serde(default)]
    pub(crate) pool: StorePoolConfig,
    #[serde(skip)]
    backup_staging_dir: PathBuf,
}

/// PostgreSQL 连接池预算；acquire 超时决定池耗尽时快速失败而非排队积压。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StorePoolConfig {
    pub max_connections: u32,
    pub acquire_timeout_seconds: u64,
}

impl Default for StorePoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 20,
            acquire_timeout_seconds: 5,
        }
    }
}

impl StorePoolConfig {
    pub(crate) fn validate(&self) -> StoreResult<()> {
        if self.max_connections < 2 || self.acquire_timeout_seconds == 0 {
            return Err(StoreError::InvalidData {
                entity: "store config",
                message:
                    "pool.max_connections must be at least 2 and acquire timeout must be positive"
                        .to_owned(),
            });
        }
        Ok(())
    }

    /// 管理观测查询可并发占用的连接数；始终为数据面保留约 20% 的池容量。
    #[must_use]
    pub const fn observability_max_connections(self) -> u32 {
        self.max_connections - self.max_connections.div_ceil(5)
    }

    pub(crate) const fn acquire_timeout(self) -> Duration {
        Duration::from_secs(self.acquire_timeout_seconds)
    }
}

impl StoreConfig {
    pub fn resolve_and_validate(&mut self, runtime_data_dir: &Path) -> StoreResult<()> {
        if runtime_data_dir.as_os_str().is_empty() {
            return Err(StoreError::InvalidData {
                entity: "store config",
                message: "runtime_data_dir must not be empty".to_owned(),
            });
        }
        self.backup_staging_dir = runtime_data_dir.join("backup-staging");
        self.validate_resolved()
    }

    pub(crate) fn validate_resolved(&mut self) -> StoreResult<()> {
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
        self.pool.validate()?;
        if self.backup_staging_dir.as_os_str().is_empty() {
            return Err(StoreError::InvalidData {
                entity: "store config",
                message: "runtime_data_dir was not resolved".to_owned(),
            });
        }
        Ok(())
    }

    pub(crate) fn database_url(&self) -> StoreResult<String> {
        self.database.connection_url("database")
    }

    pub(crate) fn redis_url(&self) -> StoreResult<String> {
        self.redis.connection_url("redis")
    }

    /// 返回由统一运行数据根目录派生的备份暂存目录。
    #[must_use]
    pub fn backup_staging_dir(&self) -> &Path {
        &self.backup_staging_dir
    }
}

pub(crate) fn optional_environment_value(name: &'static str) -> StoreResult<Option<String>> {
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
            .field("pool", &self.pool)
            .finish()
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoreConnectionConfig {
    pub(crate) url: String,
    pub(crate) password: String,
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
