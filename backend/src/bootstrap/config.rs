use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretBox};
use serde::{Deserialize, de::IgnoredAny};
use tracing_subscriber::EnvFilter;
use url::Url;

const CONFIG_SCHEMA_VERSION: u32 = 1;
const CONFIG_RELATIVE_PATH: &str = "deploy/config.yaml";
const SERVER_HOST_ENV: &str = "CPR_SERVER_HOST";
const SERVER_PORT_ENV: &str = "CPR_SERVER_PORT";
const DATABASE_URL_ENV: &str = "CPR_DATABASE_URL";
const REDIS_URL_ENV: &str = "CPR_REDIS_URL";
const SERVICE_PASSWORD_HEX_LENGTH: usize = 48;

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

type SecretValue = SecretBox<String>;

/// 已加载并完成校验的启动配置。
///
/// 数据库、Redis 和管理员初始化密码只在启动阶段保留，并在释放时清零。
#[derive(Debug)]
pub struct BootstrapConfig {
    app: AppConfig,
    database_url: SecretValue,
    redis_url: SecretValue,
    admin_default_password: SecretValue,
}

impl BootstrapConfig {
    /// 从当前目录或父目录中的 `deploy/config.yaml` 加载配置。
    pub fn load() -> Result<Self, ConfigError> {
        let current_directory = env::current_dir().map_err(|_| ConfigError::CurrentDirectory)?;
        let config_path = discover_config_path(&current_directory)?;
        let overrides = TopologyOverrides::from_environment()?;
        Self::load_from_path_with_overrides(config_path, overrides)
    }

    /// 从指定 YAML 文件加载配置，不应用容器拓扑覆盖。
    pub fn load_from_path(config_path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        Self::load_from_path_with_overrides(config_path, TopologyOverrides::default())
    }

    /// 从指定 YAML 文件加载配置，并应用明确的容器拓扑覆盖。
    pub fn load_from_path_with_overrides(
        config_path: impl AsRef<Path>,
        overrides: TopologyOverrides,
    ) -> Result<Self, ConfigError> {
        let config_path = absolute_path(config_path.as_ref())?;
        let config_directory = config_path.parent().ok_or(ConfigError::InvalidConfigPath)?;

        let document: ConfigDocument = ::config::Config::builder()
            .add_source(::config::File::from(config_path.as_path()).required(true))
            .build()
            .and_then(::config::Config::try_deserialize)
            .map_err(|_| ConfigError::InvalidDocument {
                path: config_path.clone(),
            })?;

        let ConfigDocument {
            cpr: mut file_config,
            _services: _,
        } = document;
        if file_config.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedSchemaVersion);
        }

        overrides.apply(&mut file_config);
        validate_file_config(&file_config)?;

        resolve_relative_path(config_directory, &mut file_config.runtime.data_directory);
        resolve_relative_path(config_directory, &mut file_config.logging.file.directory);

        let database_url = connection_url_with_password(
            &file_config.database.url,
            &file_config.database.password,
            "database.url",
        )?;
        let redis_url = connection_url_with_password(
            &file_config.redis.url,
            &file_config.redis.password,
            "redis.url",
        )?;

        let FileAppConfig {
            schema_version: _,
            server,
            api,
            database,
            redis,
            runtime,
            quota,
            ws_pool,
            wire_profile,
            admin,
            logging,
            telemetry,
        } = file_config;

        let app = AppConfig {
            server,
            api,
            model_aliases: BTreeMap::new(),
            auth: AuthConfig::default(),
            quota,
            database: DatabaseConfig { url: database.url },
            redis: RedisConfig { url: redis.url },
            runtime,
            ws_pool,
            wire_profile,
            admin: AdminConfig {
                session_ttl_minutes: admin.session_ttl_minutes,
                default_username: admin.default_username,
            },
            logging,
            telemetry,
        };

        Ok(Self {
            app,
            database_url,
            redis_url,
            admin_default_password: admin.default_password,
        })
    }

    /// 返回不包含启动密码的应用运行配置。
    pub fn app(&self) -> &AppConfig {
        &self.app
    }

    /// 返回已安全注入密码的 PostgreSQL 连接地址。
    pub fn database_url(&self) -> &str {
        self.database_url.expose_secret()
    }

    /// 返回已安全注入密码的 Redis 连接地址。
    pub fn redis_url(&self) -> &str {
        self.redis_url.expose_secret()
    }

    pub(crate) fn into_parts(self) -> BootstrapConfigParts {
        BootstrapConfigParts {
            app: self.app,
            database_url: self.database_url,
            redis_url: self.redis_url,
            admin_default_password: self.admin_default_password,
        }
    }
}

pub(crate) struct BootstrapConfigParts {
    pub(crate) app: AppConfig,
    pub(crate) database_url: SecretValue,
    pub(crate) redis_url: SecretValue,
    pub(crate) admin_default_password: SecretValue,
}

/// Docker Compose 对容器内部网络拓扑的固定覆盖。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TopologyOverrides {
    /// 容器内监听地址。
    pub server_host: Option<String>,
    /// 容器内监听端口。
    pub server_port: Option<u16>,
    /// 容器内 PostgreSQL 地址，不包含密码。
    pub database_url: Option<String>,
    /// 容器内 Redis 地址，不包含密码。
    pub redis_url: Option<String>,
}

impl TopologyOverrides {
    fn from_environment() -> Result<Self, ConfigError> {
        let server_port = optional_environment_value(SERVER_PORT_ENV)?
            .map(|value| {
                value
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port > 0)
                    .ok_or(ConfigError::InvalidTopologyOverride(SERVER_PORT_ENV))
            })
            .transpose()?;

        Ok(Self {
            server_host: optional_environment_value(SERVER_HOST_ENV)?,
            server_port,
            database_url: optional_environment_value(DATABASE_URL_ENV)?,
            redis_url: optional_environment_value(REDIS_URL_ENV)?,
        })
    }

    fn apply(self, config: &mut FileAppConfig) {
        if let Some(host) = self.server_host {
            config.server.host = host;
        }
        if let Some(port) = self.server_port {
            config.server.port = port;
        }
        if let Some(url) = self.database_url {
            config.database.url = url;
        }
        if let Some(url) = self.redis_url {
            config.redis.url = url;
        }
    }
}

/// 应用运行总配置，不包含任何启动密码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    /// HTTP 服务配置。
    pub server: ServerConfig,
    /// API 地址配置。
    pub api: ApiConfig,
    /// 运行时模型别名，由数据库设置加载，配置文件不承载该字段。
    pub model_aliases: BTreeMap<String, String>,
    /// 认证与刷新配置，由固定默认值和数据库运行时设置承载，配置文件不承载该字段。
    pub auth: AuthConfig,
    /// 配额配置。
    pub quota: QuotaConfig,
    /// PostgreSQL 基础连接地址，不包含密码。
    pub database: DatabaseConfig,
    /// Redis 基础连接地址，不包含密码。
    pub redis: RedisConfig,
    /// 本地持久运行目录。
    pub runtime: RuntimePathsConfig,
    /// WebSocket 连接池启动设置。
    pub ws_pool: WebSocketPoolSettings,
    /// 经审计固定的 Codex Desktop 上游请求画像。
    pub wire_profile: WireProfileConfig,
    /// 管理员初始化配置，不包含密码。
    pub admin: AdminConfig,
    /// 日志配置。
    pub logging: LoggingConfig,
    /// PostgreSQL 遥测事实记录配置。
    pub telemetry: TelemetryConfig,
}

/// HTTP 监听配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// 监听主机。
    pub host: String,
    /// 监听端口。
    pub port: u16,
}

/// 上游 API 地址配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiConfig {
    /// 上游基础 URL。
    pub base_url: String,
}

/// 认证、轮换与 token 续期配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuotaConfig {
    /// 配额刷新周期。
    pub refresh_interval_minutes: u64,
    /// 是否跳过已耗尽配额的账号。
    pub skip_exhausted: bool,
}

/// 数据库连接配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseConfig {
    /// PostgreSQL 基础连接 URL，不包含密码。
    pub url: String,
}

/// Redis 连接配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisConfig {
    /// Redis 基础连接 URL，不包含密码。
    pub url: String,
}

/// 本地持久运行目录。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimePathsConfig {
    /// 持久身份密钥与更新状态目录。
    pub data_directory: PathBuf,
}

/// WebSocket 连接池启动设置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WebSocketPoolSettings {
    /// 是否启用连接池。
    pub enabled: bool,
    /// 连接最大存活时长。
    pub max_age_ms: u64,
    /// 单账号最大连接数。
    pub max_per_account: usize,
    /// 首个真实输出（首事件）到达前的绝对超时；`0` 表示禁用。
    pub initial_event_timeout_ms: u64,
}

/// 经审计固定的 Codex Desktop 上游请求画像配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WireProfileConfig {
    /// `originator` 请求头及 User-Agent 产品名。
    pub originator: String,
    /// bundled Codex Core 版本。
    pub codex_version: String,
    /// Desktop 应用版本。
    pub desktop_version: String,
    /// Desktop 制品构建号。
    pub desktop_build: String,
    /// Codex Core UA 中的目标操作系统类型。
    pub os_type: String,
    /// Codex Core UA 中的目标操作系统版本。
    pub os_version: String,
    /// Codex Core UA 中的目标架构。
    pub arch: String,
    /// Codex Core UA 中的终端标记。
    pub terminal: String,
    /// 此画像最后一次经制品与源码核验的时间。
    pub verified_at: DateTime<Utc>,
}

/// 管理员初始化与会话配置，不包含初始化密码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminConfig {
    /// 会话有效期（分钟）。
    pub session_ttl_minutes: u64,
    /// 默认管理员用户名（首次启动时创建）。
    pub default_username: String,
}

/// 应用结构化日志配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// 默认日志级别；进程 `RUST_LOG` 可作临时覆盖。
    pub level: String,
    /// 是否写入标准输出。
    pub stdout: bool,
    /// 文件日志配置。
    pub file: FileLoggingConfig,
}

/// 应用结构化文件日志配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileLoggingConfig {
    /// 是否写入轮转文件。
    pub enabled: bool,
    /// 日志目录。
    pub directory: PathBuf,
    /// 按中国自然日计算的保留天数。
    pub retention_days: usize,
    /// 单文件大小上限，单位 MiB。
    pub max_file_size_mb: u64,
    /// 文件总数上限。
    pub max_files: usize,
}

/// PostgreSQL 遥测事实记录配置。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    /// 是否记录成功与失败代理事实。
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigDocument {
    #[serde(rename = "x-cpr")]
    cpr: FileAppConfig,
    #[serde(rename = "services")]
    _services: IgnoredAny,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileAppConfig {
    schema_version: u32,
    server: ServerConfig,
    api: ApiConfig,
    database: FileDatabaseConfig,
    redis: FileRedisConfig,
    runtime: RuntimePathsConfig,
    quota: QuotaConfig,
    ws_pool: WebSocketPoolSettings,
    wire_profile: WireProfileConfig,
    admin: FileAdminConfig,
    logging: LoggingConfig,
    telemetry: TelemetryConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDatabaseConfig {
    url: String,
    password: SecretValue,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRedisConfig {
    url: String,
    password: SecretValue,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileAdminConfig {
    session_ttl_minutes: u64,
    default_username: String,
    default_password: SecretValue,
}

fn discover_config_path(start: &Path) -> Result<PathBuf, ConfigError> {
    start
        .ancestors()
        .map(|directory| directory.join(CONFIG_RELATIVE_PATH))
        .find(|candidate| candidate.is_file())
        .ok_or(ConfigError::ConfigFileNotFound)
}

fn absolute_path(path: &Path) -> Result<PathBuf, ConfigError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let current_directory = env::current_dir().map_err(|_| ConfigError::CurrentDirectory)?;
    Ok(current_directory.join(path))
}

fn resolve_relative_path(base: &Path, path: &mut PathBuf) {
    if path.is_relative() {
        *path = base.join(&*path);
    }
}

fn optional_environment_value(name: &'static str) -> Result<Option<String>, ConfigError> {
    match env::var(name) {
        Ok(value) if value.trim().is_empty() => Err(ConfigError::InvalidTopologyOverride(name)),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(ConfigError::InvalidTopologyOverride(name)),
    }
}

fn validate_file_config(config: &FileAppConfig) -> Result<(), ConfigError> {
    validate_nonempty("server.host", &config.server.host)?;
    if config.server.port == 0 {
        return Err(ConfigError::InvalidField("server.port"));
    }
    validate_url(
        "api.base_url",
        &config.api.base_url,
        &["http", "https"],
        false,
    )?;
    validate_url(
        "database.url",
        &config.database.url,
        &["postgres", "postgresql"],
        true,
    )?;
    validate_url("redis.url", &config.redis.url, &["redis", "rediss"], true)?;
    validate_service_password("database.password", &config.database.password)?;
    validate_service_password("redis.password", &config.redis.password)?;
    validate_admin_password(config.admin.default_password.expose_secret())?;
    validate_nonempty("admin.default_username", &config.admin.default_username)?;
    if config.admin.session_ttl_minutes == 0 {
        return Err(ConfigError::InvalidField("admin.session_ttl_minutes"));
    }
    if config.runtime.data_directory.as_os_str().is_empty() {
        return Err(ConfigError::InvalidField("runtime.data_directory"));
    }
    if config.quota.refresh_interval_minutes == 0 {
        return Err(ConfigError::InvalidField("quota.refresh_interval_minutes"));
    }
    if config.ws_pool.max_age_ms == 0 {
        return Err(ConfigError::InvalidField("ws_pool.max_age_ms"));
    }
    if config.ws_pool.max_per_account == 0 {
        return Err(ConfigError::InvalidField("ws_pool.max_per_account"));
    }
    validate_wire_profile(&config.wire_profile)?;
    validate_logging(&config.logging)?;
    Ok(())
}

fn validate_url(
    field: &'static str,
    raw_url: &str,
    allowed_schemes: &[&str],
    reject_password: bool,
) -> Result<(), ConfigError> {
    let url = Url::parse(raw_url).map_err(|_| ConfigError::InvalidField(field))?;
    if !allowed_schemes.contains(&url.scheme()) || url.host_str().is_none() {
        return Err(ConfigError::InvalidField(field));
    }
    if reject_password && url.password().is_some() {
        return Err(ConfigError::PasswordInUrl(field));
    }
    Ok(())
}

fn validate_service_password(
    field: &'static str,
    password: &SecretValue,
) -> Result<(), ConfigError> {
    let password = password.expose_secret();
    if password.len() != SERVICE_PASSWORD_HEX_LENGTH
        || !password.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ConfigError::InvalidServicePassword(field));
    }
    Ok(())
}

fn validate_admin_password(password: &str) -> Result<(), ConfigError> {
    let password = password.trim();
    if password.len() < 12
        || password.contains('$')
        || WEAK_ADMIN_PASSWORDS.contains(&password.to_ascii_lowercase().as_str())
    {
        return Err(ConfigError::WeakAdminPassword);
    }
    Ok(())
}

fn validate_wire_profile(config: &WireProfileConfig) -> Result<(), ConfigError> {
    for (field, value) in [
        ("wire_profile.originator", config.originator.as_str()),
        ("wire_profile.codex_version", config.codex_version.as_str()),
        (
            "wire_profile.desktop_version",
            config.desktop_version.as_str(),
        ),
        ("wire_profile.desktop_build", config.desktop_build.as_str()),
        ("wire_profile.os_type", config.os_type.as_str()),
        ("wire_profile.os_version", config.os_version.as_str()),
        ("wire_profile.arch", config.arch.as_str()),
        ("wire_profile.terminal", config.terminal.as_str()),
    ] {
        validate_nonempty(field, value)?;
    }

    if semver::Version::parse(&config.codex_version).is_err() {
        return Err(ConfigError::InvalidField("wire_profile.codex_version"));
    }
    if !is_numeric_dotted_version(&config.desktop_version) {
        return Err(ConfigError::InvalidField("wire_profile.desktop_version"));
    }
    if !config
        .desktop_build
        .bytes()
        .all(|byte| byte.is_ascii_digit())
    {
        return Err(ConfigError::InvalidField("wire_profile.desktop_build"));
    }
    Ok(())
}

fn is_numeric_dotted_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let valid_parts = parts
        .by_ref()
        .filter(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .count();
    valid_parts >= 2 && valid_parts == value.split('.').count()
}

fn validate_logging(config: &LoggingConfig) -> Result<(), ConfigError> {
    EnvFilter::try_new(&config.level).map_err(|_| ConfigError::InvalidField("logging.level"))?;
    if !config.stdout && !config.file.enabled {
        return Err(ConfigError::InvalidField("logging"));
    }
    if config.file.directory.as_os_str().is_empty() {
        return Err(ConfigError::InvalidField("logging.file.directory"));
    }
    if config.file.retention_days == 0 {
        return Err(ConfigError::InvalidField("logging.file.retention_days"));
    }
    if config.file.max_file_size_mb == 0 {
        return Err(ConfigError::InvalidField("logging.file.max_file_size_mb"));
    }
    if config.file.max_files == 0 {
        return Err(ConfigError::InvalidField("logging.file.max_files"));
    }
    Ok(())
}

fn validate_nonempty(field: &'static str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::InvalidField(field));
    }
    Ok(())
}

fn connection_url_with_password(
    raw_url: &str,
    password: &SecretValue,
    field: &'static str,
) -> Result<SecretValue, ConfigError> {
    let mut url = Url::parse(raw_url).map_err(|_| ConfigError::InvalidField(field))?;
    url.set_password(Some(password.expose_secret()))
        .map_err(|()| ConfigError::InvalidField(field))?;
    Ok(SecretBox::new(Box::new(url.into())))
}

/// 启动配置加载错误。错误文本不会包含配置值或连接地址。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    /// 无法确定当前工作目录。
    #[error("failed to determine the current working directory")]
    CurrentDirectory,
    /// 从当前目录及父目录找不到配置文件。
    #[error("deploy/config.yaml was not found in the current directory or its parents")]
    ConfigFileNotFound,
    /// 配置路径没有父目录。
    #[error("configuration file path is invalid")]
    InvalidConfigPath,
    /// YAML 无法读取、解析或不符合结构；不透传解析器文本以避免泄露密码。
    #[error("failed to load configuration file `{path}`; check its permissions and YAML structure")]
    InvalidDocument {
        /// 配置文件路径。
        path: PathBuf,
    },
    /// 配置格式版本不受支持。
    #[error("x-cpr.schema_version must be 1")]
    UnsupportedSchemaVersion,
    /// 字段值非法。
    #[error("configuration field `{0}` is invalid")]
    InvalidField(&'static str),
    /// 连接 URL 不允许内嵌密码。
    #[error("configuration field `{0}` must not contain a password; use its password field")]
    PasswordInUrl(&'static str),
    /// PostgreSQL 或 Redis 密码格式非法。
    #[error("configuration field `{0}` must be exactly 48 hexadecimal characters")]
    InvalidServicePassword(&'static str),
    /// 管理员初始化密码非法。
    #[error("admin.default_password must be strong, at least 12 characters, and contain no `$`")]
    WeakAdminPassword,
    /// Compose 拓扑覆盖非法。
    #[error("container topology override `{0}` is invalid")]
    InvalidTopologyOverride(&'static str),
}
