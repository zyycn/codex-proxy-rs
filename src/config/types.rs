use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 应用运行总配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppConfig {
    /// HTTP 服务配置。
    pub server: ServerConfig,
    /// API 地址配置。
    pub api: ApiConfig,
    /// 模型默认值配置。
    pub model: ModelConfig,
    /// 认证与刷新配置。
    pub auth: AuthConfig,
    /// 配额配置。
    pub quota: QuotaConfig,
    /// 用量统计配置。
    pub usage_stats: UsageStatsConfig,
    /// 数据库配置。
    pub database: DatabaseConfig,
    /// TLS 偏好配置。
    pub tls: TlsConfig,
    /// WebSocket 连接池配置。
    pub ws_pool: WebSocketPoolConfig,
    /// 上游请求指纹默认配置。
    pub fingerprint: FingerprintConfig,
    /// 管理员初始化配置。
    pub admin: AdminConfig,
    /// 日志配置。
    pub logging: LoggingConfig,
}

/// HTTP 监听配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
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

/// 模型默认值与别名配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelConfig {
    /// 默认模型名。
    #[serde(rename = "default")]
    pub default_model: String,
    /// 默认推理强度。
    pub default_reasoning_effort: Option<String>,
    /// 默认服务层级。
    #[serde(rename = "default_service_tier")]
    pub service_tier: Option<String>,
    /// 模型别名映射。
    pub aliases: BTreeMap<String, String>,
    /// 模型到账号 ID 的显式路由。
    pub account_routes: BTreeMap<String, Vec<String>>,
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
    /// OAuth 授权端点。
    pub oauth_auth_endpoint: String,
    /// OAuth token 端点。
    pub oauth_token_endpoint: String,
}

/// 配额刷新与跳过配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QuotaConfig {
    /// 配额刷新周期。
    pub refresh_interval_minutes: u64,
    /// 预警阈值。
    pub warning_thresholds: QuotaWarningThresholds,
    /// 是否跳过耗尽账号。
    pub skip_exhausted: bool,
}

/// 配额预警阈值集合。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct QuotaWarningThresholds {
    /// 主阈值列表。
    pub primary: Vec<u8>,
    /// 次阈值列表。
    pub secondary: Vec<u8>,
}

/// 用量历史保留配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct UsageStatsConfig {
    /// 历史保留天数。
    pub history_retention_days: Option<u64>,
}

/// 数据库连接配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DatabaseConfig {
    /// SQLite 连接 URL。
    pub url: String,
}

/// TLS/HTTP 协议偏好配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TlsConfig {
    /// 是否强制 HTTP/1.1。
    pub force_http11: bool,
}

/// WebSocket 连接池配置。
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WebSocketPoolConfig {
    /// 是否启用连接池。
    pub enabled: bool,
    /// 连接最大存活时长。
    pub max_age_ms: u64,
    /// 单账号最大连接数。
    pub max_per_account: usize,
}

impl Default for WebSocketPoolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_age_ms: 3_300_000,
            max_per_account: 8,
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
pub struct AdminConfig {
    /// 会话有效期（分钟）。
    pub session_ttl_minutes: u64,
    /// 会话清理周期（秒）。
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
    /// 内存容量。
    pub capacity: u32,
    /// 是否捕获请求体。
    pub capture_body: bool,
}
