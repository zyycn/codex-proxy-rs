use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
    #[serde(default = "default_session_cleanup_interval")]
    pub session_cleanup_interval_secs: u64,
    /// 默认管理员用户名（首次启动时创建）
    #[serde(default = "default_admin_username")]
    pub default_username: String,
    /// 默认管理员密码（首次启动时创建）
    #[serde(default = "default_admin_password")]
    pub default_password: String,
}

fn default_session_cleanup_interval() -> u64 {
    3600 // 默认每小时清理一次过期会话
}

fn default_admin_username() -> String {
    "admin".to_string()
}

fn default_admin_password() -> String {
    "admin".to_string()
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
