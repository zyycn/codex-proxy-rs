use std::{collections::BTreeMap, env, path::Path};

use serde::{Deserialize, Serialize};

const WEAK_ADMIN_PASSWORDS: &[&str] = &[
    "",
    "admin",
    "123456",
    "password",
    "changeme",
    "change-me",
    "replace-me",
    "codex-proxy-rs",
];

/// 应用运行总配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// HTTP 服务配置。
    pub server: ServerConfig,
    /// API 地址配置。
    pub api: ApiConfig,
    /// 运行时模型别名，由数据库设置加载，配置文件不承载该字段。
    #[serde(skip)]
    pub model_aliases: BTreeMap<String, String>,
    /// 认证与刷新配置，由固定默认值和数据库运行时设置承载，配置文件不承载该字段。
    #[serde(skip)]
    pub auth: AuthConfig,
    /// 配额配置。
    #[serde(default)]
    pub quota: QuotaConfig,
    /// 数据库配置。
    pub database: DatabaseConfig,
    /// Redis 运行态存储配置。
    pub redis: RedisConfig,
    /// TLS 偏好配置。
    #[serde(default)]
    pub tls: TlsConfig,
    /// WebSocket 连接池启动设置。
    #[serde(default)]
    pub ws_pool: WebSocketPoolSettings,
    /// 上游请求指纹默认配置。
    #[serde(default)]
    pub fingerprint: FingerprintConfig,
    /// 管理员初始化配置。
    #[serde(default)]
    pub admin: AdminConfig,
    /// 日志配置。
    #[serde(default)]
    pub logging: LoggingConfig,
}

/// HTTP 监听配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// 监听主机。
    pub host: String,
    /// 监听端口。
    pub port: u16,
}

/// 上游 API 地址配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ApiConfig {
    /// 上游基础 URL。
    pub base_url: String,
}

/// 认证、轮换与 token 续期配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AuthConfig {
    /// 提前刷新秒数。
    pub refresh_margin_seconds: u64,
    /// 是否启用刷新。
    pub refresh_enabled: bool,
    /// 刷新并发度。
    pub refresh_concurrency: u32,
    /// 单账号最大并发。
    pub max_concurrent_per_account: usize,
    /// 请求最小间隔。
    pub request_interval_ms: u64,
    /// 轮换策略名。
    pub rotation_strategy: String,
    /// 套餐优先级。
    pub tier_priority: Vec<String>,
    /// OAuth 客户端 ID。
    pub oauth_client_id: String,
    /// OAuth token 端点。
    pub oauth_token_endpoint: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            refresh_margin_seconds: 3600,
            refresh_enabled: true,
            refresh_concurrency: 2,
            max_concurrent_per_account: 3,
            request_interval_ms: 50,
            rotation_strategy: "smart".to_string(),
            tier_priority: Vec::new(),
            oauth_client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
            oauth_token_endpoint: "https://auth.openai.com/oauth/token".to_string(),
        }
    }
}

/// 配额刷新与跳过配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuotaConfig {
    /// 配额刷新周期。
    #[serde(default = "default_quota_refresh_interval_minutes")]
    pub refresh_interval_minutes: u64,
    /// 是否跳过已耗尽配额的账号。
    #[serde(default = "default_skip_exhausted")]
    pub skip_exhausted: bool,
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            refresh_interval_minutes: default_quota_refresh_interval_minutes(),
            skip_exhausted: default_skip_exhausted(),
        }
    }
}

fn default_quota_refresh_interval_minutes() -> u64 {
    5
}

fn default_skip_exhausted() -> bool {
    true
}

/// 数据库连接配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DatabaseConfig {
    /// PostgreSQL 连接 URL。
    pub url: String,
}

/// Redis 连接配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RedisConfig {
    /// Redis 连接 URL。
    pub url: String,
}

/// TLS/HTTP 协议偏好配置。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TlsConfig {
    /// 是否强制 HTTP/1.1。
    pub force_http11: bool,
}

/// WebSocket 连接池启动设置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WebSocketPoolSettings {
    /// 是否启用连接池。
    pub enabled: bool,
    /// 连接最大存活时长。
    pub max_age_ms: u64,
    /// 单账号最大连接数。
    pub max_per_account: usize,
    /// 首个真实输出（首 token）到达前的绝对超时；`0` 表示禁用。
    ///
    /// 从发出 `response.create` 起算，覆盖建连/发送/上游排队/思考的全程，直到收到
    /// 首个真实内容帧（`response.created`/`response.in_progress` 等纯生命周期帧不计）。
    /// 超时判定为连接落到病态上游后端，丢弃并换新连接重试。
    #[serde(default = "default_ws_first_token_timeout_ms")]
    pub first_token_timeout_ms: u64,
}

fn default_ws_first_token_timeout_ms() -> u64 {
    20_000
}

impl Default for WebSocketPoolSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_age_ms: 3_300_000,
            max_per_account: 8,
            first_token_timeout_ms: default_ws_first_token_timeout_ms(),
        }
    }
}

/// 上游请求指纹默认配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FingerprintConfig {
    /// 客户端来源名。
    pub originator: String,
    /// 应用版本。
    pub app_version: String,
    /// 构建号。
    pub build_number: String,
    /// 平台名。
    pub platform: String,
    /// 架构名。
    pub arch: String,
    /// Chromium 主版本。
    pub chromium_version: String,
    /// User-Agent 模板。
    pub user_agent_template: String,
    /// 默认请求头。
    pub default_headers: Vec<FingerprintHeaderConfig>,
    /// 请求头排序优先级。
    pub header_order: Vec<String>,
}

impl Default for FingerprintConfig {
    fn default() -> Self {
        Self {
            originator: "Codex Desktop".to_string(),
            app_version: "26.519.81530".to_string(),
            build_number: "3178".to_string(),
            platform: "darwin".to_string(),
            arch: "arm64".to_string(),
            chromium_version: "146".to_string(),
            user_agent_template: "Codex Desktop/{version} ({platform}; {arch})".to_string(),
            default_headers: vec![
                FingerprintHeaderConfig::new("Accept-Encoding", "gzip, deflate, br, zstd"),
                FingerprintHeaderConfig::new("Accept-Language", "en-US,en;q=0.9"),
                FingerprintHeaderConfig::new("sec-ch-ua-mobile", "?0"),
                FingerprintHeaderConfig::new("sec-ch-ua-platform", "\"macOS\""),
                FingerprintHeaderConfig::new("sec-fetch-site", "same-origin"),
                FingerprintHeaderConfig::new("sec-fetch-mode", "cors"),
                FingerprintHeaderConfig::new("sec-fetch-dest", "empty"),
            ],
            header_order: vec![
                "authorization".to_string(),
                "chatgpt-account-id".to_string(),
                "originator".to_string(),
                "x-openai-internal-codex-residency".to_string(),
                "x-client-request-id".to_string(),
                "x-codex-installation-id".to_string(),
                "x-codex-turn-state".to_string(),
                "openai-beta".to_string(),
                "user-agent".to_string(),
                "sec-ch-ua".to_string(),
                "sec-ch-ua-mobile".to_string(),
                "sec-ch-ua-platform".to_string(),
                "accept-encoding".to_string(),
                "accept-language".to_string(),
                "sec-fetch-site".to_string(),
                "sec-fetch-mode".to_string(),
                "sec-fetch-dest".to_string(),
                "content-type".to_string(),
                "accept".to_string(),
                "cookie".to_string(),
            ],
        }
    }
}

/// 指纹默认请求头配置项。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FingerprintHeaderConfig {
    /// 请求头名称。
    pub name: String,
    /// 请求头值。
    pub value: String,
}

impl FingerprintHeaderConfig {
    fn new(name: &str, value: &str) -> Self {
        Self {
            name: name.to_string(),
            value: value.to_string(),
        }
    }
}

/// 管理员初始化与会话配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdminConfig {
    /// 会话有效期（分钟）。
    pub session_ttl_minutes: u64,
    /// 默认管理员用户名（首次启动时创建）
    #[serde(default = "default_admin_username")]
    pub default_username: String,
    /// 默认管理员密码（首次启动时创建）
    #[serde(default = "default_admin_password")]
    pub default_password: String,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            session_ttl_minutes: 1440,
            default_username: default_admin_username(),
            default_password: default_admin_password(),
        }
    }
}

impl AdminConfig {
    /// 校验首次启动创建管理员时使用的默认密码。
    pub fn validate_default_password(&self) -> Result<(), AdminConfigError> {
        let password = self.default_password.trim();
        if password.len() < 12 {
            return Err(AdminConfigError::WeakDefaultPassword);
        }
        if WEAK_ADMIN_PASSWORDS.contains(&password.to_ascii_lowercase().as_str()) {
            return Err(AdminConfigError::WeakDefaultPassword);
        }
        Ok(())
    }
}

/// 管理员配置校验错误。
#[derive(Debug, Clone, Copy, thiserror::Error, PartialEq, Eq)]
pub enum AdminConfigError {
    /// 默认管理员密码为空、过短或属于常见弱口令。
    #[error(
        "admin.default_password must be set to a non-default password with at least 12 characters"
    )]
    WeakDefaultPassword,
}

fn default_admin_username() -> String {
    "admin".to_string()
}

fn default_admin_password() -> String {
    String::new()
}

/// 日志持久化配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// 日志目录。
    pub directory: String,
    /// 保留天数。
    pub retention_days: usize,
    /// 是否启用日志。
    pub enabled: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            directory: ".runtime/logs".to_string(),
            retention_days: 14,
            enabled: true,
        }
    }
}

const CONFIG_FILE_ENV: &str = "CPR_CONFIG_FILE";

impl AppConfig {
    /// 从 `CPR_CONFIG_FILE` 或当前目录 `config.yaml` 加载配置文件。
    pub fn load() -> Result<Self, ::config::ConfigError> {
        let config_file = env::var(CONFIG_FILE_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty());
        match config_file {
            Some(path) => load_file(path),
            None => Self::load_from_dir("."),
        }
    }

    /// 从指定目录加载配置文件。
    pub fn load_from_dir(config_dir: impl AsRef<Path>) -> Result<Self, ::config::ConfigError> {
        load_file(config_dir.as_ref().join("config.yaml"))
    }
}

fn load_file(config_file: impl AsRef<Path>) -> Result<AppConfig, ::config::ConfigError> {
    ::config::Config::builder()
        .add_source(::config::File::from(config_file.as_ref()).required(true))
        .build()?
        .try_deserialize()
}
