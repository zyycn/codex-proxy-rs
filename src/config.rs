use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};

pub type ConfigResult<T> = Result<T, config::ConfigError>;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub api: ApiConfig,
    pub model: ModelConfig,
    pub auth: AuthConfig,
    pub quota: QuotaConfig,
    pub usage_stats: UsageStatsConfig,
    pub database: DatabaseConfig,
    pub security: SecurityConfig,
    pub tls: TlsConfig,
    pub admin: AdminConfig,
    pub logging: LoggingConfig,
}

impl AppConfig {
    pub fn load() -> ConfigResult<Self> {
        let _ = dotenvy::dotenv();
        Self::load_from_dir_with_env("config", std::env::vars())
    }

    pub fn load_from_dir_with_env<K, V, I>(
        config_dir: impl AsRef<Path>,
        env: I,
    ) -> ConfigResult<Self>
    where
        K: AsRef<str>,
        V: AsRef<str>,
        I: IntoIterator<Item = (K, V)>,
    {
        let config_dir = config_dir.as_ref();
        let mut cfg: Self = config::Config::builder()
            .add_source(config::File::from(config_dir.join("default.yaml")).required(true))
            .add_source(config::File::from(config_dir.join("local.yaml")).required(false))
            .add_source(config::File::from(config_dir.join("local.yml")).required(false))
            .build()?
            .try_deserialize()?;
        cfg.apply_env(env)?;
        Ok(cfg)
    }

    fn apply_env<K, V, I>(&mut self, env: I) -> ConfigResult<()>
    where
        K: AsRef<str>,
        V: AsRef<str>,
        I: IntoIterator<Item = (K, V)>,
    {
        for (key, value) in env {
            let key = key.as_ref();
            let value = value.as_ref();
            match key {
                "CPRS_HOST" => self.server.host = value.to_string(),
                "CPRS_PORT" => self.server.port = parse_env(key, value)?,
                "CPRS_BASE_URL" => self.api.base_url = value.to_string(),
                "CPRS_DEFAULT_MODEL" => self.model.default_model = value.to_string(),
                "CPRS_DEFAULT_REASONING_EFFORT" => {
                    self.model.default_reasoning_effort = parse_optional_string(value);
                }
                "CPRS_SERVICE_TIER" => self.model.service_tier = parse_optional_string(value),
                "CPRS_REFRESH_MARGIN_SECONDS" => {
                    self.auth.refresh_margin_seconds = parse_env(key, value)?;
                }
                "CPRS_REFRESH_ENABLED" => self.auth.refresh_enabled = parse_env(key, value)?,
                "CPRS_REFRESH_CONCURRENCY" => {
                    self.auth.refresh_concurrency = parse_env(key, value)?;
                }
                "CPRS_MAX_CONCURRENT_PER_ACCOUNT" => {
                    self.auth.max_concurrent_per_account = parse_env(key, value)?;
                }
                "CPRS_REQUEST_INTERVAL_MS" => {
                    self.auth.request_interval_ms = parse_env(key, value)?;
                }
                "CPRS_ROTATION_STRATEGY" => self.auth.rotation_strategy = value.to_string(),
                "CPRS_QUOTA_REFRESH_INTERVAL_MINUTES" => {
                    self.quota.refresh_interval_minutes = parse_env(key, value)?;
                }
                "CPRS_QUOTA_SKIP_EXHAUSTED" => {
                    self.quota.skip_exhausted = parse_env(key, value)?;
                }
                "CPRS_USAGE_HISTORY_RETENTION_DAYS" => {
                    self.usage_stats.history_retention_days = parse_optional_env(key, value)?;
                }
                "CPRS_DATABASE_URL" => self.database.url = value.to_string(),
                "CPRS_MASTER_KEY_FILE" => self.security.master_key_file = value.to_string(),
                "CPRS_API_KEY_PEPPER_FILE" => {
                    self.security.api_key_pepper_file = value.to_string();
                }
                "CPRS_FORCE_HTTP11" => self.tls.force_http11 = parse_env(key, value)?,
                "CPRS_ADMIN_SESSION_TTL_MINUTES" => {
                    self.admin.session_ttl_minutes = parse_env(key, value)?;
                }
                "CPRS_LOG_DIR" => self.logging.directory = value.to_string(),
                "CPRS_LOG_MAX_FILE_BYTES" => self.logging.max_file_bytes = parse_env(key, value)?,
                "CPRS_LOG_RETENTION_DAYS" => self.logging.retention_days = parse_env(key, value)?,
                "CPRS_LOGS_ENABLED" => self.logging.enabled = parse_env(key, value)?,
                "CPRS_LOGS_CAPACITY" => self.logging.capacity = parse_env(key, value)?,
                "CPRS_LOGS_CAPTURE_BODY" => self.logging.capture_body = parse_env(key, value)?,
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ApiConfig {
    pub base_url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelConfig {
    #[serde(rename = "default")]
    pub default_model: String,
    pub default_reasoning_effort: Option<String>,
    #[serde(rename = "default_service_tier")]
    pub service_tier: Option<String>,
    pub aliases: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AuthConfig {
    pub refresh_margin_seconds: u64,
    pub refresh_enabled: bool,
    pub refresh_concurrency: u32,
    pub max_concurrent_per_account: usize,
    pub request_interval_ms: u64,
    pub rotation_strategy: String,
    pub tier_priority: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QuotaConfig {
    pub refresh_interval_minutes: u64,
    pub warning_thresholds: QuotaWarningThresholds,
    pub skip_exhausted: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QuotaWarningThresholds {
    pub primary: Vec<u8>,
    pub secondary: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct UsageStatsConfig {
    pub history_retention_days: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SecurityConfig {
    pub master_key_file: String,
    pub api_key_pepper_file: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TlsConfig {
    pub force_http11: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AdminConfig {
    pub session_ttl_minutes: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LoggingConfig {
    pub directory: String,
    pub max_file_bytes: u64,
    pub retention_days: u64,
    pub enabled: bool,
    pub capacity: u32,
    pub capture_body: bool,
}

fn parse_env<T>(key: &str, value: &str) -> ConfigResult<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse::<T>()
        .map_err(|error| config::ConfigError::Message(format!("invalid {key}: {error}")))
}

fn parse_optional_env<T>(key: &str, value: &str) -> ConfigResult<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match parse_optional_string(value) {
        Some(value) => parse_env(key, &value).map(Some),
        None => Ok(None),
    }
}

fn parse_optional_string(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value != "null").then(|| value.to_string())
}
