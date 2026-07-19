//! 网关唯一组合根：启动配置、基础设施装配、Provider/Admin 投影和 HTTP 生命周期。

pub mod workers;

use std::{
    env,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use futures::StreamExt as _;
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
            database: _,
            redis: _,
            wire_profile,
            admin,
            logging,
        } = file_config;

        let app = AppConfig {
            server,
            wire_profile,
            admin: AdminConfig {
                session_ttl_minutes: admin.session_ttl_minutes,
                default_username: admin.default_username,
            },
            logging,
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
    /// 经审计固定的 Codex Desktop 上游请求画像。
    pub wire_profile: WireProfileConfig,
    /// 管理员初始化配置，不包含密码。
    pub admin: AdminConfig,
    /// 日志配置。
    pub logging: LoggingConfig,
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

impl WireProfileConfig {
    #[must_use]
    pub fn user_agent(&self) -> String {
        format!(
            "{}/{} ({} {}; {}) {} ({}; {})",
            self.originator,
            self.codex_version,
            self.os_type,
            self.os_version,
            self.arch,
            self.terminal,
            self.originator,
            self.desktop_version,
        )
    }

    #[must_use]
    pub fn dashboard_view(&self) -> gateway_api::admin::observability::DashboardWireProfileView {
        gateway_api::admin::observability::DashboardWireProfileView {
            originator: self.originator.clone(),
            codex_version: self.codex_version.clone(),
            desktop_version: self.desktop_version.clone(),
            desktop_build: self.desktop_build.clone(),
            target: gateway_api::admin::observability::DashboardWireTargetView {
                os_type: self.os_type.clone(),
                os_version: self.os_version.clone(),
                arch: self.arch.clone(),
                terminal: self.terminal.clone(),
            },
            user_agent: self.user_agent(),
            verified_at: self.verified_at,
            release: gateway_api::admin::observability::DashboardDesktopReleaseView {
                status: "unchecked".to_owned(),
                checked_at: None,
                latest_version: None,
                latest_build: None,
                published_at: None,
                minimum_system_version: None,
                hardware_requirements: None,
                download_url: None,
                download_size: None,
                signature_present: None,
                error: None,
            },
        }
    }
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
    database: FileDatabaseConfig,
    redis: FileRedisConfig,
    wire_profile: WireProfileConfig,
    admin: FileAdminConfig,
    logging: LoggingConfig,
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
    validate_wire_profile(&config.wire_profile)?;
    validate_logging(&config.logging)?;
    Ok(())
}

use chrono::{Duration as ChronoDuration, FixedOffset, TimeZone, Timelike};
use gateway_api::admin::{
    WireValidationError,
    observability::{
        AttemptMetricsView, BillingView, CostCoverageView, CostView, CursorWire,
        DashboardAccountUsageView, DashboardCacheCardView, DashboardCapacityInfoView,
        DashboardCardsView, DashboardCredentialUsageView, DashboardCredentialsCardView,
        DashboardDataView, DashboardPoolSummaryView, DashboardQuery, DashboardTokensCardView,
        DashboardTrafficCardView, DetailQuery, DiagnosticDimension as WireDiagnosticDimension,
        DiagnosticItemView, DiagnosticsQuery, DiagnosticsView, HealthTimelinePointView,
        HealthTimelineView, OpsErrorMetadataView, OpsErrorView, OpsQuery, OverviewCostPointView,
        OverviewCostView, OverviewHealthPointView, OverviewHealthView,
        OverviewPerformancePointView, OverviewPerformanceView, PageData, PageMeta,
        ProviderOverviewView, RequestMetricsView, TokenDetailsView, TrendData, TrendKind,
        TrendPointView, TrendSummaryView, UsageAttemptView, UsageInsightsOverviewView, UsageQuery,
        UsageRecordDetailView, UsageRecordMetadataView, UsageRecordView, UsageSummaryView,
        parse_attempt_index as parse_wire_attempt_index, parse_datetime as parse_wire_datetime,
        parse_status as parse_wire_status,
    },
};
use gateway_store::DecimalAmount;
use gateway_store::postgres::{
    AttemptMetrics, CurrencyCostTotal, DashboardObservation,
    DiagnosticDimension as StoreDiagnosticDimension, DiagnosticObservation, ObservabilityCursor,
    ObservabilityPageNumber, ObservabilityPageSize, ObservabilityRange, ObservabilityRepository,
    OpsErrorFilter, OpsErrorPage, OpsErrorQuery, OpsErrorRecord, RequestMetricPoint,
    RequestMetrics, UsageAttemptObservation, UsageOverview, UsageRecord, UsageRecordDetail,
    UsageRecordFilter, UsageRecordPage, UsageRecordQuery,
};
use serde::{Serialize, de::DeserializeOwned};

const MAX_CURSOR_BYTES: usize = 1024;
const HEALTH_TIMELINE_SLOT_MINUTES: i64 = 15;
const HEALTH_TIMELINE_SLOTS: i64 = 24 * 4;
const HEALTH_TIMELINE_MIN_SAMPLE_SIZE: u64 = 10;
const HEALTH_TIMELINE_UNAVAILABLE_FAILURE_THRESHOLD: u64 = 3;
const HEALTH_TIMELINE_STABLE_RELIABILITY: f64 = 99.0;

#[derive(Debug, Clone, Copy, Default)]
struct HealthWindow {
    success_requests: u64,
    failed_requests: u64,
    cancelled_requests: u64,
    incomplete_requests: u64,
    caller_error_requests: u64,
}

fn format_number(value: u64) -> String {
    let text = value.to_string();
    let mut output = String::with_capacity(text.len() + text.len() / 3);
    for (index, character) in text.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            output.push(',');
        }
        output.push(character);
    }
    output.chars().rev().collect()
}

fn format_compact_number(value: u64) -> String {
    if value < 1_000 {
        return format_number(value);
    }
    for (suffix, threshold) in [
        ("P", 1_000_000_000_000_000_u64),
        ("T", 1_000_000_000_000_u64),
        ("B", 1_000_000_000_u64),
        ("M", 1_000_000_u64),
        ("K", 1_000_u64),
    ] {
        if value >= threshold {
            let scaled = value as f64 / threshold as f64;
            return format!("{scaled:.1}{suffix}").replace(".0", "");
        }
    }
    format_number(value)
}

fn format_tokens(value: u64) -> String {
    format_compact_number(value)
}

fn format_billing_amount(value: &DecimalAmount) -> String {
    format!("${}", value.as_str())
}

fn format_duration_ms(value: Option<i64>) -> String {
    let Some(value) = value.filter(|value| *value >= 0) else {
        return "—".to_owned();
    };
    if value < 1_000 {
        format!("{value} ms")
    } else if value < 60_000 {
        format!("{:.2} s", value as f64 / 1_000.0)
    } else if value < 3_600_000 {
        format!("{:.1} min", value as f64 / 60_000.0)
    } else {
        format!("{:.1} h", value as f64 / 3_600_000.0)
    }
}

pub struct ObservabilityAdminAdapter {
    repository: Arc<dyn ObservabilityRepository>,
    accounts: Arc<dyn gateway_store::postgres::ProviderAccountRepository>,
    runtime_settings: Arc<dyn gateway_store::postgres::RuntimeSettingsRepository>,
    wire_profile: gateway_api::admin::observability::DashboardWireProfileView,
}

impl ObservabilityAdminAdapter {
    #[must_use]
    pub fn new(
        repository: Arc<dyn ObservabilityRepository>,
        accounts: Arc<dyn gateway_store::postgres::ProviderAccountRepository>,
        runtime_settings: Arc<dyn gateway_store::postgres::RuntimeSettingsRepository>,
        wire_profile: gateway_api::admin::observability::DashboardWireProfileView,
    ) -> Self {
        Self {
            repository,
            accounts,
            runtime_settings,
            wire_profile,
        }
    }
}

#[async_trait]
impl ObservabilityAdminService for ObservabilityAdminAdapter {
    async fn dashboard_summary(
        &self,
        query: DashboardQuery,
    ) -> Result<DashboardDataView, AdminServiceError> {
        let kind = query.trend_kind().map_err(map_wire_error)?;
        let range = dashboard_range(query.start_time, query.end_time)?;
        let (observation, accounts, runtime_settings) = tokio::try_join!(
            self.repository.dashboard_summary(range),
            self.accounts.list_provider_accounts(None, true),
            self.runtime_settings.load_runtime_settings(),
        )
        .map_err(|error| map_store_error(error, "Dashboard"))?;
        Ok(dashboard_view(
            observation,
            accounts,
            &runtime_settings,
            kind,
            self.wire_profile.clone(),
        ))
    }

    async fn dashboard_trend(&self, query: DashboardQuery) -> Result<TrendData, AdminServiceError> {
        let kind = query.trend_kind().map_err(map_wire_error)?;
        let range = dashboard_today_range(query.start_time, query.end_time)?;
        let points = self
            .repository
            .dashboard_trend(range)
            .await
            .map_err(|error| map_store_error(error, "Dashboard trend"))?;
        Ok(trend_view(kind, &points))
    }

    async fn usage_records(
        &self,
        query: UsageQuery,
    ) -> Result<PageData<UsageRecordView>, AdminServiceError> {
        let (store_query, page, page_size) = usage_store_query(&query)?;
        let result = self
            .repository
            .list_usage_records(store_query)
            .await
            .map_err(|error| map_store_error(error, "Usage records"))?;
        usage_page_view(result, page, page_size)
    }

    async fn usage_record_detail(
        &self,
        query: DetailQuery,
    ) -> Result<UsageRecordDetailView, AdminServiceError> {
        query.validate().map_err(map_wire_error)?;
        let detail = self
            .repository
            .usage_record_detail(query.id.trim())
            .await
            .map_err(|error| map_store_error(error, "Usage record"))?;
        Ok(usage_detail_view(detail))
    }

    async fn usage_records_summary(
        &self,
        query: UsageQuery,
    ) -> Result<UsageSummaryView, AdminServiceError> {
        let range = usage_range(query.start_time.as_deref(), query.end_time.as_deref())?;
        let filter = usage_filter(&query)?;
        let overview = self
            .repository
            .usage_summary(range, filter)
            .await
            .map_err(|error| map_store_error(error, "Usage summary"))?;
        let average_latency = average(
            overview.requests.latency_sum,
            overview.requests.latency_count,
        );
        Ok(UsageSummaryView {
            total_requests: format_compact_number(overview.requests.request_count),
            input_tokens: format_tokens(overview.requests.input_tokens),
            output_tokens: format_tokens(overview.requests.output_tokens),
            cached_tokens: format_tokens(overview.requests.cached_tokens),
            cache_write_tokens: format_tokens(overview.requests.cache_write_tokens),
            total_tokens: format_tokens(overview.requests.total_tokens),
            average_latency_ms: display_duration(average_latency),
            logical_requests: request_metrics_view(&overview.requests),
            attempts: attempt_metrics_view(&overview.attempts),
        })
    }

    async fn usage_insights_overview(
        &self,
        query: UsageQuery,
    ) -> Result<UsageInsightsOverviewView, AdminServiceError> {
        let range = usage_range(query.start_time.as_deref(), query.end_time.as_deref())?;
        let filter = usage_filter(&query)?;
        let (overview, points) = tokio::try_join!(
            self.repository.usage_summary(range, filter.clone()),
            self.repository.usage_trend(range, filter),
        )
        .map_err(|error| map_store_error(error, "Usage insights"))?;
        Ok(overview_view(overview, &points))
    }

    async fn usage_insights_diagnostics(
        &self,
        query: DiagnosticsQuery,
    ) -> Result<DiagnosticsView, AdminServiceError> {
        let wire_dimension = query.dimension().map_err(map_wire_error)?;
        let (dimension, dimension_name) = map_diagnostic_dimension(wire_dimension);
        let range = usage_range(query.start_time.as_deref(), query.end_time.as_deref())?;
        let status_code = parse_wire_status(query.status_code).map_err(map_wire_error)?;
        let filter = UsageRecordFilter {
            provider_kind: non_empty(query.provider),
            model: non_empty(query.model),
            status_code,
            search: non_empty(query.search),
            ..UsageRecordFilter::default()
        };
        let items = self
            .repository
            .usage_diagnostics(range, filter, dimension)
            .await
            .map_err(|error| map_store_error(error, "Usage diagnostics"))?;
        let total = items.iter().map(|item| item.request_count).sum();
        Ok(DiagnosticsView {
            dimension: dimension_name.to_owned(),
            items: items
                .into_iter()
                .map(|item| diagnostic_item_view(item, total))
                .collect(),
        })
    }

    async fn ops_errors(
        &self,
        query: OpsQuery,
    ) -> Result<PageData<OpsErrorView>, AdminServiceError> {
        let (page_number, page_size_number) = query.validate_page().map_err(map_wire_error)?;
        query.validate_cursor().map_err(map_wire_error)?;
        let page = ObservabilityPageNumber::new(page_number).map_err(map_invalid_store)?;
        let page_size = ObservabilityPageSize::new(page_size_number).map_err(map_invalid_store)?;
        let cursor = decode_observability_cursor(query.cursor.as_deref())?;
        let range = usage_range(query.start_time.as_deref(), query.end_time.as_deref())?;
        let status_code = parse_wire_status(
            query
                .upstream_status_code
                .or(query.client_status_code)
                .or(query.status_code),
        )
        .map_err(map_wire_error)?;
        let filter = OpsErrorFilter {
            client_api_key_ref: non_empty(query.client_api_key_id),
            request_id: non_empty(query.request_id),
            provider_account_ref: non_empty(query.account_id),
            provider_kind: non_empty(query.provider),
            operation: non_empty(query.route).or_else(|| non_empty(query.kind)),
            model: non_empty(query.model),
            transport: non_empty(query.transport),
            attempt_index: parse_wire_attempt_index(query.attempt_index).map_err(map_wire_error)?,
            response_id: non_empty(query.response_id),
            upstream_request_id: non_empty(query.upstream_request_id),
            failure_kind: non_empty(query.failure_class),
            status_code,
            search: non_empty(query.search),
        };
        let result = self
            .repository
            .list_ops_errors(OpsErrorQuery {
                range,
                filter,
                cursor,
                page,
                page_size,
            })
            .await
            .map_err(|error| map_store_error(error, "Ops errors"))?;
        ops_page_view(result, page.get(), page_size.get())
    }
}

fn dashboard_view(
    observation: DashboardObservation,
    accounts: Vec<gateway_store::postgres::ProviderAccountSummary>,
    runtime_settings: &gateway_store::postgres::RuntimeSettings,
    kind: TrendKind,
    wire_profile: gateway_api::admin::observability::DashboardWireProfileView,
) -> DashboardDataView {
    let today_start = china_day_start(observation.range.end);
    let yesterday_start = today_start - ChronoDuration::days(1);
    let today = sum_points(&observation.trend, today_start, observation.range.end);
    let yesterday = sum_points(&observation.trend, yesterday_start, today_start);
    let costs = cost_views(&observation.attempts.costs);
    let usd = currency_amount(&observation.attempts.costs, "USD");
    let trend = trend_view(kind, &observation.trend);
    let health_timeline = health_timeline_view(&observation.trend);
    let account_usage = observation
        .account_usage
        .iter()
        .map(|credential| DashboardAccountUsageView {
            id: credential.account_id.clone(),
            email: credential
                .email
                .clone()
                .unwrap_or_else(|| credential.name.clone()),
            plan_type: credential.plan_type.clone(),
            tokens: credential
                .total_tokens
                .map_or_else(|| "—".to_owned(), format_tokens),
            quota_used_percent: None,
            last_used: relative_time(credential.last_used_at, observation.range.end),
        })
        .collect::<Vec<_>>();
    let credential_usage = observation
        .account_usage
        .into_iter()
        .map(|credential| DashboardCredentialUsageView {
            id: credential.account_id,
            display_name: credential.email.unwrap_or(credential.name),
            plan_type: credential.plan_type,
            tokens: credential
                .total_tokens
                .map_or_else(|| "-".to_owned(), format_tokens),
            tokens_value: credential.total_tokens,
            last_used: credential
                .last_used_at
                .map_or_else(|| "-".to_owned(), |value| china_datetime(&value)),
            provider: credential.provider_kind,
            availability: credential.availability,
            request_count: credential.request_count,
        })
        .collect::<Vec<_>>();
    let now = observation.range.end;
    let total = u64::try_from(accounts.len()).unwrap_or(u64::MAX);
    let active = u64::try_from(
        accounts
            .iter()
            .filter(|account| {
                account.enabled
                    && account.availability == ProviderAccountAvailability::Ready
                    && account.access_token_expires_at > now
            })
            .count(),
    )
    .unwrap_or(u64::MAX);
    let pool_summary = DashboardPoolSummaryView {
        total,
        active,
        expired: u64::try_from(
            accounts
                .iter()
                .filter(|account| {
                    account.availability == ProviderAccountAvailability::Expired
                        || account.access_token_expires_at <= now
                })
                .count(),
        )
        .unwrap_or(u64::MAX),
        quota_exhausted: u64::try_from(
            accounts
                .iter()
                .filter(|account| {
                    account.availability == ProviderAccountAvailability::QuotaExhausted
                })
                .count(),
        )
        .unwrap_or(u64::MAX),
        refreshing: None,
        disabled: u64::try_from(accounts.iter().filter(|account| !account.enabled).count())
            .unwrap_or(u64::MAX),
        banned: u64::try_from(
            accounts
                .iter()
                .filter(|account| account.availability == ProviderAccountAvailability::Banned)
                .count(),
        )
        .unwrap_or(u64::MAX),
    };
    let max_concurrent = u64::from(runtime_settings.max_concurrent_per_account);
    let total_slots = active.saturating_mul(max_concurrent);
    let usage_records = observation
        .recent_requests
        .into_iter()
        .map(usage_record_view)
        .collect::<Vec<_>>();

    DashboardDataView {
        cards: DashboardCardsView {
            credentials: DashboardCredentialsCardView {
                total: format_compact_number(observation.provider_accounts.total),
                total_value: observation.provider_accounts.total,
                enabled: format_compact_number(observation.provider_accounts.enabled),
                enabled_value: observation.provider_accounts.enabled,
                unavailable: format_compact_number(observation.provider_accounts.unavailable),
                unavailable_value: observation.provider_accounts.unavailable,
            },
            traffic: DashboardTrafficCardView {
                today_requests: format_compact_number(today.request_count),
                today_requests_value: today.request_count,
                yesterday_requests_value: yesterday.request_count,
                total_requests: format_compact_number(observation.requests.request_count),
            },
            tokens: DashboardTokensCardView {
                today_tokens: format_tokens(today.total_tokens),
                today_tokens_value: today.total_tokens,
                yesterday_tokens_value: yesterday.total_tokens,
                total_tokens: format_tokens(observation.requests.total_tokens),
                total_billing_amount_usd: usd.map_or_else(|| "—".to_owned(), format_billing_amount),
            },
            cache: DashboardCacheCardView {
                today_hit_rate: display_rate(rate(today.cached_tokens, today.input_tokens)),
                today_hit_rate_value: optional_rate(today.cached_tokens, today.input_tokens),
                yesterday_hit_rate_value: optional_rate(
                    yesterday.cached_tokens,
                    yesterday.input_tokens,
                ),
                total_hit_rate: display_rate(rate(
                    observation.requests.cached_tokens,
                    observation.requests.input_tokens,
                )),
                total_cached_tokens: format_tokens(observation.requests.cached_tokens),
                average_first_token_latency_ms: display_duration(average(
                    observation.requests.first_token_latency_sum,
                    observation.requests.first_token_latency_count,
                )),
            },
        },
        trend,
        health_timeline,
        wire_profile,
        account_usage,
        credential_usage,
        usage_records,
        pool_summary,
        capacity_info: DashboardCapacityInfoView {
            max_concurrent_per_account: max_concurrent,
            total_slots,
            used_slots: None,
            available_slots: None,
        },
        rotation_strategy: runtime_settings.rotation_strategy.clone(),
        logical_requests: request_metrics_view(&observation.requests),
        attempts: attempt_metrics_view(&observation.attempts),
        costs,
    }
}

fn relative_time(value: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let Some(value) = value else {
        return "从未使用".to_owned();
    };
    let elapsed = now.signed_duration_since(value);
    if elapsed.num_seconds() < 0 {
        return china_datetime(&value);
    }
    if elapsed.num_seconds() < 60 {
        return "刚刚".to_owned();
    }
    if elapsed.num_minutes() < 60 {
        return format!("{} 分钟前", elapsed.num_minutes());
    }
    if elapsed.num_hours() < 24 {
        return format!("{} 小时前", elapsed.num_hours());
    }
    format!("{} 天前", elapsed.num_days())
}

fn usage_page_view(
    page: UsageRecordPage,
    page_number: u32,
    page_size: u16,
) -> Result<PageData<UsageRecordView>, AdminServiceError> {
    Ok(PageData {
        items: page.items.into_iter().map(usage_record_view).collect(),
        page: page_meta(page_number, page_size, page.total),
        next_cursor: page
            .next_cursor
            .as_ref()
            .map(encode_observability_cursor)
            .transpose()?,
    })
}

fn ops_page_view(
    page: OpsErrorPage,
    page_number: u32,
    page_size: u16,
) -> Result<PageData<OpsErrorView>, AdminServiceError> {
    Ok(PageData {
        items: page.items.into_iter().map(ops_error_view).collect(),
        page: page_meta(page_number, page_size, page.total),
        next_cursor: page
            .next_cursor
            .as_ref()
            .map(encode_observability_cursor)
            .transpose()?,
    })
}

fn page_meta(page: u32, page_size: u16, total: u64) -> PageMeta {
    let page_size_u64 = u64::from(page_size);
    let total_pages_u64 = total.saturating_add(page_size_u64 - 1) / page_size_u64;
    PageMeta {
        page,
        page_size,
        total,
        total_pages: u32::try_from(total_pages_u64).unwrap_or(u32::MAX),
    }
}

fn usage_record_view(record: UsageRecord) -> UsageRecordView {
    let tokens = token_details(&record);
    let billing = usage_billing_view(&record);
    let costs = record
        .cost_amount
        .as_ref()
        .zip(record.cost_currency.as_ref())
        .map(|(amount, currency)| {
            vec![CostView {
                currency: currency.clone(),
                estimated_amount: amount.to_string(),
            }]
        })
        .unwrap_or_default();
    let status_code = record
        .client_status_code
        .or(record.upstream_status_code)
        .map(i64::from);
    let message = record
        .error_message
        .clone()
        .unwrap_or_else(|| record.outcome.clone());
    let first_token_display = display_duration(record.first_token_ms);
    let latency_display = display_duration(record.latency_ms);
    let cost_coverage = match record.cost_source.as_str() {
        "provider_reported" | "calculated" => CostCoverageView {
            known: 1,
            partial: 0,
            unknown: 0,
            not_billable: 0,
        },
        _ => CostCoverageView {
            known: 0,
            partial: 0,
            unknown: 1,
            not_billable: 0,
        },
    };
    UsageRecordView {
        id: record.id.clone(),
        request_id: record.id,
        client_api_key_id: Some(record.client_api_key_ref),
        kind: record.operation.clone(),
        provider: record.provider_kind,
        account_id: record.provider_account_ref,
        account_email: record.provider_account_email,
        route: None,
        model: record
            .upstream_model_id
            .clone()
            .unwrap_or_else(|| record.requested_model_id.clone()),
        requested_model: Some(record.requested_model_id),
        upstream_model: record.upstream_model_id,
        service_tier: None,
        status_code,
        transport: record.upstream_transport.or(Some(record.client_transport)),
        attempt_index: None,
        attempt_count: u64::from(record.attempt_count),
        response_id: record.client_response_id,
        upstream_request_id: record.upstream_request_id,
        latency_ms: record.latency_ms,
        first_token_ms: record.first_token_ms,
        input_tokens: record.input_tokens,
        output_tokens: record.output_tokens,
        cached_tokens: record.cached_tokens,
        cache_write_tokens: record.cache_write_tokens,
        reasoning_tokens: record.reasoning_tokens,
        message,
        metadata: UsageRecordMetadataView {
            protocol: record.protocol,
            logical_outcome: record.outcome.clone(),
            attempt_count: u64::from(record.attempt_count),
        },
        created_at: record.started_at,
        created_at_display: china_datetime(&record.started_at),
        client_ip: record.client_ip,
        user_agent: record.user_agent,
        reasoning_effort: record.reasoning_effort,
        reasoning_preset: record.reasoning_preset,
        compact: Some(record.compact),
        request_kind: record.request_kind,
        subagent_kind: record.subagent_kind,
        token_details: tokens,
        billing,
        costs,
        cost_coverage,
        first_token_latency_ms: record.first_token_ms,
        first_token_latency_ms_display: first_token_display,
        latency_ms_display: latency_display,
        logical_outcome: record.outcome,
    }
}

fn usage_billing_view(record: &UsageRecord) -> Option<BillingView> {
    let total = record.cost_amount.as_ref()?;
    let currency = record.cost_currency.as_deref()?;
    if record.cost_source != "calculated" {
        return Some(total_only_billing_view(total, currency));
    }
    let Some(breakdown) = calculated_billing_breakdown(record) else {
        return Some(total_only_billing_view(total, currency));
    };
    let Ok(persisted_total) = total.as_str().parse::<gateway_core::accounting::Decimal>() else {
        return Some(total_only_billing_view(total, currency));
    };
    if breakdown.total_amount().amount() != persisted_total
        || breakdown.total_amount().currency().as_str() != currency
    {
        return Some(total_only_billing_view(total, currency));
    }

    Some(BillingView {
        input_amount_display: format_money(breakdown.input_amount()),
        output_amount_display: format_money(breakdown.output_amount()),
        cache_read_amount_display: format_money(breakdown.cache_read_amount()),
        cache_write_amount_display: format_money(breakdown.cache_write_amount()),
        standard_amount_display: format_money(breakdown.standard_amount()),
        total_amount_display: format_decimal_currency(total, currency),
        input_price_display: format_token_price(breakdown.input_price_per_million()),
        output_price_display: format_token_price(breakdown.output_price_per_million()),
        cache_read_price_display: format_token_price(breakdown.cache_read_price_per_million()),
        cache_write_price_display: format_token_price(breakdown.cache_write_price_per_million()),
        service_tier_display: format_service_tier(breakdown.service_tier()),
        multiplier_display: format!("{:.2}x", f64::from(breakdown.multiplier_percent()) / 100.0),
    })
}

fn calculated_billing_breakdown(
    record: &UsageRecord,
) -> Option<gateway_core::accounting::CalculatedCostBreakdown> {
    let model = record.upstream_model_id.as_deref()?;
    let input = record.input_tokens?;
    let output = record.output_tokens?;
    let cached = record.cached_tokens?;
    match record.provider_kind.as_deref()? {
        "xai" => provider_xai::grok_billing_breakdown(model, input, output, cached),
        "openai" => {
            let cache_write = record.cache_write_tokens?;
            [None, Some("fast"), Some("flex")]
                .into_iter()
                .filter_map(|tier| {
                    provider_openai::openai_billing_breakdown(
                        model,
                        input,
                        output,
                        cached,
                        cache_write,
                        tier,
                    )
                })
                .find(|breakdown| {
                    record.cost_amount.as_ref().is_some_and(|total| {
                        total
                            .as_str()
                            .parse::<gateway_core::accounting::Decimal>()
                            .is_ok_and(|persisted| breakdown.total_amount().amount() == persisted)
                    })
                })
        }
        _ => None,
    }
}

fn total_only_billing_view(total: &DecimalAmount, currency: &str) -> BillingView {
    BillingView {
        input_amount_display: "—".to_owned(),
        output_amount_display: "—".to_owned(),
        cache_read_amount_display: "—".to_owned(),
        cache_write_amount_display: "—".to_owned(),
        standard_amount_display: "—".to_owned(),
        total_amount_display: format_decimal_currency(total, currency),
        input_price_display: "—".to_owned(),
        output_price_display: "—".to_owned(),
        cache_read_price_display: "—".to_owned(),
        cache_write_price_display: "—".to_owned(),
        service_tier_display: "—".to_owned(),
        multiplier_display: "—".to_owned(),
    }
}

fn format_money(money: gateway_core::accounting::Money) -> String {
    let currency = money.currency();
    let amount = money.amount().to_string();
    format_decimal_currency_value(&amount, currency.as_str())
}

fn format_decimal_currency(amount: &DecimalAmount, currency: &str) -> String {
    format_decimal_currency_value(amount.as_str(), currency)
}

fn format_decimal_currency_value(amount: &str, currency: &str) -> String {
    if currency == "USD" {
        let value = amount.parse::<f64>().unwrap_or_default();
        let precision = if value != 0.0 && value.abs() < 0.01 {
            4
        } else {
            2
        };
        format!("${value:.precision$}")
    } else {
        format!("{currency} {amount}")
    }
}

fn format_token_price(price: gateway_core::accounting::Money) -> String {
    if price.currency().as_str() != "USD" {
        return format!("{} {} / 1M Token", price.currency(), price.amount());
    }
    let value = price.amount().scaled() as f64 / 10_000_000_000.0;
    format!("${value:.4} / 1M Token")
}

fn format_service_tier(service_tier: Option<&str>) -> String {
    match service_tier {
        Some("priority" | "fast") => "Fast".to_owned(),
        Some("flex") => "Flex".to_owned(),
        Some("default") | None => "Default".to_owned(),
        Some(other) => other.to_owned(),
    }
}

fn usage_detail_view(detail: UsageRecordDetail) -> UsageRecordDetailView {
    UsageRecordDetailView {
        request: usage_record_view(detail.request),
        attempts: detail.attempts.into_iter().map(attempt_view).collect(),
    }
}

fn attempt_view(attempt: UsageAttemptObservation) -> UsageAttemptView {
    let occurred_at = attempt.occurred_at;
    UsageAttemptView {
        id: attempt.id,
        attempt_index: attempt.attempt_index,
        trigger: attempt.source,
        provider: attempt
            .provider_kind
            .unwrap_or_else(|| "unknown".to_owned()),
        provider_instance_id: attempt
            .provider_instance_id
            .unwrap_or_else(|| "unknown".to_owned()),
        model: attempt
            .upstream_model_id
            .unwrap_or_else(|| "unknown".to_owned()),
        transport: attempt
            .upstream_transport
            .unwrap_or_else(|| "unknown".to_owned()),
        send_state: attempt
            .upstream_send_state
            .unwrap_or_else(|| "unknown".to_owned()),
        outcome: attempt.outcome,
        downstream_committed: attempt.downstream_committed,
        status_code: attempt.status_code,
        provider_error_code: attempt.provider_error_code,
        failure_class: attempt.failure_kind,
        cost_estimate_status: attempt
            .cost_source
            .clone()
            .unwrap_or_else(|| "unavailable".to_owned()),
        estimated_cost_amount: attempt.cost_amount.map(|amount| amount.to_string()),
        estimated_cost_currency: attempt.cost_currency,
        input_tokens: attempt.input_tokens,
        output_tokens: attempt.output_tokens,
        cached_tokens: attempt.cached_tokens,
        total_tokens: attempt.total_tokens,
        first_token_ms: None,
        latency_ms: attempt.latency_ms,
        credential_name: attempt.provider_account_ref,
        account_email: None,
        started_at: occurred_at,
        completed_at: Some(occurred_at),
    }
}

fn token_details(record: &UsageRecord) -> TokenDetailsView {
    let input = record.input_tokens;
    let output = record.output_tokens;
    let cached = record.cached_tokens;
    let cache_write = record.cache_write_tokens;
    let reasoning = record.reasoning_tokens;
    let total = record.total_tokens;
    TokenDetailsView {
        input_tokens: input,
        output_tokens: output,
        cached_tokens: cached,
        cache_write_tokens: cache_write,
        reasoning_tokens: reasoning,
        total_tokens: total,
        input_tokens_display: optional_number(input),
        output_tokens_display: optional_number(output),
        cached_tokens_display: optional_compact_number(cached),
        cache_write_tokens_display: optional_compact_number(cache_write),
        reasoning_tokens_display: optional_number(reasoning),
        total_tokens_display: optional_number(total),
    }
}

fn optional_number(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), format_number)
}

fn optional_compact_number(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), format_compact_number)
}

fn trend_view(kind: TrendKind, points: &[RequestMetricPoint]) -> TrendData {
    let summary = trend_summary(kind, points);
    let points = points.iter().map(trend_point_view).collect::<Vec<_>>();
    TrendData {
        kind,
        points,
        summary,
    }
}

fn trend_point_view(point: &RequestMetricPoint) -> TrendPointView {
    let metrics = &point.metrics;
    let latency = average(metrics.latency_sum, metrics.latency_count);
    let first_token_latency = average(
        metrics.first_token_latency_sum,
        metrics.first_token_latency_count,
    );
    let errors = service_failure_count(metrics);
    let success_rate = (metrics.request_count > 0)
        .then(|| metrics.success_count as f64 / metrics.request_count as f64 * 100.0);
    let local = point.bucket_start.with_timezone(&china_offset());
    let label = local.format("%m-%d %H:%M").to_string();
    TrendPointView {
        time: local.format("%H:%M").to_string(),
        bucket: point.bucket_start,
        label,
        requests: format_compact_number(metrics.request_count),
        requests_value: metrics.request_count,
        input_tokens: format_tokens(metrics.input_tokens),
        input_tokens_value: metrics.input_tokens,
        output_tokens: format_tokens(metrics.output_tokens),
        output_tokens_value: metrics.output_tokens,
        cached_tokens: format_tokens(metrics.cached_tokens),
        cached_tokens_value: metrics.cached_tokens,
        cache_hit_rate_value: rate(metrics.cached_tokens, metrics.input_tokens),
        tokens_value: metrics.total_tokens,
        errors: format_compact_number(errors),
        errors_value: errors,
        latency: display_duration(latency),
        latency_value: latency,
        first_token_latency: display_duration(first_token_latency),
        first_token_latency_value: first_token_latency,
        max_latency: display_duration(metrics.max_latency_ms),
        max_latency_value: metrics.max_latency_ms,
        min_latency: display_duration(metrics.min_latency_ms),
        min_latency_value: metrics.min_latency_ms,
        success_rate: success_rate.map_or_else(|| "—".to_owned(), |value| format!("{value:.1}%")),
        success_rate_value: success_rate,
    }
}

fn trend_summary(kind: TrendKind, points: &[RequestMetricPoint]) -> Vec<TrendSummaryView> {
    match kind {
        TrendKind::Usage => vec![
            summary(
                "输入",
                points.iter().map(|point| point.metrics.input_tokens).sum(),
                None,
            ),
            summary(
                "输出",
                points.iter().map(|point| point.metrics.output_tokens).sum(),
                None,
            ),
            summary(
                "缓存",
                points.iter().map(|point| point.metrics.cached_tokens).sum(),
                None,
            ),
        ],
        TrendKind::Latency => {
            let latency_sum = points.iter().map(|point| point.metrics.latency_sum).sum();
            let latency_count = points.iter().map(|point| point.metrics.latency_count).sum();
            let maximum = points
                .iter()
                .filter_map(|point| point.metrics.max_latency_ms)
                .max();
            let minimum = points
                .iter()
                .filter_map(|point| point.metrics.min_latency_ms)
                .min();
            vec![
                summary_duration("平均", average(latency_sum, latency_count)),
                summary_duration("最高", maximum),
                summary_duration("最低", minimum),
            ]
        }
        TrendKind::Errors => {
            let errors = points
                .iter()
                .map(|point| service_failure_count(&point.metrics))
                .sum::<u64>();
            let successes = points
                .iter()
                .map(|point| point.metrics.success_count)
                .sum::<u64>();
            let requests = points
                .iter()
                .map(|point| point.metrics.request_count)
                .sum::<u64>();
            let health_requests = successes.saturating_add(errors);
            let success =
                (health_requests > 0).then(|| successes as f64 / health_requests as f64 * 100.0);
            vec![
                summary("错误数", errors, None),
                TrendSummaryView {
                    label: "成功率".to_owned(),
                    value: "—".to_owned(),
                    ratio: success.map(|value| format!("{value:.1}%")),
                },
                summary("总请求", requests, None),
            ]
        }
    }
}

fn summary(label: &str, value: u64, ratio: Option<String>) -> TrendSummaryView {
    TrendSummaryView {
        label: label.to_owned(),
        value: format_compact_number(value),
        ratio,
    }
}

fn summary_duration(label: &str, value: Option<u64>) -> TrendSummaryView {
    TrendSummaryView {
        label: label.to_owned(),
        value: display_duration(value),
        ratio: None,
    }
}

fn health_timeline_view(points: &[RequestMetricPoint]) -> HealthTimelineView {
    health_timeline_view_at(points, Utc::now())
}

/// 按指定时刻投影固定的中国自然日健康时间线。
#[must_use]
pub fn health_timeline_view_at(
    records: &[RequestMetricPoint],
    now: DateTime<Utc>,
) -> HealthTimelineView {
    let current_slot = china_quarter_hour_start(now);
    let start = china_day_start(now);
    let mut buckets = (0..HEALTH_TIMELINE_SLOTS)
        .map(|index| {
            (
                start + ChronoDuration::minutes(HEALTH_TIMELINE_SLOT_MINUTES * index),
                HealthWindow::default(),
            )
        })
        .collect::<Vec<_>>();
    for record in records {
        if record.bucket_start < start || record.bucket_start > now {
            continue;
        }
        let record_slot = china_quarter_hour_start(record.bucket_start);
        if let Some((_, bucket)) = buckets
            .iter_mut()
            .find(|(bucket_start, _)| *bucket_start == record_slot)
        {
            bucket.success_requests = bucket
                .success_requests
                .saturating_add(record.metrics.success_count);
            bucket.failed_requests = bucket
                .failed_requests
                .saturating_add(service_failure_count(&record.metrics));
            bucket.cancelled_requests = bucket
                .cancelled_requests
                .saturating_add(record.metrics.cancelled_count);
            bucket.incomplete_requests = bucket
                .incomplete_requests
                .saturating_add(record.metrics.incomplete_count);
            bucket.caller_error_requests = bucket
                .caller_error_requests
                .saturating_add(record.metrics.caller_error_count);
        }
    }

    let totals = buckets
        .iter()
        .filter(|(bucket_start, _)| *bucket_start <= current_slot)
        .fold(HealthWindow::default(), |mut totals, (_, bucket)| {
            totals.success_requests = totals
                .success_requests
                .saturating_add(bucket.success_requests);
            totals.failed_requests = totals
                .failed_requests
                .saturating_add(bucket.failed_requests);
            totals.cancelled_requests = totals
                .cancelled_requests
                .saturating_add(bucket.cancelled_requests);
            totals.incomplete_requests = totals
                .incomplete_requests
                .saturating_add(bucket.incomplete_requests);
            totals.caller_error_requests = totals
                .caller_error_requests
                .saturating_add(bucket.caller_error_requests);
            totals
        });
    HealthTimelineView {
        title: "请求健康时间线".to_owned(),
        description: "有效请求可用性".to_owned(),
        reliability_display: format_health_reliability(totals),
        status: health_status(totals, false).to_owned(),
        success_requests: totals.success_requests,
        failed_requests: totals.failed_requests,
        cancelled_requests: totals.cancelled_requests,
        incomplete_requests: totals.incomplete_requests,
        caller_error_requests: totals.caller_error_requests,
        points: buckets
            .into_iter()
            .enumerate()
            .map(|(index, (bucket_start, bucket))| {
                let elapsed_minutes = index as i64 * HEALTH_TIMELINE_SLOT_MINUTES;
                HealthTimelinePointView {
                    time: format!("{:02}:{:02}", elapsed_minutes / 60, elapsed_minutes % 60),
                    status: health_status(bucket, bucket_start > current_slot).to_owned(),
                    reliability_display: format_health_reliability(bucket),
                    success_requests: bucket.success_requests,
                    failed_requests: bucket.failed_requests,
                    cancelled_requests: bucket.cancelled_requests,
                    incomplete_requests: bucket.incomplete_requests,
                    caller_error_requests: bucket.caller_error_requests,
                }
            })
            .collect(),
    }
}

fn health_status(bucket: HealthWindow, is_future: bool) -> &'static str {
    let eligible_requests = bucket
        .success_requests
        .saturating_add(bucket.failed_requests);
    if is_future {
        "future"
    } else if eligible_requests == 0 {
        "no_data"
    } else if bucket.success_requests == 0
        && bucket.failed_requests >= HEALTH_TIMELINE_UNAVAILABLE_FAILURE_THRESHOLD
    {
        "unavailable"
    } else if eligible_requests < HEALTH_TIMELINE_MIN_SAMPLE_SIZE {
        "low_sample"
    } else if health_reliability(bucket)
        .is_some_and(|reliability| reliability < HEALTH_TIMELINE_STABLE_RELIABILITY)
    {
        "unstable"
    } else {
        "stable"
    }
}

fn health_reliability(bucket: HealthWindow) -> Option<f64> {
    let eligible_requests = bucket
        .success_requests
        .saturating_add(bucket.failed_requests);
    (eligible_requests > 0)
        .then(|| bucket.success_requests as f64 / eligible_requests as f64 * 100.0)
}

fn format_health_reliability(bucket: HealthWindow) -> String {
    health_reliability(bucket)
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "-".to_owned())
}

fn service_failure_count(metrics: &RequestMetrics) -> u64 {
    metrics
        .failure_count
        .saturating_sub(metrics.caller_error_count)
}

fn overview_view(
    overview: UsageOverview,
    points: &[RequestMetricPoint],
) -> UsageInsightsOverviewView {
    let requests = &overview.requests;
    let estimated_cost = currency_amount(&overview.attempts.costs, "USD");
    let failed_requests = service_failure_count(requests);
    let health_requests = requests.success_count.saturating_add(failed_requests);
    let health_points = points
        .iter()
        .map(|point| {
            let failed = service_failure_count(&point.metrics);
            let health_requests = point.metrics.success_count.saturating_add(failed);
            OverviewHealthPointView {
                bucket: point.bucket_start,
                label: point
                    .bucket_start
                    .with_timezone(&china_offset())
                    .format("%m-%d %H:%M")
                    .to_string(),
                success_requests: point.metrics.success_count,
                failed_requests: failed,
                cancelled_requests: point.metrics.cancelled_count,
                incomplete_requests: point.metrics.incomplete_count,
                caller_error_requests: point.metrics.caller_error_count,
                error_rate: rate(failed, health_requests),
            }
        })
        .collect();
    let performance_points = points
        .iter()
        .map(|point| OverviewPerformancePointView {
            bucket: point.bucket_start,
            label: point
                .bucket_start
                .with_timezone(&china_offset())
                .format("%m-%d %H:%M")
                .to_string(),
            latency_p50_ms: point
                .metrics
                .latency_percentiles
                .p50_ms
                .map(|value| value.as_f64()),
            latency_p95_ms: point
                .metrics
                .latency_percentiles
                .p95_ms
                .map(|value| value.as_f64()),
            latency_p99_ms: point
                .metrics
                .latency_percentiles
                .p99_ms
                .map(|value| value.as_f64()),
            first_token_p50_ms: point
                .metrics
                .first_token_latency_percentiles
                .p50_ms
                .map(|value| value.as_f64()),
            first_token_p95_ms: point
                .metrics
                .first_token_latency_percentiles
                .p95_ms
                .map(|value| value.as_f64()),
            first_token_p99_ms: point
                .metrics
                .first_token_latency_percentiles
                .p99_ms
                .map(|value| value.as_f64()),
        })
        .collect();
    let cost_points = points
        .iter()
        .map(|point| OverviewCostPointView {
            bucket: point.bucket_start,
            label: point
                .bucket_start
                .with_timezone(&china_offset())
                .format("%m-%d %H:%M")
                .to_string(),
            input_tokens: point.metrics.input_tokens,
            output_tokens: point.metrics.output_tokens,
            cached_tokens: point.metrics.cached_tokens,
            total_tokens: point.metrics.total_tokens,
            estimated_cost: None,
            standard_cost: None,
            cached_token_rate: rate(point.metrics.cached_tokens, point.metrics.input_tokens),
            cache_hit_request_rate: point.metrics.cache_hit_request_rate(),
        })
        .collect();

    UsageInsightsOverviewView {
        granularity: "15m".to_owned(),
        health: OverviewHealthView {
            total_requests: health_requests,
            success_requests: requests.success_count,
            failed_requests,
            cancelled_requests: requests.cancelled_count,
            incomplete_requests: requests.incomplete_count,
            caller_error_requests: requests.caller_error_count,
            success_rate: rate(requests.success_count, health_requests),
            request_change_rate: None,
            success_rate_change: None,
            points: health_points,
        },
        performance: OverviewPerformanceView {
            latency_p50_ms: requests
                .latency_percentiles
                .p50_ms
                .map(|value| value.as_f64()),
            latency_p95_ms: requests
                .latency_percentiles
                .p95_ms
                .map(|value| value.as_f64()),
            latency_p99_ms: requests
                .latency_percentiles
                .p99_ms
                .map(|value| value.as_f64()),
            first_token_p50_ms: requests
                .first_token_latency_percentiles
                .p50_ms
                .map(|value| value.as_f64()),
            first_token_p95_ms: requests
                .first_token_latency_percentiles
                .p95_ms
                .map(|value| value.as_f64()),
            first_token_p99_ms: requests
                .first_token_latency_percentiles
                .p99_ms
                .map(|value| value.as_f64()),
            latency_coverage: rate(requests.latency_count, requests.request_count),
            first_token_coverage: rate(requests.first_token_latency_count, requests.request_count),
            points: performance_points,
        },
        cost: OverviewCostView {
            estimated_cost: estimated_cost.map(ToString::to_string),
            standard_cost: None,
            cost_per_request: None,
            tokens_per_request: if requests.request_count > 0 {
                requests.total_tokens as f64 / requests.request_count as f64
            } else {
                0.0
            },
            cached_token_rate: rate(requests.cached_tokens, requests.input_tokens),
            cache_hit_request_rate: requests.cache_hit_request_rate(),
            input_tokens: requests.input_tokens,
            output_tokens: requests.output_tokens,
            cached_tokens: requests.cached_tokens,
            total_tokens: requests.total_tokens,
            points: cost_points,
            costs: cost_views(&overview.attempts.costs),
            coverage: cost_coverage_view(&overview.attempts),
        },
        attempts: attempt_metrics_view(&overview.attempts),
        providers: overview
            .providers
            .into_iter()
            .map(|provider| ProviderOverviewView {
                provider: provider.provider_kind,
                request_count: provider.request_count,
                attempt_count: provider.attempt_count,
                failure_count: provider.failure_count,
                total_tokens: provider.total_tokens,
            })
            .collect(),
    }
}

fn diagnostic_item_view(item: DiagnosticObservation, total: u64) -> DiagnosticItemView {
    let error_count = item.failure_count;
    DiagnosticItemView {
        name: display_dimension_name(&item.name),
        request_count: item.request_count,
        success_count: item.request_count.saturating_sub(error_count),
        error_count,
        error_rate: rate(error_count, item.request_count),
        request_share: rate(item.request_count, total),
        average_latency_ms: item.average_latency_ms,
        estimated_cost: None,
        attempt_count: item.attempt_count,
        total_tokens: item.total_tokens,
    }
}

fn ops_error_view(error: OpsErrorRecord) -> OpsErrorView {
    let status = error.status_code.map(i64::from);
    OpsErrorView {
        id: error.event_id,
        request_id: error.request_id,
        client_api_key_id: error.client_api_key_ref,
        kind: error.operation.clone(),
        provider: error.provider_kind,
        account_id: error.provider_account_ref,
        route: error.operation,
        model: error.upstream_model_id,
        status_code: status,
        client_status_code: None,
        upstream_status_code: None,
        transport: error.upstream_transport,
        attempt_index: error.attempt_index,
        failure_class: error.failure_kind,
        response_id: error.client_response_id,
        upstream_request_id: error.upstream_request_id,
        latency_ms: error.latency_ms,
        message: error.message,
        metadata: OpsErrorMetadataView {
            source: error.source,
            component: error.component,
            attempt_id: None,
            provider_instance_id: error.provider_instance_id,
            account_label: None,
        },
        created_at: error.occurred_at,
        created_at_display: china_datetime(&error.occurred_at),
    }
}

fn request_metrics_view(metrics: &RequestMetrics) -> RequestMetricsView {
    RequestMetricsView {
        request_count: metrics.request_count,
        success_count: metrics.success_count,
        failure_count: metrics.failure_count,
        cancelled_count: metrics.cancelled_count,
        incomplete_count: metrics.incomplete_count,
        caller_error_count: metrics.caller_error_count,
        input_tokens: metrics.input_tokens,
        output_tokens: metrics.output_tokens,
        cached_tokens: metrics.cached_tokens,
        cache_write_tokens: metrics.cache_write_tokens,
        reasoning_tokens: metrics.reasoning_tokens,
        total_tokens: metrics.total_tokens,
    }
}

fn attempt_metrics_view(metrics: &AttemptMetrics) -> AttemptMetricsView {
    AttemptMetricsView {
        attempt_count: metrics.attempt_count,
        success_count: metrics.success_count,
        failure_count: metrics.failure_count,
        cancelled_count: metrics.cancelled_count,
        incomplete_count: metrics.incomplete_count,
        rate_limited_count: metrics.rate_limited_count,
        auth_failure_count: metrics.auth_failure_count,
        provider5xx_count: metrics.provider_5xx_count,
        cost_coverage: cost_coverage_view(metrics),
        costs: cost_views(&metrics.costs),
    }
}

fn cost_coverage_view(metrics: &AttemptMetrics) -> CostCoverageView {
    CostCoverageView {
        known: metrics
            .cost_coverage
            .provider_reported_count
            .saturating_add(metrics.cost_coverage.calculated_count),
        partial: 0,
        unknown: metrics.cost_coverage.unavailable_count,
        not_billable: 0,
    }
}

fn cost_views(costs: &[CurrencyCostTotal]) -> Vec<CostView> {
    costs
        .iter()
        .map(|cost| CostView {
            currency: cost.currency.clone(),
            estimated_amount: cost.amount.to_string(),
        })
        .collect()
}

fn currency_amount<'a>(
    costs: &'a [CurrencyCostTotal],
    currency: &str,
) -> Option<&'a DecimalAmount> {
    costs
        .iter()
        .find(|cost| cost.currency.eq_ignore_ascii_case(currency))
        .map(|cost| &cost.amount)
}

fn usage_store_query(
    query: &UsageQuery,
) -> Result<(UsageRecordQuery, u32, u16), AdminServiceError> {
    let (page_number, page_size_number) = query.validate_page().map_err(map_wire_error)?;
    query.validate_cursor().map_err(map_wire_error)?;
    let page = ObservabilityPageNumber::new(page_number).map_err(map_invalid_store)?;
    let page_size = ObservabilityPageSize::new(page_size_number).map_err(map_invalid_store)?;
    let cursor = decode_observability_cursor(query.cursor.as_deref())?;
    let range = usage_range(query.start_time.as_deref(), query.end_time.as_deref())?;
    let filter = usage_filter(query)?;
    Ok((
        UsageRecordQuery {
            range,
            filter,
            cursor,
            page,
            page_size,
        },
        page.get(),
        page_size.get(),
    ))
}

fn usage_filter(query: &UsageQuery) -> Result<UsageRecordFilter, AdminServiceError> {
    let kind = non_empty(query.outcome.clone()).or_else(|| {
        non_empty(query.kind.clone()).filter(|value| {
            matches!(
                value.as_str(),
                "running" | "succeeded" | "failed" | "cancelled" | "incomplete"
            )
        })
    });
    Ok(UsageRecordFilter {
        client_api_key_ref: non_empty(query.client_api_key_id.clone()),
        request_id: non_empty(query.request_id.clone()),
        provider_account_ref: non_empty(query.account_id.clone()),
        operation: non_empty(query.route.clone()),
        provider_kind: non_empty(query.provider.clone()),
        model: non_empty(query.model.clone()),
        outcome: kind,
        status_code: parse_wire_status(query.status_code).map_err(map_wire_error)?,
        transport: non_empty(query.transport.clone()),
        attempt_index: parse_wire_attempt_index(query.attempt_index).map_err(map_wire_error)?,
        response_id: non_empty(query.response_id.clone()),
        upstream_request_id: non_empty(query.upstream_request_id.clone()),
        search: non_empty(query.search.clone()),
    })
}

fn decode_cursor<T: DeserializeOwned>(encoded: &str) -> Result<T, AdminServiceError> {
    if encoded.is_empty() || encoded.len() > MAX_CURSOR_BYTES {
        return Err(AdminServiceError::invalid("Invalid observability cursor"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| AdminServiceError::invalid("Invalid observability cursor"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| AdminServiceError::invalid("Invalid observability cursor"))
}

fn encode_cursor<T: Serialize>(cursor: &T) -> Result<String, AdminServiceError> {
    let bytes = serde_json::to_vec(cursor)
        .map_err(|_| AdminServiceError::internal("Failed to encode observability cursor"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_observability_cursor(
    value: Option<&str>,
) -> Result<Option<ObservabilityCursor>, AdminServiceError> {
    value
        .map(|encoded| {
            let wire: CursorWire = decode_cursor(encoded)?;
            ObservabilityCursor::new(wire.observed_at, wire.stable_id).map_err(map_invalid_store)
        })
        .transpose()
}

fn encode_observability_cursor(cursor: &ObservabilityCursor) -> Result<String, AdminServiceError> {
    encode_cursor(&CursorWire {
        observed_at: cursor.observed_at,
        stable_id: cursor.stable_id.clone(),
    })
}

fn usage_range(
    start: Option<&str>,
    end: Option<&str>,
) -> Result<ObservabilityRange, AdminServiceError> {
    let end = parse_wire_datetime(end)
        .map_err(map_wire_error)?
        .unwrap_or_else(Utc::now);
    let start = parse_wire_datetime(start)
        .map_err(map_wire_error)?
        .unwrap_or(end - ChronoDuration::days(7));
    external_observability_range(start, end)
}

fn dashboard_range(
    start: Option<String>,
    end: Option<String>,
) -> Result<ObservabilityRange, AdminServiceError> {
    let end = parse_wire_datetime(end.as_deref())
        .map_err(map_wire_error)?
        .unwrap_or_else(Utc::now);
    let start = parse_wire_datetime(start.as_deref())
        .map_err(map_wire_error)?
        .unwrap_or_else(|| china_day_start(end) - ChronoDuration::days(1));
    external_observability_range(start, end)
}

fn dashboard_today_range(
    start: Option<String>,
    end: Option<String>,
) -> Result<ObservabilityRange, AdminServiceError> {
    let end = parse_wire_datetime(end.as_deref())
        .map_err(map_wire_error)?
        .unwrap_or_else(Utc::now);
    let start = parse_wire_datetime(start.as_deref())
        .map_err(map_wire_error)?
        .unwrap_or_else(|| china_day_start(end));
    external_observability_range(start, end)
}

/// 外部管理查询最多覆盖 366 天；内部账号留存投影不经过此边界。
pub fn external_observability_range(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<ObservabilityRange, AdminServiceError> {
    let duration = end.signed_duration_since(start);
    if duration <= ChronoDuration::zero() || duration > ChronoDuration::days(366) {
        return Err(AdminServiceError::invalid(
            "Invalid observability time range",
        ));
    }
    ObservabilityRange::new(start, end).map_err(map_invalid_store)
}

fn map_invalid_store(error: StoreError) -> AdminServiceError {
    map_store_error(error, "Observability query")
}

fn map_wire_error(error: WireValidationError) -> AdminServiceError {
    match error.field() {
        "timeRange" => AdminServiceError::invalid("Invalid time range"),
        "statusCode" => AdminServiceError::invalid("Status code must be between 100 and 599"),
        "attemptIndex" => AdminServiceError::invalid("Attempt index is out of range"),
        "kind" => AdminServiceError::invalid("Invalid dashboard trend kind"),
        "dimension" => AdminServiceError::invalid("Invalid diagnostics dimension"),
        "id" => AdminServiceError::invalid("Usage record ID is required"),
        "page" | "pageSize" => AdminServiceError::invalid("Invalid Observability query"),
        "cursor" => AdminServiceError::invalid("Invalid observability cursor"),
        _ => AdminServiceError::invalid("Invalid observability query"),
    }
}

fn map_diagnostic_dimension(
    dimension: WireDiagnosticDimension,
) -> (StoreDiagnosticDimension, &'static str) {
    match dimension {
        WireDiagnosticDimension::Model => (StoreDiagnosticDimension::Model, "model"),
        WireDiagnosticDimension::Account => (StoreDiagnosticDimension::Account, "account"),
        WireDiagnosticDimension::ApiKey => (StoreDiagnosticDimension::ApiKey, "apiKey"),
        WireDiagnosticDimension::Provider => (StoreDiagnosticDimension::Provider, "provider"),
        WireDiagnosticDimension::Transport => (StoreDiagnosticDimension::Transport, "transport"),
        WireDiagnosticDimension::Failure => (StoreDiagnosticDimension::Failure, "failureClass"),
        WireDiagnosticDimension::Status => (StoreDiagnosticDimension::Status, "status"),
    }
}

fn map_store_error(error: StoreError, resource: &'static str) -> AdminServiceError {
    match error {
        StoreError::NotFound { .. } => {
            AdminServiceError::not_found(format!("{resource} not found"))
        }
        StoreError::InvalidData { .. } => AdminServiceError::invalid(format!("Invalid {resource}")),
        StoreError::Conflict { .. } => AdminServiceError::conflict(format!("{resource} conflict")),
        StoreError::Unavailable { .. } => {
            tracing::error!(error = %error, resource, "Observability repository unavailable");
            AdminServiceError::unavailable("Observability repository unavailable")
        }
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn average(sum: u64, count: u64) -> Option<u64> {
    (count > 0).then(|| sum / count)
}

fn rate(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn optional_rate(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then(|| rate(numerator, denominator))
}

fn display_rate(value: f64) -> String {
    if value.is_finite() {
        format!("{:.1}%", value * 100.0)
    } else {
        "—".to_owned()
    }
}

fn display_duration(value: Option<u64>) -> String {
    format_duration_ms(value.and_then(|value| i64::try_from(value).ok()))
}

fn china_offset() -> FixedOffset {
    FixedOffset::east_opt(8 * 60 * 60).expect("China offset is valid")
}

fn china_rfc3339(value: &DateTime<Utc>) -> String {
    value.with_timezone(&china_offset()).to_rfc3339()
}

fn china_datetime(value: &DateTime<Utc>) -> String {
    value
        .with_timezone(&china_offset())
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

fn china_day_start(value: DateTime<Utc>) -> DateTime<Utc> {
    let offset = china_offset();
    let local = value.with_timezone(&offset);
    let local_start = local
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is valid");
    offset
        .from_local_datetime(&local_start)
        .single()
        .expect("fixed offset has one local time")
        .with_timezone(&Utc)
}

fn china_quarter_hour_start(value: DateTime<Utc>) -> DateTime<Utc> {
    let offset = china_offset();
    let local = value.with_timezone(&offset);
    let local_start = local
        .date_naive()
        .and_hms_opt(local.hour(), local.minute() / 15 * 15, 0)
        .expect("quarter hour is valid");
    offset
        .from_local_datetime(&local_start)
        .single()
        .expect("fixed offset has one local time")
        .with_timezone(&Utc)
}

fn sum_points(
    points: &[RequestMetricPoint],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> RequestMetrics {
    let mut total = RequestMetrics::default();
    for metrics in points
        .iter()
        .filter(|point| point.bucket_start >= start && point.bucket_start < end)
        .map(|point| &point.metrics)
    {
        total.request_count = total.request_count.saturating_add(metrics.request_count);
        total.success_count = total.success_count.saturating_add(metrics.success_count);
        total.failure_count = total.failure_count.saturating_add(metrics.failure_count);
        total.input_tokens = total.input_tokens.saturating_add(metrics.input_tokens);
        total.output_tokens = total.output_tokens.saturating_add(metrics.output_tokens);
        total.cached_tokens = total.cached_tokens.saturating_add(metrics.cached_tokens);
        total.total_tokens = total.total_tokens.saturating_add(metrics.total_tokens);
    }
    total
}

fn display_dimension_name(value: &str) -> String {
    match value {
        "__none__" => "未知".to_owned(),
        value => value.to_owned(),
    }
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

use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_api::admin::{
    AdminServiceError,
    auth::{
        AdminAuthAuditEvent, AdminAuthBackend, AdminAuthService, AdminBackendSession,
        AdminRequestContext, AdminSessionResolver, DefaultAdminAuthService,
    },
};
use gateway_store::postgres::{
    AdminAuditActorKind, AdminAuditEvent, AdminSecurityAuditRepository, RuntimeSettingsRepository,
};
use gateway_store::{ConflictKind, Revision, StoreError};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

/// Admin auth domain 与两个 Store owner 之间的组合 adapter。
pub struct StoreAdminAuthBackend {
    security: Arc<dyn AdminSecurityAuditRepository>,
    runtime_settings: Arc<dyn RuntimeSettingsRepository>,
    runtime_auth: Arc<dyn gateway_store::redis::AdminAuthStateRepository>,
}

impl StoreAdminAuthBackend {
    #[must_use]
    pub const fn new(
        security: Arc<dyn AdminSecurityAuditRepository>,
        runtime_settings: Arc<dyn RuntimeSettingsRepository>,
        runtime_auth: Arc<dyn gateway_store::redis::AdminAuthStateRepository>,
    ) -> Self {
        Self {
            security,
            runtime_settings,
            runtime_auth,
        }
    }
}

#[async_trait]
impl AdminAuthBackend for StoreAdminAuthBackend {
    async fn password_hash(
        &self,
        admin_user_id: &str,
    ) -> Result<Option<String>, AdminServiceError> {
        self.security
            .password_hash(admin_user_id)
            .await
            .map_err(|_| AdminServiceError::unavailable("Admin security store unavailable"))
    }

    async fn store_password_hash(
        &self,
        admin_user_id: &str,
        password_hash: &str,
    ) -> Result<(), AdminServiceError> {
        self.security
            .replace_password_hash(admin_user_id, password_hash)
            .await
            .map_err(|_| AdminServiceError::unavailable("Admin security store unavailable"))
    }

    async fn admin_api_key(&self) -> Result<Option<String>, AdminServiceError> {
        self.runtime_settings
            .load_runtime_settings()
            .await
            .map(|settings| settings.admin_api_key)
            .map_err(|_| AdminServiceError::unavailable("Runtime settings unavailable"))
    }

    async fn load_admin_session(
        &self,
        session_id: &str,
    ) -> Result<Option<AdminBackendSession>, AdminServiceError> {
        self.runtime_auth
            .load_admin_session(session_id)
            .await
            .map(|session| {
                session.map(|session| AdminBackendSession {
                    admin_user_id: session.admin_user_id,
                    expires_at: session.expires_at,
                })
            })
            .map_err(|_| AdminServiceError::unavailable("Admin session store unavailable"))
    }

    async fn store_admin_session(
        &self,
        session_id: &str,
        session: &AdminBackendSession,
    ) -> Result<(), AdminServiceError> {
        self.runtime_auth
            .store_admin_session(
                session_id,
                &gateway_store::redis::AdminSessionRecord {
                    admin_user_id: session.admin_user_id.clone(),
                    expires_at: session.expires_at,
                },
            )
            .await
            .map_err(|_| AdminServiceError::unavailable("Admin session store unavailable"))
    }

    async fn delete_admin_session(
        &self,
        session_id: &str,
    ) -> Result<Option<AdminBackendSession>, AdminServiceError> {
        self.runtime_auth
            .delete_admin_session(session_id)
            .await
            .map(|session| {
                session.map(|session| AdminBackendSession {
                    admin_user_id: session.admin_user_id,
                    expires_at: session.expires_at,
                })
            })
            .map_err(|_| AdminServiceError::unavailable("Admin session store unavailable"))
    }

    async fn login_source_is_throttled(
        &self,
        source: &str,
        failure_limit: u32,
        window_seconds: u64,
    ) -> Result<bool, AdminServiceError> {
        self.runtime_auth
            .login_source_is_throttled(source, failure_limit, window_seconds)
            .await
            .map_err(|_| AdminServiceError::unavailable("Admin login state unavailable"))
    }

    async fn record_login_failure(
        &self,
        source: &str,
        failure_limit: u32,
        window_seconds: u64,
    ) -> Result<bool, AdminServiceError> {
        self.runtime_auth
            .record_login_failure(source, failure_limit, window_seconds)
            .await
            .map_err(|_| AdminServiceError::unavailable("Admin login state unavailable"))
    }

    async fn clear_login_failures(&self, source: &str) -> Result<(), AdminServiceError> {
        self.runtime_auth
            .clear_login_failures(source)
            .await
            .map_err(|_| AdminServiceError::unavailable("Admin login state unavailable"))
    }

    async fn append_auth_audit(&self, event: AdminAuthAuditEvent) -> Result<(), AdminServiceError> {
        self.security
            .append_admin_audit_event(AdminAuditEvent {
                id: format!("audit_{}", Uuid::now_v7().simple()),
                actor_kind: AdminAuditActorKind::AdminSession,
                actor_admin_user_id: Some(event.admin_user_id.clone()),
                actor_ref: format!("admin:{}", event.admin_user_id),
                admin_request_id: None,
                action: event.action.to_owned(),
                entity_kind: "admin_session".to_owned(),
                entity_ref: event.admin_user_id,
                config_revision: None,
                changed_fields: Vec::new(),
                created_at: event.occurred_at,
            })
            .await
            .map_err(|_| AdminServiceError::unavailable("Admin audit store unavailable"))
    }
}

pub type AdminSessionService = DefaultAdminAuthService;
fn audit_actor(context: &AdminRequestContext) -> (AdminAuditActorKind, Option<String>, String) {
    if let Some(admin_user_id) = context.admin_user_id() {
        (
            AdminAuditActorKind::AdminSession,
            Some(admin_user_id.to_owned()),
            format!("admin:{admin_user_id}"),
        )
    } else {
        (
            AdminAuditActorKind::AdminApiKey,
            None,
            "admin_api_key".to_owned(),
        )
    }
}

fn admin_audit_event(
    context: &AdminRequestContext,
    action: &'static str,
    entity_kind: &'static str,
    entity_ref: String,
    changed_fields: &[&str],
) -> AdminAuditEvent {
    let (actor_kind, actor_admin_user_id, actor_ref) = audit_actor(context);
    AdminAuditEvent {
        id: format!("audit_{}", Uuid::now_v7().simple()),
        actor_kind,
        actor_admin_user_id,
        actor_ref,
        admin_request_id: Some(context.request_id().to_owned()),
        action: action.to_owned(),
        entity_kind: entity_kind.to_owned(),
        entity_ref,
        config_revision: None,
        changed_fields: changed_fields
            .iter()
            .map(|field| (*field).to_owned())
            .collect(),
        created_at: Utc::now(),
    }
}

fn revision(value: u64) -> Result<Revision, AdminServiceError> {
    Revision::new(value)
        .map_err(|_| AdminServiceError::invalid("expectedConfigRevision must be positive"))
}

fn map_admin_store_error(error: StoreError, entity: &'static str) -> AdminServiceError {
    match error {
        StoreError::NotFound { .. } => {
            AdminServiceError::not_found(format!("{entity} was not found"))
        }
        StoreError::Conflict {
            kind: ConflictKind::StaleRevision,
            ..
        } => AdminServiceError::conflict("Configuration revision is stale; reload and retry"),
        StoreError::Conflict { .. } => {
            AdminServiceError::conflict(format!("{entity} state does not allow this operation"))
        }
        StoreError::InvalidData { .. } => {
            AdminServiceError::invalid(format!("Invalid {entity} configuration"))
        }
        StoreError::Unavailable { .. } => {
            AdminServiceError::unavailable("Configuration repository unavailable")
        }
    }
}

use gateway_api::admin::{
    AdminSessionState,
    accounts::{
        AccountAdminService, AccountAdminState, AccountConnectionTestEvent,
        AccountConnectionTestEventStream, AccountExportData, AccountModelView, AccountModelsData,
        AccountPageData, AccountQuotaData, AccountQuotaView, AccountQuotaWindowView,
        AccountRefreshData, AccountRefreshRequest, AccountSort, AccountStatus, AccountSummaryView,
        AccountUsageView, AccountView, CurrencyCostView, ModelUsageView, ProviderFilter,
        SortDirection, SortField, ValidatedListQuery,
    },
    auth::AdminAuthState,
    catalog::{CatalogAdminService, CatalogAdminState},
    client_keys::{ClientKeyAdminService, ClientKeyAdminState},
    observability::{ObservabilityAdminService, ObservabilityAdminState},
    openai::{CodexAdminService, CodexAdminState},
    settings::{AdminSettingsService, AdminSettingsState},
    system::{SystemAdminService, SystemAdminState},
    xai::{XaiAdminService, XaiAdminState},
};
use gateway_api::openai::{OpenAiApiState, OpenAiClientService};

/// 旧账号页需要的统一只读目录；Provider 专属动作由后续同一 adapter 分派给 owner。
pub struct AccountAdminAdapter {
    accounts: Arc<dyn gateway_store::postgres::ProviderAccountRepository>,
    admin_accounts: Arc<dyn gateway_store::postgres::ProviderAccountAdminRepository>,
    core_store: Arc<dyn gateway_core::engine::credential::ProviderAccountStore>,
    control_plane: Arc<dyn gateway_store::postgres::ControlPlaneRepository>,
    instances: Arc<dyn gateway_store::postgres::ConfigCatalogRepository>,
    observability: Arc<dyn gateway_store::postgres::ObservabilityRepository>,
    security: Arc<dyn gateway_store::postgres::AdminSecurityAuditRepository>,
    codex_owner: Arc<provider_openai::credential::CodexCredentialAdminService>,
    codex_quota: Arc<provider_openai::credential::CodexCredentialQuotaService>,
    codex_catalog: Arc<provider_openai::credential::CodexCredentialCatalogService>,
    xai_repository: provider_xai::GrokCredentialRepository,
    xai_refresh: Arc<provider_xai::GrokCredentialRefreshService>,
    xai_quota: Arc<provider_xai::GrokCredentialQuotaService>,
    xai_catalog: Arc<provider_xai::GrokCredentialCatalogService>,
    connection_test: Arc<GatewayOpenAiService>,
    publisher: RuntimeSnapshotPublisher,
}

/// [`AccountAdminAdapter`] 的显式 owner 组合，避免位置参数错接。
pub struct AccountAdminPorts {
    pub accounts: Arc<dyn gateway_store::postgres::ProviderAccountRepository>,
    pub admin_accounts: Arc<dyn gateway_store::postgres::ProviderAccountAdminRepository>,
    pub core_store: Arc<dyn gateway_core::engine::credential::ProviderAccountStore>,
    pub control_plane: Arc<dyn gateway_store::postgres::ControlPlaneRepository>,
    pub instances: Arc<dyn gateway_store::postgres::ConfigCatalogRepository>,
    pub observability: Arc<dyn gateway_store::postgres::ObservabilityRepository>,
    pub security: Arc<dyn gateway_store::postgres::AdminSecurityAuditRepository>,
    pub codex_owner: Arc<provider_openai::credential::CodexCredentialAdminService>,
    pub codex_quota: Arc<provider_openai::credential::CodexCredentialQuotaService>,
    pub codex_catalog: Arc<provider_openai::credential::CodexCredentialCatalogService>,
    pub xai_repository: provider_xai::GrokCredentialRepository,
    pub xai_refresh: Arc<provider_xai::GrokCredentialRefreshService>,
    pub xai_quota: Arc<provider_xai::GrokCredentialQuotaService>,
    pub xai_catalog: Arc<provider_xai::GrokCredentialCatalogService>,
    pub connection_test: Arc<GatewayOpenAiService>,
    pub publisher: RuntimeSnapshotPublisher,
}

struct AccountListPresentationInput {
    id: String,
    account: gateway_store::postgres::ProviderAccountSummary,
    instance: gateway_store::postgres::ProviderInstanceRecord,
    rolling_usage: Option<gateway_store::postgres::ProviderAccountUsageObservation>,
}

impl AccountAdminAdapter {
    #[must_use]
    pub fn new(ports: AccountAdminPorts) -> Self {
        Self {
            accounts: ports.accounts,
            admin_accounts: ports.admin_accounts,
            core_store: ports.core_store,
            control_plane: ports.control_plane,
            instances: ports.instances,
            observability: ports.observability,
            security: ports.security,
            codex_owner: ports.codex_owner,
            codex_quota: ports.codex_quota,
            codex_catalog: ports.codex_catalog,
            xai_repository: ports.xai_repository,
            xai_refresh: ports.xai_refresh,
            xai_quota: ports.xai_quota,
            xai_catalog: ports.xai_catalog,
            connection_test: ports.connection_test,
            publisher: ports.publisher,
        }
    }

    async fn load_account(
        &self,
        id: &str,
    ) -> Result<gateway_store::postgres::ProviderAccountRecord, AdminServiceError> {
        self.accounts
            .load_provider_account(id)
            .await
            .map_err(|error| map_admin_store_error(error, "provider account"))?
            .ok_or_else(|| AdminServiceError::not_found("Provider account was not found"))
    }

    async fn load_instance(
        &self,
        id: &str,
    ) -> Result<gateway_store::postgres::ProviderInstanceRecord, AdminServiceError> {
        self.instances
            .get_provider_instance(id)
            .await
            .map_err(|error| map_admin_store_error(error, "Provider instance"))?
            .ok_or_else(|| AdminServiceError::not_found("Provider instance was not found"))
    }

    async fn load_account_usage_by_id(
        &self,
        range: ObservabilityRange,
        account_ids: &[String],
    ) -> Result<
        BTreeMap<String, gateway_store::postgres::ProviderAccountUsageObservation>,
        AdminServiceError,
    > {
        let mut usage_by_account = BTreeMap::new();
        for account_ids in account_ids.chunks(200) {
            let query = gateway_store::postgres::ProviderAccountUsageQuery::for_accounts(
                range,
                account_ids.to_vec(),
            )
            .map_err(|error| map_admin_store_error(error, "account usage"))?;
            let observations = self
                .observability
                .provider_account_usage(query)
                .await
                .map_err(|error| map_admin_store_error(error, "account usage"))?;
            usage_by_account.extend(
                observations
                    .into_iter()
                    .map(|observation| (observation.account_id.clone(), observation)),
            );
        }
        Ok(usage_by_account)
    }

    async fn load_account_view(
        &self,
        id: &str,
        refresh_quota: bool,
    ) -> Result<AccountView, AdminServiceError> {
        let record = self.load_account(id).await?;
        let instance = self
            .load_instance(&record.summary.provider_instance_id)
            .await?;
        let settings = self
            .control_plane
            .load_control_plane()
            .await
            .map_err(|error| map_admin_store_error(error, "runtime settings"))?
            .settings;
        let now = Utc::now();
        let usage_range = ObservabilityRange::new(
            now - ChronoDuration::days(i64::from(settings.usage_retention_days)),
            now,
        )
        .map_err(|error| map_admin_store_error(error, "account usage"))?;
        let rolling_range = ObservabilityRange::new(now - ChronoDuration::hours(24), now)
            .map_err(|error| map_admin_store_error(error, "account usage"))?;
        let account_ids = vec![id.to_owned()];
        let (mut usage_by_account, mut rolling_usage_by_account) = tokio::try_join!(
            self.load_account_usage_by_id(usage_range, &account_ids),
            self.load_account_usage_by_id(rolling_range, &account_ids),
        )?;
        let usage = usage_by_account.remove(id);
        let rolling_usage = rolling_usage_by_account.remove(id);
        let quota = self
            .provider_quota_view(
                &record.summary,
                &instance,
                refresh_quota,
                rolling_usage.as_ref(),
            )
            .await?;
        let refresh_token_expires_at = if record.summary.provider_kind == "xai" {
            let account_id =
                gateway_core::engine::credential::ProviderAccountId::new(id.to_owned())
                    .map_err(|_| AdminServiceError::invalid("Invalid provider account ID"))?;
            self.xai_repository
                .read_lifecycle(&account_id)
                .await
                .map_err(map_xai_repository_error)?
                .refresh_token_expires_at()
                .map(china_rfc3339)
        } else {
            None
        };
        let mut view = account_view(record.summary, instance.name, usage, now);
        view.quota = quota;
        view.refresh_token_expires_at = refresh_token_expires_at;
        Ok(view)
    }

    async fn provider_quota_view(
        &self,
        account: &gateway_store::postgres::ProviderAccountSummary,
        instance: &gateway_store::postgres::ProviderInstanceRecord,
        refresh: bool,
        rolling_usage: Option<&gateway_store::postgres::ProviderAccountUsageObservation>,
    ) -> Result<AccountQuotaView, AdminServiceError> {
        let account_id =
            gateway_core::engine::credential::ProviderAccountId::new(account.id.clone())
                .map_err(|_| AdminServiceError::invalid("Invalid provider account ID"))?;
        match account.provider_kind.as_str() {
            "openai" => {
                let snapshot = if refresh {
                    Some(
                        self.codex_quota
                            .refresh_account(&core_provider_instance(instance)?, &account_id)
                            .await
                            .map_err(map_codex_quota_error)?,
                    )
                } else {
                    self.codex_quota
                        .read_account(&account_id)
                        .await
                        .map_err(map_codex_quota_error)?
                };
                Ok(snapshot.map_or_else(empty_quota_view, codex_quota_view))
            }
            "xai" => {
                let snapshot = if refresh {
                    Some(
                        self.xai_quota
                            .refresh_account(&account_id)
                            .await
                            .map_err(map_xai_quota_error)?,
                    )
                } else {
                    self.xai_quota
                        .read_account(&account_id)
                        .await
                        .map_err(map_xai_quota_error)?
                };
                Ok(snapshot.map_or_else(empty_quota_view, |snapshot| {
                    xai_quota_view(snapshot, rolling_usage)
                }))
            }
            _ => Ok(empty_quota_view()),
        }
    }

    async fn account_models(
        &self,
        id: &str,
        refresh: bool,
    ) -> Result<AccountModelsData, AdminServiceError> {
        let record = self.load_account(id).await?;
        let instance = self
            .load_instance(&record.summary.provider_instance_id)
            .await?;
        let account_id = gateway_core::engine::credential::ProviderAccountId::new(id.to_owned())
            .map_err(|_| AdminServiceError::invalid("Invalid provider account ID"))?;
        let revision = CoreCredentialRevision::new(record.summary.credential_revision.get())
            .map_err(|_| AdminServiceError::invalid("Invalid credential revision"))?;
        let models = match record.summary.provider_kind.as_str() {
            "openai" if refresh => self
                .codex_catalog
                .synchronize_account(&core_provider_instance(&instance)?, &account_id)
                .await
                .map_err(map_codex_catalog_error)?,
            "openai" => self
                .codex_catalog
                .cached_account_models(
                    &gateway_core::routing::ProviderInstanceId::new(instance.id)
                        .map_err(|_| AdminServiceError::invalid("Invalid Provider instance"))?,
                    &account_id,
                )
                .map_err(map_codex_catalog_error)?
                .unwrap_or_default(),
            "xai" if refresh => self
                .xai_catalog
                .refresh_account_catalog(&account_id, provider_xai::transport::GROK_CLIENT_VERSION)
                .await
                .map_err(map_xai_catalog_error)?
                .seed()
                .models()
                .to_vec(),
            "xai" => self
                .xai_catalog
                .read_account_catalog(&account_id, revision)
                .await
                .map_err(map_xai_catalog_error)?
                .map(|catalog| catalog.seed().models().to_vec())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        Ok(AccountModelsData {
            models: models
                .into_iter()
                .map(|id| AccountModelView {
                    label: id.clone(),
                    id,
                })
                .collect(),
        })
    }

    async fn list_presentation(
        &self,
        input: AccountListPresentationInput,
    ) -> Result<(String, AccountQuotaView, Option<String>), AdminServiceError> {
        let quota = self
            .provider_quota_view(
                &input.account,
                &input.instance,
                false,
                input.rolling_usage.as_ref(),
            )
            .await?;
        let refresh_token_expires_at = if input.account.provider_kind == "xai" {
            let account_id =
                gateway_core::engine::credential::ProviderAccountId::new(input.account.id)
                    .map_err(|_| AdminServiceError::invalid("Invalid provider account ID"))?;
            self.xai_repository
                .read_lifecycle(&account_id)
                .await
                .map_err(map_xai_repository_error)?
                .refresh_token_expires_at()
                .map(china_rfc3339)
        } else {
            None
        };
        Ok((input.id, quota, refresh_token_expires_at))
    }
}

enum AccountRefreshGuard {
    Codex(provider_openai::credential::PreparedCodexCredentialRotationGuard),
    Xai(provider_xai::PreparedGrokCredentialRotationGuard),
}

fn core_provider_instance(
    record: &gateway_store::postgres::ProviderInstanceRecord,
) -> Result<gateway_core::routing::ProviderInstance, AdminServiceError> {
    Ok(gateway_core::routing::ProviderInstance::new(
        gateway_core::routing::ProviderInstanceId::new(record.id.clone())
            .map_err(|_| AdminServiceError::invalid("Invalid Provider instance ID"))?,
        gateway_core::routing::ProviderKind::new(record.provider_kind.clone())
            .map_err(|_| AdminServiceError::invalid("Invalid Provider kind"))?,
        record.base_url.clone(),
        record.enabled,
        gateway_core::routing::InstanceHealth::Healthy,
    ))
}

fn empty_quota_view() -> AccountQuotaView {
    AccountQuotaView {
        refreshed_at_display: "—".to_owned(),
        windows: Vec::new(),
    }
}

fn codex_quota_view(
    snapshot: provider_openai::credential::CodexAccountQuotaSnapshot,
) -> AccountQuotaView {
    let now = Utc::now();
    let observed_at = DateTime::<Utc>::from(snapshot.observed_at());
    AccountQuotaView {
        refreshed_at_display: relative_time(Some(observed_at), now),
        windows: snapshot
            .windows()
            .iter()
            .map(|window| {
                let label = codex_quota_window_label(window);
                let used = window.used_percent();
                AccountQuotaWindowView {
                    key: window.key().to_owned(),
                    group: codex_quota_window_group(window.kind()).to_owned(),
                    window_seconds: window.window_seconds(),
                    label_display: label,
                    used_percent: used,
                    used_percent_display: used
                        .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}%")),
                    local_usage: None,
                    reset_at_display: window
                        .reset_at()
                        .map_or_else(|| "—".to_owned(), |value| china_datetime(&value)),
                }
            })
            .collect(),
    }
}

fn codex_quota_window_group(
    kind: provider_openai::credential::CodexQuotaWindowKind,
) -> &'static str {
    match kind {
        provider_openai::credential::CodexQuotaWindowKind::Monthly => "monthly",
        provider_openai::credential::CodexQuotaWindowKind::ShortTerm
        | provider_openai::credential::CodexQuotaWindowKind::Weekly => "shortTerm",
        provider_openai::credential::CodexQuotaWindowKind::Other => "other",
    }
}

fn codex_quota_window_label(window: &provider_openai::credential::CodexQuotaWindow) -> String {
    let base = match window.kind() {
        provider_openai::credential::CodexQuotaWindowKind::ShortTerm => "5小时限额".to_owned(),
        provider_openai::credential::CodexQuotaWindowKind::Weekly => "周限额".to_owned(),
        provider_openai::credential::CodexQuotaWindowKind::Monthly => "月限额".to_owned(),
        provider_openai::credential::CodexQuotaWindowKind::Other => {
            custom_quota_window_label(window.window_seconds())
        }
    };
    let source = window.source();
    if matches!(source, "core" | "spend_control" | "monthly_limit") {
        return base;
    }
    let source = if is_codex_review_limit(source) {
        "代码审查"
    } else {
        source
    };
    format!("{source} · {base}")
}

fn custom_quota_window_label(window_seconds: Option<u64>) -> String {
    let Some(seconds) = window_seconds.filter(|seconds| *seconds > 0) else {
        return "额度".to_owned();
    };
    if seconds % 86_400 == 0 {
        format!("{}天限额", seconds / 86_400)
    } else if seconds % 3_600 == 0 {
        format!("{}小时限额", seconds / 3_600)
    } else {
        format!("{}分钟限额", seconds.div_ceil(60))
    }
}

fn is_codex_review_limit(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    matches!(
        normalized.as_str(),
        "review" | "code_review" | "codex_review" | "codex_code_review"
    ) || normalized.contains("code_review")
        || normalized.contains("codex_review")
}

fn xai_quota_view(
    snapshot: provider_xai::GrokQuotaSnapshot,
    rolling_usage: Option<&gateway_store::postgres::ProviderAccountUsageObservation>,
) -> AccountQuotaView {
    let billing = snapshot.billing();
    if !billing.has_authoritative_quota() {
        return xai_free_quota_view(snapshot.observed_at(), rolling_usage);
    }
    let used = billing.used_percent();
    let period = billing.period_type().unwrap_or("billing");
    let window_seconds = xai_quota_window_seconds(billing.period_start(), billing.period_end());
    AccountQuotaView {
        refreshed_at_display: relative_time(Some(snapshot.observed_at()), Utc::now()),
        windows: vec![AccountQuotaWindowView {
            key: period.to_owned(),
            group: xai_quota_window_group(billing.period_kind()).to_owned(),
            window_seconds,
            label_display: xai_quota_window_label(billing.period_kind(), window_seconds),
            used_percent: used,
            used_percent_display: used
                .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}%")),
            local_usage: Some(serde_json::json!({
                "periodStart": billing.period_start(),
                "periodEnd": billing.period_end(),
                "monthlyLimitCents": billing.monthly_limit_cents(),
                "includedUsedCents": billing.included_used_cents(),
                "onDemandCapCents": billing.on_demand_cap_cents(),
                "onDemandUsedCents": billing.on_demand_used_cents(),
                "prepaidBalanceCents": billing.prepaid_balance_cents(),
            })),
            reset_at_display: billing
                .period_end()
                .and_then(parse_rfc3339_utc)
                .map_or_else(|| "—".to_owned(), |value| china_datetime(&value)),
        }],
    }
}

fn xai_free_quota_view(
    observed_at: DateTime<Utc>,
    rolling_usage: Option<&gateway_store::postgres::ProviderAccountUsageObservation>,
) -> AccountQuotaView {
    let local_usage = rolling_usage.map(|usage| {
        let total_tokens = usage.total_tokens.unwrap_or(0);
        serde_json::json!({
            "requestCount": usage.request_count,
            "requestCountDisplay": format_number(usage.request_count),
            "inputTokens": usage.input_tokens.unwrap_or(0),
            "inputTokensDisplay": display_optional_tokens(usage.input_tokens),
            "outputTokens": usage.output_tokens.unwrap_or(0),
            "outputTokensDisplay": display_optional_tokens(usage.output_tokens),
            "cachedTokens": usage.cached_tokens.unwrap_or(0),
            "cachedTokensDisplay": display_optional_tokens(usage.cached_tokens),
            "totalTokens": total_tokens,
            "totalTokensDisplay": format_tokens(total_tokens),
        })
    });
    AccountQuotaView {
        refreshed_at_display: relative_time(Some(observed_at), Utc::now()),
        windows: vec![AccountQuotaWindowView {
            key: "free-rolling-24h".to_owned(),
            group: "shortTerm".to_owned(),
            window_seconds: Some(provider_xai::GROK_FREE_ROLLING_WINDOW_SECONDS),
            label_display: "24小时额度".to_owned(),
            used_percent: None,
            used_percent_display: "—".to_owned(),
            local_usage,
            reset_at_display: "—".to_owned(),
        }],
    }
}

fn xai_quota_window_group(kind: provider_xai::GrokQuotaPeriodKind) -> &'static str {
    match kind {
        provider_xai::GrokQuotaPeriodKind::Weekly => "shortTerm",
        provider_xai::GrokQuotaPeriodKind::Monthly => "monthly",
        provider_xai::GrokQuotaPeriodKind::Other => "other",
    }
}

fn xai_quota_window_label(
    kind: provider_xai::GrokQuotaPeriodKind,
    window_seconds: Option<u64>,
) -> String {
    match kind {
        provider_xai::GrokQuotaPeriodKind::Weekly => "周限额".to_owned(),
        provider_xai::GrokQuotaPeriodKind::Monthly => "月限额".to_owned(),
        provider_xai::GrokQuotaPeriodKind::Other => custom_quota_window_label(window_seconds),
    }
}

fn xai_quota_window_seconds(start: Option<&str>, end: Option<&str>) -> Option<u64> {
    let start = start.and_then(parse_rfc3339_utc)?;
    let end = end.and_then(parse_rfc3339_utc)?;
    end.signed_duration_since(start)
        .num_seconds()
        .try_into()
        .ok()
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

fn map_codex_quota_error(
    error: provider_openai::credential::CodexCredentialQuotaError,
) -> AdminServiceError {
    use provider_openai::credential::CodexCredentialQuotaError as Error;
    match error {
        Error::NotFound => AdminServiceError::not_found("Codex account was not found"),
        Error::RevisionConflict => AdminServiceError::conflict("Codex quota snapshot is stale"),
        Error::InvalidInstance | Error::InvalidCredentialData => {
            AdminServiceError::invalid("Codex quota data is invalid")
        }
        Error::TransportInitialization | Error::Repository(_) | Error::Store | Error::Upstream => {
            AdminServiceError::unavailable("Codex quota service is unavailable")
        }
    }
}

fn map_xai_quota_error(error: provider_xai::GrokQuotaError) -> AdminServiceError {
    use provider_xai::GrokQuotaError as Error;
    match error {
        Error::AccountUnavailable => AdminServiceError::not_found("xAI account is unavailable"),
        Error::StaleCredentialSnapshot => {
            AdminServiceError::conflict("xAI quota snapshot is stale")
        }
        Error::InvalidData => AdminServiceError::invalid("xAI quota data is invalid"),
        Error::Upstream | Error::Store => {
            AdminServiceError::unavailable("xAI quota service is unavailable")
        }
    }
}

fn map_codex_catalog_error(
    error: provider_openai::credential::CodexCredentialCatalogError,
) -> AdminServiceError {
    use provider_openai::credential::CodexCredentialCatalogError as Error;
    match error {
        Error::NoEligibleCredential => {
            AdminServiceError::not_found("Codex account has no available model catalog")
        }
        Error::InvalidInstance | Error::InvalidCredentialData | Error::ConflictingModelFacts => {
            AdminServiceError::invalid("Codex model catalog is invalid")
        }
        Error::Upstream | Error::Cache => {
            AdminServiceError::unavailable("Codex model catalog is unavailable")
        }
    }
}

fn map_xai_catalog_error(error: provider_xai::GrokCredentialCatalogError) -> AdminServiceError {
    use provider_xai::GrokCredentialCatalogError as Error;
    match error {
        Error::NoEligibleCredential => {
            AdminServiceError::not_found("xAI account has no available model catalog")
        }
        Error::StaleCredentialSnapshot => {
            AdminServiceError::conflict("xAI model catalog snapshot is stale")
        }
        Error::InvalidInstance | Error::InvalidCredentialData | Error::ConflictingModelFacts => {
            AdminServiceError::invalid("xAI model catalog is invalid")
        }
        Error::Upstream | Error::Cache | Error::Store => {
            AdminServiceError::unavailable("xAI model catalog is unavailable")
        }
    }
}

fn map_xai_refresh_error(error: provider_xai::GrokCredentialRefreshError) -> AdminServiceError {
    use provider_xai::GrokCredentialRefreshError as Error;
    match error {
        Error::InvalidConfiguration | Error::InvalidRefreshResponse => {
            AdminServiceError::invalid("xAI credential refresh data is invalid")
        }
        Error::ManualFailure(provider_xai::GrokRefreshFailure::InvalidGrant) => {
            AdminServiceError::invalid("xAI refresh token is expired")
        }
        Error::Repository(provider_xai::GrokCredentialRepositoryError::CredentialNotFound) => {
            AdminServiceError::not_found("xAI account was not found")
        }
        Error::Repository(provider_xai::GrokCredentialRepositoryError::StaleCredentialRevision) => {
            AdminServiceError::conflict("xAI credential revision is stale")
        }
        _ => AdminServiceError::unavailable("xAI credential refresh is unavailable"),
    }
}

#[async_trait]
impl AccountAdminService for AccountAdminAdapter {
    async fn list(&self, query: ValidatedListQuery) -> Result<AccountPageData, AdminServiceError> {
        let (control_plane, accounts, instances) = tokio::try_join!(
            self.control_plane.load_control_plane(),
            self.accounts.list_provider_accounts(None, true),
            self.instances.list_provider_instances(true),
        )
        .map_err(|error| map_admin_store_error(error, "account catalog"))?;
        let instances_by_id = instances
            .into_iter()
            .map(|instance| (instance.id.clone(), instance))
            .collect::<BTreeMap<_, _>>();
        let now = Utc::now();
        let usage_range = ObservabilityRange::new(
            now - ChronoDuration::days(i64::from(control_plane.settings.usage_retention_days)),
            now,
        )
        .map_err(|error| map_admin_store_error(error, "account usage"))?;
        let rolling_range = ObservabilityRange::new(now - ChronoDuration::hours(24), now)
            .map_err(|error| map_admin_store_error(error, "account usage"))?;
        let account_ids = accounts
            .iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>();
        let (mut usage_by_account, mut rolling_usage_by_account) = tokio::try_join!(
            self.load_account_usage_by_id(usage_range, &account_ids),
            self.load_account_usage_by_id(rolling_range, &account_ids),
        )?;
        let accounts_by_id = accounts
            .iter()
            .cloned()
            .map(|account| (account.id.clone(), account))
            .collect::<BTreeMap<_, _>>();
        let summary = account_summary(&accounts, now);
        let mut items = accounts
            .into_iter()
            .filter(|account| account_matches_query(account, &query, now))
            .map(|account| {
                let instance_name = instances_by_id
                    .get(&account.provider_instance_id)
                    .map(|instance| instance.name.clone())
                    .unwrap_or_else(|| account.provider_instance_id.clone());
                let usage = usage_by_account.remove(&account.id);
                account_view(account, instance_name, usage, now)
            })
            .collect::<Vec<_>>();
        sort_account_views(&mut items, query.sort);
        let total = u64::try_from(items.len()).unwrap_or(u64::MAX);
        let page_size = usize::try_from(query.page_size).unwrap_or(usize::MAX);
        let offset = usize::try_from(query.page.saturating_sub(1))
            .unwrap_or(usize::MAX)
            .saturating_mul(page_size);
        let mut items = items
            .into_iter()
            .skip(offset)
            .take(page_size)
            .collect::<Vec<_>>();
        let presentation_inputs = items
            .iter()
            .map(|view| {
                let account = accounts_by_id.get(&view.id).cloned().ok_or_else(|| {
                    AdminServiceError::invalid("Provider account presentation is missing")
                })?;
                let instance = instances_by_id
                    .get(&account.provider_instance_id)
                    .cloned()
                    .ok_or_else(|| {
                        AdminServiceError::invalid("Provider account instance is missing")
                    })?;
                Ok(AccountListPresentationInput {
                    id: view.id.clone(),
                    account,
                    instance,
                    rolling_usage: rolling_usage_by_account.remove(&view.id),
                })
            })
            .collect::<Result<Vec<_>, AdminServiceError>>()?;
        let presentations = futures::stream::iter(
            presentation_inputs
                .into_iter()
                .map(|input| self.list_presentation(input)),
        )
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;
        let mut presentations_by_id = BTreeMap::new();
        for presentation in presentations {
            let (id, quota, refresh_token_expires_at) = presentation?;
            presentations_by_id.insert(id, (quota, refresh_token_expires_at));
        }
        for item in &mut items {
            if let Some((quota, refresh_token_expires_at)) = presentations_by_id.remove(&item.id) {
                item.quota = quota;
                item.refresh_token_expires_at = refresh_token_expires_at;
            }
        }
        let total_pages = if total == 0 {
            0
        } else {
            u32::try_from(total.div_ceil(u64::from(query.page_size))).unwrap_or(u32::MAX)
        };
        Ok(AccountPageData {
            config_revision: control_plane.settings.config_revision.get(),
            items,
            page: gateway_api::admin::PageMeta::new(
                query.page,
                query.page_size,
                total,
                total_pages,
            ),
            summary,
        })
    }

    async fn export(
        &self,
        context: &AdminRequestContext,
        ids: Vec<String>,
    ) -> Result<AccountExportData, AdminServiceError> {
        let mut codex = Vec::new();
        let mut xai = Vec::new();
        for id in &ids {
            let record = self
                .accounts
                .load_provider_account(id)
                .await
                .map_err(|error| map_admin_store_error(error, "provider account"))?
                .ok_or_else(|| AdminServiceError::not_found("Provider account was not found"))?;
            let account_id =
                gateway_core::engine::credential::ProviderAccountId::new(id.clone())
                    .map_err(|_| AdminServiceError::invalid("Invalid provider account ID"))?;
            let credential_revision =
                CoreCredentialRevision::new(record.summary.credential_revision.get())
                    .map_err(|_| AdminServiceError::invalid("Invalid credential revision"))?;
            let loaded = self
                .core_store
                .load_credential(&account_id, credential_revision)
                .await
                .map_err(|_| AdminServiceError::conflict("Credential revision is stale"))?;
            match record.summary.provider_kind.as_str() {
                "openai" => codex.push(provider_openai::credential::ExportManagedCodexCredential {
                    current: loaded,
                    added_at: record.summary.created_at,
                    updated_at: record.summary.updated_at,
                }),
                "xai" => xai.push(loaded),
                _ => {
                    return Err(AdminServiceError::invalid(
                        "Provider does not support account export",
                    ));
                }
            }
        }
        let mut documents = Vec::new();
        if !codex.is_empty() {
            let document = provider_openai::credential::CodexCredentialAdmin
                .format_cpr_export(codex)
                .and_then(provider_openai::credential::CodexCprExportDocument::into_json)
                .map_err(map_codex_admin_error)?;
            documents.push(serde_json::json!({"provider": "openai", "document": document}));
        }
        if !xai.is_empty() {
            let document = provider_xai::GrokCredentialAdmin
                .export_oauth_bundle(&xai, Utc::now())
                .map_err(map_xai_repository_error)?
                .into_value();
            documents.push(serde_json::json!({"provider": "xai", "document": document}));
        }
        self.security
            .append_admin_audit_event(admin_audit_event(
                context,
                "export_sensitive",
                "provider_account",
                format!("{} accounts", ids.len()),
                &[],
            ))
            .await
            .map_err(|error| map_admin_store_error(error, "account export audit"))?;
        Ok(AccountExportData::new(serde_json::json!({
            "exportedAt": Utc::now().to_rfc3339(),
            "documents": documents,
        })))
    }

    async fn refresh(
        &self,
        context: &AdminRequestContext,
        request: AccountRefreshRequest,
    ) -> Result<AccountRefreshData, AdminServiceError> {
        let record = self.load_account(&request.id).await?;
        let account_id =
            gateway_core::engine::credential::ProviderAccountId::new(request.id.clone())
                .map_err(|_| AdminServiceError::invalid("Invalid provider account ID"))?;
        let credential_revision =
            CoreCredentialRevision::new(record.summary.credential_revision.get())
                .map_err(|_| AdminServiceError::invalid("Invalid credential revision"))?;
        let (profile, credential, guard) = match record.summary.provider_kind.as_str() {
            "openai" => {
                let (profile, credential, guard) = self
                    .codex_owner
                    .manual_refresh(account_id, credential_revision)
                    .await
                    .map_err(map_codex_admin_error)?
                    .into_parts();
                (profile, credential, AccountRefreshGuard::Codex(guard))
            }
            "xai" => {
                let (profile, credential, guard) = self
                    .xai_refresh
                    .prepare_manual_refresh(&account_id, credential_revision)
                    .await
                    .map_err(map_xai_refresh_error)?
                    .into_parts();
                (profile, credential, AccountRefreshGuard::Xai(guard))
            }
            _ => {
                return Err(AdminServiceError::invalid(
                    "Provider does not support OAuth refresh",
                ));
            }
        };
        let rotation = self
            .admin_accounts
            .rotate_provider_account(
                revision(request.expected_config_revision)?,
                RotateProviderAccount {
                    scope: provider_scope(
                        record.summary.provider_kind,
                        record.summary.provider_instance_id,
                    ),
                    profile: store_profile(profile),
                    credential: store_credential_update(credential)?,
                    audit: admin_audit_event(
                        context,
                        "refresh",
                        "provider_account",
                        request.id.clone(),
                        &["provider_credentials_json", "credential_revision"],
                    ),
                },
            )
            .await
            .map_err(|error| map_admin_store_error(error, "provider account"))?;
        match guard {
            AccountRefreshGuard::Codex(guard) => drop(guard),
            AccountRefreshGuard::Xai(guard) => drop(guard),
        }
        self.publisher
            .publish_committed(rotation.config_revision)
            .await;
        Ok(AccountRefreshData {
            config_revision: rotation.config_revision.get(),
            account: self.load_account_view(&request.id, false).await?,
        })
    }

    async fn quota(&self, id: String) -> Result<AccountQuotaData, AdminServiceError> {
        Ok(AccountQuotaData {
            account: self.load_account_view(&id, false).await?,
        })
    }

    async fn refresh_quota(&self, id: String) -> Result<AccountQuotaData, AdminServiceError> {
        Ok(AccountQuotaData {
            account: self.load_account_view(&id, true).await?,
        })
    }

    async fn models(&self, id: String) -> Result<AccountModelsData, AdminServiceError> {
        self.account_models(&id, false).await
    }

    async fn refresh_models(&self, id: String) -> Result<AccountModelsData, AdminServiceError> {
        self.account_models(&id, true).await
    }

    async fn test_connection(
        &self,
        id: String,
        model_id: String,
    ) -> Result<AccountConnectionTestEventStream, AdminServiceError> {
        let record = self.load_account(&id).await?;
        let initial_account_status =
            account_status_text(account_status(&record.summary, Utc::now()))
                .0
                .to_owned();
        let provider_instance_id =
            gateway_core::routing::ProviderInstanceId::new(record.summary.provider_instance_id)
                .map_err(|_| AdminServiceError::invalid("Invalid Provider instance ID"))?;
        let upstream_model = UpstreamModelId::new(model_id.clone())
            .map_err(|_| AdminServiceError::invalid("Invalid upstream model ID"))?;
        let account_id = gateway_core::engine::credential::ProviderAccountId::new(id.clone())
            .map_err(|_| AdminServiceError::invalid("Invalid provider account ID"))?;
        let initial_events = vec![
            AccountConnectionTestEvent::started(model_id.clone()),
            AccountConnectionTestEvent::request(serde_json::json!({
                "model": model_id,
                "input": [{
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "Reply with exactly OK." }]
                }],
                "stream": true,
                "store": false
            })),
        ];
        let connection_test = Arc::clone(&self.connection_test);
        let accounts = Arc::clone(&self.accounts);
        let terminal_events = futures::stream::once(async move {
            let result = connection_test
                .test_account(account_id, provider_instance_id, upstream_model)
                .await;
            let account_status = accounts
                .load_provider_account(&id)
                .await
                .ok()
                .flatten()
                .map(|account| {
                    account_status_text(account_status(&account.summary, Utc::now()))
                        .0
                        .to_owned()
                })
                .unwrap_or(initial_account_status);
            let mut events = Vec::new();
            match result {
                Ok(content) => {
                    events.extend(content.into_iter().map(AccountConnectionTestEvent::content));
                    events.push(AccountConnectionTestEvent::completed(account_status));
                }
                Err(error) => {
                    let message = match error.kind() {
                        GatewayErrorKind::InvalidRequest
                        | GatewayErrorKind::Unsupported
                        | GatewayErrorKind::ModelNotFound => {
                            AdminServiceError::invalid(error.to_string())
                        }
                        _ => AdminServiceError::unavailable("Provider connection test failed"),
                    };
                    events.push(AccountConnectionTestEvent::failed(
                        message.to_string(),
                        account_status,
                    ));
                }
            }
            events
        })
        .flat_map(futures::stream::iter);
        Ok(Box::pin(
            futures::stream::iter(initial_events).chain(terminal_events),
        ))
    }
}

fn account_matches_query(
    account: &gateway_store::postgres::ProviderAccountSummary,
    query: &ValidatedListQuery,
    now: DateTime<Utc>,
) -> bool {
    let provider_matches = match &query.provider {
        ProviderFilter::All => true,
        ProviderFilter::Provider(provider) => account.provider_kind == provider.as_str(),
    };
    let search_matches = query.search.as_ref().is_none_or(|search| {
        let search = search.to_lowercase();
        [
            Some(account.id.as_str()),
            Some(account.name.as_str()),
            account.email.as_deref(),
            account.upstream_account_id.as_deref(),
            Some(account.upstream_user_id.as_str()),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.to_lowercase().contains(&search))
    });
    let status_matches = query
        .status
        .is_none_or(|status| account_status(account, now) == status);
    provider_matches && search_matches && status_matches
}

fn account_status(
    account: &gateway_store::postgres::ProviderAccountSummary,
    now: DateTime<Utc>,
) -> AccountStatus {
    if !account.enabled {
        AccountStatus::Disabled
    } else if account.availability == ProviderAccountAvailability::Banned {
        AccountStatus::Banned
    } else if account.availability == ProviderAccountAvailability::QuotaExhausted {
        AccountStatus::QuotaExhausted
    } else if account.availability == ProviderAccountAvailability::Expired
        || account.access_token_expires_at <= now
    {
        AccountStatus::Expired
    } else {
        AccountStatus::Active
    }
}

fn account_status_text(status: AccountStatus) -> (&'static str, &'static str) {
    match status {
        AccountStatus::Active => ("active", "正常"),
        AccountStatus::Expired => ("expired", "已过期"),
        AccountStatus::QuotaExhausted => ("quota_exhausted", "额度耗尽"),
        AccountStatus::Disabled => ("disabled", "已禁用"),
        AccountStatus::Banned => ("banned", "已封禁"),
    }
}

fn account_summary(
    accounts: &[gateway_store::postgres::ProviderAccountSummary],
    now: DateTime<Utc>,
) -> AccountSummaryView {
    let total = u64::try_from(accounts.len()).unwrap_or(u64::MAX);
    let active = u64::try_from(
        accounts
            .iter()
            .filter(|account| account_status(account, now) == AccountStatus::Active)
            .count(),
    )
    .unwrap_or(u64::MAX);
    let quota_exhausted = u64::try_from(
        accounts
            .iter()
            .filter(|account| account_status(account, now) == AccountStatus::QuotaExhausted)
            .count(),
    )
    .unwrap_or(u64::MAX);
    AccountSummaryView {
        total,
        active,
        quota_exhausted,
        attention: total.saturating_sub(active),
    }
}

fn account_view(
    account: gateway_store::postgres::ProviderAccountSummary,
    provider_instance_name: String,
    usage: Option<gateway_store::postgres::ProviderAccountUsageObservation>,
    now: DateTime<Utc>,
) -> AccountView {
    let status = account_status(&account, now);
    let (status, _) = account_status_text(status);
    let expires_at = china_rfc3339(&account.access_token_expires_at);
    let added_at = china_rfc3339(&account.created_at);
    let updated_at = china_rfc3339(&account.updated_at);
    AccountView {
        id: account.id.clone(),
        name: account.name,
        provider: account.provider_kind,
        provider_instance_id: account.provider_instance_id,
        provider_instance_name,
        resource_ref: account.id,
        email: account.email,
        account_id: account.upstream_account_id,
        user_id: Some(account.upstream_user_id),
        label: None,
        plan_type: account.plan_type,
        has_refresh_token: account.has_refresh_token,
        status: status.to_owned(),
        display_status: status.to_owned(),
        token_refreshing: false,
        availability: account.availability.as_str().to_owned(),
        enabled: account.enabled,
        credential_revision: account.credential_revision.get(),
        state_revision: None,
        access_token_expires_at: Some(expires_at),
        access_token_expires_at_display: Some(china_datetime(&account.access_token_expires_at)),
        refresh_token_expires_at: None,
        next_refresh_at: account.next_refresh_at.map(|value| china_rfc3339(&value)),
        added_at,
        added_at_display: china_datetime(&account.created_at),
        updated_at,
        updated_at_display: china_datetime(&account.updated_at),
        quota: AccountQuotaView {
            refreshed_at_display: account
                .quota_observed_at
                .map_or_else(|| "—".to_owned(), |value| china_datetime(&value)),
            windows: Vec::new(),
        },
        usage: account_usage_view(usage),
    }
}

fn account_usage_view(
    usage: Option<gateway_store::postgres::ProviderAccountUsageObservation>,
) -> AccountUsageView {
    let Some(usage) = usage else {
        return empty_account_usage();
    };
    let coverage = &usage.cost_coverage;
    let known_count = coverage
        .provider_reported_count
        .saturating_add(coverage.calculated_count);
    let cost_estimate_status = if known_count == 0 {
        "unknown"
    } else if coverage.unavailable_count > 0 {
        "partial"
    } else {
        "known"
    };
    let costs = usage.costs.iter().map(account_currency_cost_view).collect();
    let models = usage
        .models
        .into_iter()
        .map(account_model_usage_view)
        .collect();
    AccountUsageView {
        request_count: Some(usage.request_count),
        request_count_display: format_number(usage.request_count),
        input_tokens: usage.input_tokens,
        input_tokens_display: display_optional_tokens(usage.input_tokens),
        output_tokens: usage.output_tokens,
        output_tokens_display: display_optional_tokens(usage.output_tokens),
        cached_tokens: usage.cached_tokens,
        cached_tokens_display: display_optional_tokens(usage.cached_tokens),
        total_tokens: usage.total_tokens,
        total_tokens_display: display_optional_tokens(usage.total_tokens),
        created_tokens: usage.cache_write_tokens,
        created_tokens_display: display_optional_tokens(usage.cache_write_tokens),
        read_tokens: usage.cached_tokens,
        read_tokens_display: display_optional_tokens(usage.cached_tokens),
        last_used_at: usage.last_used_at.map(|value| china_rfc3339(&value)),
        last_used_at_display: usage.last_used_at.map_or_else(
            || "—".to_owned(),
            |value| relative_time(Some(value), Utc::now()),
        ),
        cost_estimate_status: cost_estimate_status.to_owned(),
        known_cost_count: Some(known_count),
        partial_cost_count: Some(if cost_estimate_status == "partial" {
            1
        } else {
            0
        }),
        unknown_cost_count: Some(coverage.unavailable_count),
        costs,
        models,
    }
}

fn account_model_usage_view(
    usage: gateway_store::postgres::ProviderAccountModelUsageObservation,
) -> ModelUsageView {
    let known_count = usage
        .cost_coverage
        .provider_reported_count
        .saturating_add(usage.cost_coverage.calculated_count);
    let cost_estimate_status = if known_count == 0 {
        "unknown"
    } else if usage.cost_coverage.unavailable_count > 0 {
        "partial"
    } else {
        "known"
    };
    let usd = usage
        .costs
        .iter()
        .find(|cost| cost.currency.eq_ignore_ascii_case("USD"));
    ModelUsageView {
        model: usage.model,
        request_count: usage.request_count,
        request_count_display: format_number(usage.request_count),
        success_rate: (usage.request_count > 0)
            .then(|| usage.success_count as f64 * 100.0 / usage.request_count as f64),
        success_rate_display: if usage.request_count == 0 {
            "—".to_owned()
        } else {
            format!(
                "{:.1}%",
                usage.success_count as f64 * 100.0 / usage.request_count as f64
            )
        },
        input_tokens: usage.input_tokens,
        input_tokens_display: display_optional_tokens(usage.input_tokens),
        output_tokens: usage.output_tokens,
        output_tokens_display: display_optional_tokens(usage.output_tokens),
        cached_tokens: usage.cached_tokens,
        cached_tokens_display: display_optional_tokens(usage.cached_tokens),
        total_tokens: usage.total_tokens,
        total_tokens_display: display_optional_tokens(usage.total_tokens),
        billing_amount_usd: usd.map(|cost| cost.amount.as_str().to_owned()),
        billing_amount_usd_display: usd.map_or_else(
            || "—".to_owned(),
            |cost| format_billing_amount(&cost.amount),
        ),
        cost_estimate_status: cost_estimate_status.to_owned(),
        known_cost_count: known_count,
        partial_cost_count: u64::from(cost_estimate_status == "partial"),
        unknown_cost_count: usage.cost_coverage.unavailable_count,
        costs: usage.costs.iter().map(account_currency_cost_view).collect(),
        last_used_at: china_rfc3339(&usage.last_used_at),
        last_used_at_display: relative_time(Some(usage.last_used_at), Utc::now()),
    }
}

fn account_currency_cost_view(cost: &CurrencyCostTotal) -> CurrencyCostView {
    CurrencyCostView {
        currency: cost.currency.clone(),
        estimated_amount: cost.amount.as_str().to_owned(),
        estimated_amount_display: format!("{} {}", cost.currency, cost.amount.as_str()),
    }
}

fn display_optional_tokens(value: Option<u64>) -> String {
    value.map_or_else(|| "—".to_owned(), format_tokens)
}

fn empty_account_usage() -> AccountUsageView {
    AccountUsageView {
        request_count: None,
        request_count_display: "—".to_owned(),
        input_tokens: None,
        input_tokens_display: "—".to_owned(),
        output_tokens: None,
        output_tokens_display: "—".to_owned(),
        cached_tokens: None,
        cached_tokens_display: "—".to_owned(),
        total_tokens: None,
        total_tokens_display: "—".to_owned(),
        created_tokens: None,
        created_tokens_display: "—".to_owned(),
        read_tokens: None,
        read_tokens_display: "—".to_owned(),
        last_used_at: None,
        last_used_at_display: "—".to_owned(),
        cost_estimate_status: "unavailable".to_owned(),
        known_cost_count: None,
        partial_cost_count: None,
        unknown_cost_count: None,
        costs: Vec::new(),
        models: Vec::new(),
    }
}

fn sort_account_views(items: &mut [AccountView], sort: Option<AccountSort>) {
    let Some(sort) = sort else {
        items.sort_by(|left, right| left.id.cmp(&right.id));
        return;
    };
    items.sort_by(|left, right| {
        let ordering = match sort.field {
            SortField::Email => left.email.cmp(&right.email),
            SortField::Status => left.status.cmp(&right.status),
            SortField::PlanType => left.plan_type.cmp(&right.plan_type),
            SortField::Usage => left.usage.total_tokens.cmp(&right.usage.total_tokens),
            SortField::LastUsedAt => left.usage.last_used_at.cmp(&right.usage.last_used_at),
            SortField::ExpiresAt => left
                .access_token_expires_at
                .cmp(&right.access_token_expires_at),
        }
        .then_with(|| left.id.cmp(&right.id));
        match sort.direction {
            SortDirection::Asc => ordering,
            SortDirection::Desc => ordering.reverse(),
        }
    });
}

/// 管理端各领域端口的只读组合。
pub struct AdminServices {
    sessions: Arc<AdminSessionService>,
    accounts: Arc<dyn AccountAdminService>,
    catalog: Arc<dyn CatalogAdminService>,
    client_keys: Arc<dyn ClientKeyAdminService>,
    codex: Arc<dyn CodexAdminService>,
    observability: Arc<dyn ObservabilityAdminService>,
    settings: Arc<dyn AdminSettingsService>,
    system: Arc<dyn SystemAdminService>,
    xai: Arc<dyn XaiAdminService>,
}

/// [`AdminServices`] 的显式构造参数，避免位置参数错接领域端口。
pub struct AdminServicePorts {
    pub sessions: Arc<AdminSessionService>,
    pub accounts: Arc<dyn AccountAdminService>,
    pub catalog: Arc<dyn CatalogAdminService>,
    pub client_keys: Arc<dyn ClientKeyAdminService>,
    pub codex: Arc<dyn CodexAdminService>,
    pub observability: Arc<dyn ObservabilityAdminService>,
    pub settings: Arc<dyn AdminSettingsService>,
    pub system: Arc<dyn SystemAdminService>,
    pub xai: Arc<dyn XaiAdminService>,
}

impl AdminServices {
    #[must_use]
    pub fn new(ports: AdminServicePorts) -> Self {
        Self {
            sessions: ports.sessions,
            accounts: ports.accounts,
            catalog: ports.catalog,
            client_keys: ports.client_keys,
            codex: ports.codex,
            observability: ports.observability,
            settings: ports.settings,
            system: ports.system,
            xai: ports.xai,
        }
    }
}

use gateway_api::admin::catalog::{
    CatalogListQuery, CatalogMutationData, CatalogRevisionRequest, CreateProviderInstanceRequest,
    ProviderInstanceDetailData, ProviderInstanceListData, ProviderInstanceView,
    UpdateProviderInstanceRequest,
};
use gateway_store::postgres::{
    ConfigCatalogRepository, ControlPlaneReplacement, ControlPlaneRepository, ControlPlaneSnapshot,
    NewProviderInstance, RuntimeSettings, RuntimeSettingsUpdate, UpdateProviderInstanceDetails,
};

/// Provider instance 的 PostgreSQL 管理投影。
pub struct CatalogAdminAdapter {
    control_plane: Arc<dyn ControlPlaneRepository>,
    instances: Arc<dyn ConfigCatalogRepository>,
    publisher: RuntimeSnapshotPublisher,
}

impl CatalogAdminAdapter {
    #[must_use]
    pub const fn new(
        control_plane: Arc<dyn ControlPlaneRepository>,
        instances: Arc<dyn ConfigCatalogRepository>,
        publisher: RuntimeSnapshotPublisher,
    ) -> Self {
        Self {
            control_plane,
            instances,
            publisher,
        }
    }

    async fn revision(&self) -> Result<u64, AdminServiceError> {
        self.control_plane
            .load_control_plane()
            .await
            .map(|snapshot| snapshot.settings.config_revision.get())
            .map_err(|error| map_admin_store_error(error, "catalog"))
    }

    async fn provider_instance_mutation(
        &self,
        context: &AdminRequestContext,
        request: CatalogRevisionRequest,
        enabled: Option<bool>,
    ) -> Result<CatalogMutationData, AdminServiceError> {
        let expected = revision(request.expected_config_revision)?;
        let id = request.id;
        let (action, changed_fields) = match enabled {
            Some(true) => ("enable", &["enabled"][..]),
            Some(false) => ("disable", &["enabled"][..]),
            None => ("delete", &[][..]),
        };
        let audit = admin_audit_event(
            context,
            action,
            "provider_instance",
            id.clone(),
            changed_fields,
        );
        let committed = match enabled {
            Some(enabled) => {
                self.control_plane
                    .set_provider_instance_enabled(expected, &id, enabled, audit)
                    .await
            }
            None => {
                self.control_plane
                    .delete_provider_instance(expected, &id, audit)
                    .await
            }
        }
        .map_err(|error| map_admin_store_error(error, "provider instance"))?;
        self.publisher.publish_committed(committed).await;
        Ok(CatalogMutationData {
            config_revision: committed.get(),
            id,
        })
    }
}

#[async_trait]
impl CatalogAdminService for CatalogAdminAdapter {
    async fn list_provider_instances(
        &self,
        query: CatalogListQuery,
    ) -> Result<ProviderInstanceListData, AdminServiceError> {
        let revision = self.revision().await?;
        let records = self
            .instances
            .list_provider_instances(true)
            .await
            .map_err(|error| map_admin_store_error(error, "provider instance"))?;
        let (records, next_cursor) = paginate_by_id(
            records,
            query.cursor.as_deref(),
            usize::from(query.page_size()),
            |record| &record.id,
        )?;
        Ok(ProviderInstanceListData {
            config_revision: revision,
            items: records.into_iter().map(provider_instance_view).collect(),
            next_cursor,
        })
    }

    async fn provider_instance(
        &self,
        id: String,
    ) -> Result<ProviderInstanceDetailData, AdminServiceError> {
        let revision = self.revision().await?;
        let item = self
            .instances
            .get_provider_instance(&id)
            .await
            .map_err(|error| map_admin_store_error(error, "provider instance"))?
            .ok_or_else(|| AdminServiceError::not_found("Provider instance was not found"))?;
        Ok(ProviderInstanceDetailData {
            config_revision: revision,
            item: provider_instance_view(item),
        })
    }

    async fn create_provider_instance(
        &self,
        context: &AdminRequestContext,
        request: CreateProviderInstanceRequest,
    ) -> Result<CatalogMutationData, AdminServiceError> {
        let expected = revision(request.expected_config_revision)?;
        let id = request.id;
        let committed = self
            .control_plane
            .create_provider_instance(
                expected,
                NewProviderInstance {
                    id: id.clone(),
                    provider_kind: request.provider_kind,
                    name: request.name,
                    base_url: request.base_url,
                },
                admin_audit_event(
                    context,
                    "create",
                    "provider_instance",
                    id.clone(),
                    &["provider_kind", "name", "base_url", "enabled"],
                ),
            )
            .await
            .map_err(|error| map_admin_store_error(error, "provider instance"))?;
        self.publisher.publish_committed(committed).await;
        Ok(CatalogMutationData {
            config_revision: committed.get(),
            id,
        })
    }

    async fn update_provider_instance(
        &self,
        context: &AdminRequestContext,
        request: UpdateProviderInstanceRequest,
    ) -> Result<CatalogMutationData, AdminServiceError> {
        let expected = revision(request.expected_config_revision)?;
        let id = request.id;
        let committed = self
            .control_plane
            .update_provider_instance(
                expected,
                UpdateProviderInstanceDetails {
                    id: id.clone(),
                    name: request.name,
                    base_url: request.base_url,
                },
                admin_audit_event(
                    context,
                    "update",
                    "provider_instance",
                    id.clone(),
                    &["name", "base_url"],
                ),
            )
            .await
            .map_err(|error| map_admin_store_error(error, "provider instance"))?;
        self.publisher.publish_committed(committed).await;
        Ok(CatalogMutationData {
            config_revision: committed.get(),
            id,
        })
    }

    async fn enable_provider_instance(
        &self,
        context: &AdminRequestContext,
        request: CatalogRevisionRequest,
    ) -> Result<CatalogMutationData, AdminServiceError> {
        self.provider_instance_mutation(context, request, Some(true))
            .await
    }

    async fn disable_provider_instance(
        &self,
        context: &AdminRequestContext,
        request: CatalogRevisionRequest,
    ) -> Result<CatalogMutationData, AdminServiceError> {
        self.provider_instance_mutation(context, request, Some(false))
            .await
    }

    async fn delete_provider_instance(
        &self,
        context: &AdminRequestContext,
        request: CatalogRevisionRequest,
    ) -> Result<CatalogMutationData, AdminServiceError> {
        self.provider_instance_mutation(context, request, None)
            .await
    }
}

fn runtime_settings_update(settings: &RuntimeSettings) -> RuntimeSettingsUpdate {
    RuntimeSettingsUpdate {
        admin_api_key: settings.admin_api_key.clone(),
        refresh_margin_seconds: settings.refresh_margin_seconds,
        refresh_concurrency: settings.refresh_concurrency,
        max_concurrent_per_account: settings.max_concurrent_per_account,
        request_interval_ms: settings.request_interval_ms,
        rotation_strategy: settings.rotation_strategy.clone(),
        provider_model_mappings: settings.provider_model_mappings.clone(),
        usage_retention_days: settings.usage_retention_days,
        ops_event_retention_days: settings.ops_event_retention_days,
        audit_retention_days: settings.audit_retention_days,
    }
}

fn provider_instance_view(
    record: gateway_store::postgres::ProviderInstanceRecord,
) -> ProviderInstanceView {
    ProviderInstanceView {
        id: record.id,
        provider_kind: record.provider_kind,
        name: record.name,
        base_url: record.base_url,
        enabled: record.enabled,
        created_at: record.created_at,
        updated_at: record.updated_at,
    }
}

fn paginate_by_id<T, F>(
    items: Vec<T>,
    cursor: Option<&str>,
    limit: usize,
    id: F,
) -> Result<(Vec<T>, Option<String>), AdminServiceError>
where
    F: Fn(&T) -> &str,
{
    let start = match cursor {
        Some(cursor) => items
            .iter()
            .position(|item| id(item) == cursor)
            .map(|index| index + 1)
            .ok_or_else(|| AdminServiceError::invalid("Invalid catalog cursor"))?,
        None => 0,
    };
    let has_more = items.len().saturating_sub(start) > limit;
    let page = items
        .into_iter()
        .skip(start)
        .take(limit)
        .collect::<Vec<_>>();
    let next_cursor = has_more
        .then(|| page.last().map(|item| id(item).to_owned()))
        .flatten();
    Ok((page, next_cursor))
}

use gateway_api::admin::settings::{RuntimeSettingsView, UpdateRuntimeSettingsRequest};
use rand_core::{OsRng, RngCore as _};

/// Runtime settings 与旧设置页只读 route 投影的组合 adapter。
pub struct RuntimeSettingsAdminAdapter {
    control_plane: Arc<dyn ControlPlaneRepository>,
    publisher: RuntimeSnapshotPublisher,
}

impl RuntimeSettingsAdminAdapter {
    #[must_use]
    pub const fn new(
        control_plane: Arc<dyn ControlPlaneRepository>,
        publisher: RuntimeSnapshotPublisher,
    ) -> Self {
        Self {
            control_plane,
            publisher,
        }
    }

    async fn replace_snapshot(
        &self,
        context: &AdminRequestContext,
        _snapshot: ControlPlaneSnapshot,
        expected_revision: Revision,
        settings: RuntimeSettingsUpdate,
        action: &'static str,
        changed_fields: &[&str],
    ) -> Result<ControlPlaneSnapshot, AdminServiceError> {
        let replacement = ControlPlaneReplacement {
            settings,
            audit: admin_audit_event(
                context,
                action,
                "runtime_settings",
                "1".to_owned(),
                changed_fields,
            ),
        };
        let committed = self
            .control_plane
            .replace_control_plane(expected_revision, replacement)
            .await
            .map_err(|error| map_admin_store_error(error, "runtime settings"))?;
        self.publisher
            .publish_committed(committed.settings.config_revision)
            .await;
        Ok(committed)
    }
}

#[async_trait]
impl AdminSettingsService for RuntimeSettingsAdminAdapter {
    async fn load(&self) -> Result<RuntimeSettingsView, AdminServiceError> {
        self.control_plane
            .load_control_plane()
            .await
            .map(settings_view)
            .map_err(|error| map_admin_store_error(error, "runtime settings"))
    }

    async fn replace(
        &self,
        context: &AdminRequestContext,
        request: UpdateRuntimeSettingsRequest,
    ) -> Result<RuntimeSettingsView, AdminServiceError> {
        let expected = revision(request.expected_config_revision)?;
        let snapshot = self
            .control_plane
            .load_control_plane()
            .await
            .map_err(|error| map_admin_store_error(error, "runtime settings"))?;
        let settings = RuntimeSettingsUpdate {
            admin_api_key: snapshot.settings.admin_api_key.clone(),
            refresh_margin_seconds: request.refresh_margin_seconds,
            refresh_concurrency: to_u32_admin(request.refresh_concurrency, "refreshConcurrency")?,
            max_concurrent_per_account: to_u32_admin(
                request.max_concurrent_per_account,
                "maxConcurrentPerAccount",
            )?,
            request_interval_ms: request.request_interval_ms,
            rotation_strategy: request.rotation_strategy,
            provider_model_mappings: request.provider_model_mappings,
            usage_retention_days: to_u32_admin(request.usage_retention_days, "usageRetentionDays")?,
            ops_event_retention_days: to_u32_admin(
                request.ops_event_retention_days,
                "opsEventRetentionDays",
            )?,
            audit_retention_days: to_u32_admin(request.audit_retention_days, "auditRetentionDays")?,
        };
        self.replace_snapshot(
            context,
            snapshot,
            expected,
            settings,
            "update",
            &[
                "refresh_margin_seconds",
                "refresh_concurrency",
                "max_concurrent_per_account",
                "request_interval_ms",
                "rotation_strategy",
                "provider_model_mappings",
                "usage_retention_days",
                "ops_event_retention_days",
                "audit_retention_days",
            ],
        )
        .await
        .map(settings_view)
    }

    async fn admin_api_key_exists(&self) -> Result<bool, AdminServiceError> {
        self.control_plane
            .load_control_plane()
            .await
            .map(|snapshot| snapshot.settings.admin_api_key.is_some())
            .map_err(|error| map_admin_store_error(error, "runtime settings"))
    }

    async fn regenerate_admin_api_key(
        &self,
        context: &AdminRequestContext,
    ) -> Result<String, AdminServiceError> {
        let snapshot = self
            .control_plane
            .load_control_plane()
            .await
            .map_err(|error| map_admin_store_error(error, "runtime settings"))?;
        let expected = snapshot.settings.config_revision;
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let key = format!("admin-{}", hex::encode(bytes));
        let mut settings = runtime_settings_update(&snapshot.settings);
        settings.admin_api_key = Some(key.clone());
        self.replace_snapshot(
            context,
            snapshot,
            expected,
            settings,
            "regenerate_admin_api_key",
            &["admin_api_key"],
        )
        .await?;
        Ok(key)
    }

    async fn delete_admin_api_key(
        &self,
        context: &AdminRequestContext,
    ) -> Result<(), AdminServiceError> {
        let snapshot = self
            .control_plane
            .load_control_plane()
            .await
            .map_err(|error| map_admin_store_error(error, "runtime settings"))?;
        if snapshot.settings.admin_api_key.is_none() {
            return Ok(());
        }
        let expected = snapshot.settings.config_revision;
        let mut settings = runtime_settings_update(&snapshot.settings);
        settings.admin_api_key = None;
        self.replace_snapshot(
            context,
            snapshot,
            expected,
            settings,
            "delete_admin_api_key",
            &["admin_api_key"],
        )
        .await?;
        Ok(())
    }
}

fn settings_view(snapshot: ControlPlaneSnapshot) -> RuntimeSettingsView {
    RuntimeSettingsView {
        config_revision: snapshot.settings.config_revision.get(),
        provider_model_mappings: snapshot.settings.provider_model_mappings,
        refresh_margin_seconds: snapshot.settings.refresh_margin_seconds,
        refresh_concurrency: u64::from(snapshot.settings.refresh_concurrency),
        max_concurrent_per_account: u64::from(snapshot.settings.max_concurrent_per_account),
        request_interval_ms: snapshot.settings.request_interval_ms,
        rotation_strategy: snapshot.settings.rotation_strategy,
        usage_retention_days: u64::from(snapshot.settings.usage_retention_days),
        ops_event_retention_days: u64::from(snapshot.settings.ops_event_retention_days),
        audit_retention_days: u64::from(snapshot.settings.audit_retention_days),
        updated_at: snapshot.settings.updated_at,
    }
}

fn to_u32_admin(value: u64, field: &'static str) -> Result<u32, AdminServiceError> {
    u32::try_from(value).map_err(|_| AdminServiceError::invalid(format!("Invalid {field}")))
}

use gateway_api::admin::client_keys::{
    ClientKeyCursorData, ClientKeyCursorValue, ClientKeyListData, ClientKeySort,
    ClientKeySortDirection, ClientKeySortField, ClientKeyView, ClientKeyViewFields,
    CreateClientKeyFields, CreatedClientKeyData, ListClientKeysFields, MutatedClientKeyData,
    RevealedClientKeyData, UpdateClientKeyFields, decode_client_key_cursor,
    encode_client_key_cursor,
};
use gateway_store::postgres::{
    ClientApiKeyCursor as StoreClientKeyCursor,
    ClientApiKeyCursorValue as StoreClientKeyCursorValue, ClientApiKeyListQuery,
    ClientApiKeyRecord, ClientApiKeyRepository, ClientApiKeySort as StoreClientKeySort,
    ClientApiKeySortDirection as StoreClientKeySortDirection,
    ClientApiKeySortField as StoreClientKeySortField, NewClientApiKey, UpdateClientApiKeyDetails,
};

const DEFAULT_CLIENT_KEY_PAGE_SIZE: u16 = 50;

/// 明文 Client API Key 的安全列表、CAS 管理与显式 reveal adapter。
pub struct ClientKeyAdminAdapter {
    control_plane: Arc<dyn ControlPlaneRepository>,
    repository: Arc<dyn ClientApiKeyRepository>,
    publisher: RuntimeSnapshotPublisher,
}

impl ClientKeyAdminAdapter {
    #[must_use]
    pub const fn new(
        control_plane: Arc<dyn ControlPlaneRepository>,
        repository: Arc<dyn ClientApiKeyRepository>,
        publisher: RuntimeSnapshotPublisher,
    ) -> Self {
        Self {
            control_plane,
            repository,
            publisher,
        }
    }

    async fn mutate_enabled(
        &self,
        context: &AdminRequestContext,
        id: String,
        expected_config_revision: u64,
        enabled: Option<bool>,
    ) -> Result<MutatedClientKeyData, AdminServiceError> {
        let expected = revision(expected_config_revision)?;
        let (action, fields) = match enabled {
            Some(true) => ("enable", &["enabled"][..]),
            Some(false) => ("disable", &["enabled"][..]),
            None => ("delete", &[][..]),
        };
        let audit = admin_audit_event(context, action, "client_api_key", id.clone(), fields);
        let committed = match enabled {
            Some(enabled) => {
                self.control_plane
                    .set_client_api_key_enabled(expected, &id, enabled, audit)
                    .await
            }
            None => {
                self.control_plane
                    .delete_client_api_key(expected, &id, audit)
                    .await
            }
        }
        .map_err(|error| map_admin_store_error(error, "client API key"))?;
        self.publisher.publish_committed(committed).await;
        Ok(MutatedClientKeyData::new(committed.get(), id))
    }
}

#[async_trait]
impl ClientKeyAdminService for ClientKeyAdminAdapter {
    async fn list(
        &self,
        query: ListClientKeysFields,
    ) -> Result<ClientKeyListData, AdminServiceError> {
        let sort = store_client_key_sort(query.sort);
        let cursor = query
            .cursor
            .as_deref()
            .map(decode_client_key_cursor)
            .transpose()
            .map_err(|_| AdminServiceError::invalid("Invalid client key cursor"))?
            .map(|cursor| store_client_key_cursor(cursor, sort))
            .transpose()
            .map_err(|error| map_admin_store_error(error, "client API key cursor"))?;
        let page = self
            .repository
            .list_client_api_keys(ClientApiKeyListQuery {
                cursor,
                page_size: query.limit.unwrap_or(DEFAULT_CLIENT_KEY_PAGE_SIZE),
                search: query.search,
                sort,
            })
            .await
            .map_err(|error| map_admin_store_error(error, "client API key"))?;
        let next_cursor = page
            .next_cursor
            .map(|cursor| encode_client_key_cursor(&wire_client_key_cursor(cursor)))
            .transpose()
            .map_err(|_| AdminServiceError::internal("Failed to encode client key cursor"))?;
        let config_revision = self
            .control_plane
            .load_control_plane()
            .await
            .map_err(|error| map_admin_store_error(error, "runtime settings"))?
            .settings
            .config_revision
            .get();
        Ok(ClientKeyListData::new(
            config_revision,
            page.items.into_iter().map(client_key_view).collect(),
            next_cursor,
            page.total,
        ))
    }

    async fn create(
        &self,
        context: &AdminRequestContext,
        fields: CreateClientKeyFields,
    ) -> Result<CreatedClientKeyData, AdminServiceError> {
        let expected = revision(fields.expected_config_revision)?;
        let id = format!("key_{}", Uuid::now_v7().simple());
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let plaintext_key = format!("sk_{}", URL_SAFE_NO_PAD.encode(bytes));
        let prefix = plaintext_key.chars().take(10).collect::<String>();
        let committed = self
            .control_plane
            .create_client_api_key(
                expected,
                NewClientApiKey {
                    id: id.clone(),
                    name: fields.name,
                    label: fields.label,
                    provider_kind: fields.provider_kind,
                    key: plaintext_key.clone(),
                    max_concurrency: fields.max_concurrency,
                    requests_per_minute: fields.requests_per_minute,
                    tokens_per_minute: fields.tokens_per_minute,
                },
                admin_audit_event(
                    context,
                    "create",
                    "client_api_key",
                    id.clone(),
                    &[
                        "name",
                        "label",
                        "provider_kind",
                        "key",
                        "enabled",
                        "max_concurrency",
                        "requests_per_minute",
                        "tokens_per_minute",
                    ],
                ),
            )
            .await
            .map_err(|error| map_admin_store_error(error, "client API key"))?;
        self.publisher.publish_committed(committed).await;
        Ok(CreatedClientKeyData::new(
            committed.get(),
            id,
            prefix,
            plaintext_key,
        ))
    }

    async fn reveal(&self, id: String) -> Result<RevealedClientKeyData, AdminServiceError> {
        let secret = self
            .repository
            .reveal_client_api_key(&id)
            .await
            .map_err(|error| map_admin_store_error(error, "client API key"))?
            .ok_or_else(|| AdminServiceError::not_found("Client API key was not found"))?;
        Ok(RevealedClientKeyData::new(secret.id, secret.key))
    }

    async fn update(
        &self,
        context: &AdminRequestContext,
        fields: UpdateClientKeyFields,
    ) -> Result<MutatedClientKeyData, AdminServiceError> {
        let expected = revision(fields.expected_config_revision)?;
        let id = fields.id;
        let committed = self
            .control_plane
            .update_client_api_key(
                expected,
                UpdateClientApiKeyDetails {
                    id: id.clone(),
                    name: fields.name,
                    label: fields.label,
                    provider_kind: fields.provider_kind,
                    max_concurrency: fields.max_concurrency,
                    requests_per_minute: fields.requests_per_minute,
                    tokens_per_minute: fields.tokens_per_minute,
                },
                admin_audit_event(
                    context,
                    "update",
                    "client_api_key",
                    id.clone(),
                    &[
                        "name",
                        "label",
                        "provider_kind",
                        "max_concurrency",
                        "requests_per_minute",
                        "tokens_per_minute",
                    ],
                ),
            )
            .await
            .map_err(|error| map_admin_store_error(error, "client API key"))?;
        self.publisher.publish_committed(committed).await;
        Ok(MutatedClientKeyData::new(committed.get(), id))
    }

    async fn disable(
        &self,
        context: &AdminRequestContext,
        id: String,
        expected_config_revision: u64,
    ) -> Result<MutatedClientKeyData, AdminServiceError> {
        self.mutate_enabled(context, id, expected_config_revision, Some(false))
            .await
    }

    async fn enable(
        &self,
        context: &AdminRequestContext,
        id: String,
        expected_config_revision: u64,
    ) -> Result<MutatedClientKeyData, AdminServiceError> {
        self.mutate_enabled(context, id, expected_config_revision, Some(true))
            .await
    }

    async fn delete(
        &self,
        context: &AdminRequestContext,
        id: String,
        expected_config_revision: u64,
    ) -> Result<MutatedClientKeyData, AdminServiceError> {
        self.mutate_enabled(context, id, expected_config_revision, None)
            .await
    }
}

const fn store_client_key_sort(sort: ClientKeySort) -> StoreClientKeySort {
    StoreClientKeySort {
        field: match sort.field {
            ClientKeySortField::Name => StoreClientKeySortField::Name,
            ClientKeySortField::Enabled => StoreClientKeySortField::Enabled,
            ClientKeySortField::CreatedAt => StoreClientKeySortField::CreatedAt,
            ClientKeySortField::LastUsedAt => StoreClientKeySortField::LastUsedAt,
        },
        direction: match sort.direction {
            ClientKeySortDirection::Asc => StoreClientKeySortDirection::Asc,
            ClientKeySortDirection::Desc => StoreClientKeySortDirection::Desc,
        },
    }
}

fn store_client_key_cursor(
    cursor: ClientKeyCursorData,
    expected_sort: StoreClientKeySort,
) -> gateway_store::StoreResult<StoreClientKeyCursor> {
    let sort = store_client_key_sort(cursor.sort);
    if sort != expected_sort {
        return Err(gateway_store::StoreError::InvalidData {
            entity: "client API key cursor",
            message: "cursor sort does not match the requested sort".to_owned(),
        });
    }
    let value = match cursor.value {
        ClientKeyCursorValue::Name(value) => StoreClientKeyCursorValue::Name(value),
        ClientKeyCursorValue::Enabled(value) => StoreClientKeyCursorValue::Enabled(value),
        ClientKeyCursorValue::CreatedAt(value) => StoreClientKeyCursorValue::CreatedAt(value),
        ClientKeyCursorValue::LastUsedAt(value) => StoreClientKeyCursorValue::LastUsedAt(value),
    };
    StoreClientKeyCursor::new(sort, value, cursor.id)
}

fn wire_client_key_cursor(cursor: StoreClientKeyCursor) -> ClientKeyCursorData {
    let sort = ClientKeySort {
        field: match cursor.sort.field {
            StoreClientKeySortField::Name => ClientKeySortField::Name,
            StoreClientKeySortField::Enabled => ClientKeySortField::Enabled,
            StoreClientKeySortField::CreatedAt => ClientKeySortField::CreatedAt,
            StoreClientKeySortField::LastUsedAt => ClientKeySortField::LastUsedAt,
        },
        direction: match cursor.sort.direction {
            StoreClientKeySortDirection::Asc => ClientKeySortDirection::Asc,
            StoreClientKeySortDirection::Desc => ClientKeySortDirection::Desc,
        },
    };
    let value = match cursor.value {
        StoreClientKeyCursorValue::Name(value) => ClientKeyCursorValue::Name(value),
        StoreClientKeyCursorValue::Enabled(value) => ClientKeyCursorValue::Enabled(value),
        StoreClientKeyCursorValue::CreatedAt(value) => ClientKeyCursorValue::CreatedAt(value),
        StoreClientKeyCursorValue::LastUsedAt(value) => ClientKeyCursorValue::LastUsedAt(value),
    };
    ClientKeyCursorData {
        sort,
        value,
        id: cursor.id,
    }
}

fn client_key_view(record: ClientApiKeyRecord) -> ClientKeyView {
    ClientKeyView::new(ClientKeyViewFields {
        id: record.id,
        name: record.name,
        label: record.label,
        provider_kind: record.provider_kind,
        prefix: record.prefix,
        enabled: record.enabled,
        max_concurrency: record.max_concurrency,
        requests_per_minute: record.requests_per_minute,
        tokens_per_minute: record.tokens_per_minute,
        created_at: record.created_at,
        updated_at: record.updated_at,
        last_used_at: record.last_used_at,
    })
}

use gateway_api::admin::openai::{
    CodexCredentialDetailsData, CodexCredentialListData, CodexCredentialMutationData,
    CodexCredentialRotationData, CodexCredentialView, CodexCredentialsDocumentImportData,
    CodexOAuthAuthorizationStartedData, CompleteOAuthAuthorizationRequest,
    CredentialCursorWire as CodexCredentialCursor, CredentialMutationRequest as CodexMutation,
    ImportCredentialsDocumentRequest as CodexImportDocument,
    ListCredentialsQuery as CodexListQuery, RotateCredentialRequest as CodexRotate,
    StartOAuthAuthorizationRequest as CodexStartAuthorization,
};
use gateway_core::engine::credential::{
    CredentialCasUpdate as CoreCredentialUpdate, CredentialRevision as CoreCredentialRevision,
    NewProviderAccount as CoreNewProviderAccount, ProviderAccountStore,
};
use gateway_store::{
    JsonObject,
    postgres::{
        DeleteProviderAccounts, ImportProviderAccounts,
        NewProviderAccount as StoreNewProviderAccount, ProviderAccountAdminRepository,
        ProviderAccountAdminScope, ProviderAccountAvailability, ProviderAccountRepository,
        ProviderAccountSummary, ProviderCredentialUpdate, RotateProviderAccount,
        SetProviderAccountEnabled, UpdateProviderAccount,
    },
};
use provider_openai::credential::{
    CodexCredentialAdmin, CodexCredentialAdminError, CodexCredentialAdminService, CodexOAuthAdmin,
    CodexOAuthAdminError, CodexOAuthFlowBinding, CodexOAuthPendingStore,
    CodexOAuthPendingStoreError, CodexOAuthSecret, CodexPendingAuthorization,
    CodexTokenIdentityVerifier, CompleteCodexOAuthAuthorization, RotateManagedCodexCredential,
    StartCodexOAuthAuthorization, StartCodexOAuthReauthorization, StoredCodexPendingAuthorization,
};
use secrecy::SecretString;
use tokio::sync::Mutex as AsyncMutex;

const PROVIDER_CREDENTIAL_JSON_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone)]
struct PendingCodexAdminFlow {
    owner_ref: String,
    expected_config_revision: Revision,
    kind: PendingCodexAdminFlowKind,
}

#[derive(Debug, Clone)]
enum PendingCodexAdminFlowKind {
    Create {
        provider_instance_id: String,
    },
    Reauthorize {
        account_id: String,
        provider_instance_id: String,
    },
}

/// 单进程 OAuth flow 的一次性 server-only 状态。
pub struct InMemoryCodexOAuthPendingStore {
    flows: AsyncMutex<BTreeMap<String, CodexPendingAuthorization>>,
}

impl InMemoryCodexOAuthPendingStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            flows: AsyncMutex::new(BTreeMap::new()),
        }
    }
}

impl Default for InMemoryCodexOAuthPendingStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CodexOAuthPendingStore for InMemoryCodexOAuthPendingStore {
    async fn create(
        &self,
        pending: &CodexPendingAuthorization,
    ) -> Result<(), CodexOAuthPendingStoreError> {
        let duplicate = CodexPendingAuthorization::from_stored(StoredCodexPendingAuthorization {
            flow_id: pending.flow_id().to_owned(),
            owner_ref: pending.owner_ref().to_owned(),
            started_request_ref: pending.started_request_ref().to_owned(),
            provider_instance_id: pending.provider_instance_id().to_owned(),
            name: pending.name().to_owned(),
            expires_at: pending.expires_at(),
            state: pending.state().clone(),
            nonce: pending.nonce().clone(),
            code_verifier: pending.code_verifier().clone(),
            reauthorization_account_id: pending
                .reauthorization()
                .map(|target| target.account_id().to_string()),
            reauthorization_credential_revision: pending
                .reauthorization()
                .map(|target| target.credential_revision().get()),
        })?;
        let mut flows = self.flows.lock().await;
        if flows
            .insert(pending.flow_id().to_owned(), duplicate)
            .is_some()
        {
            return Err(CodexOAuthPendingStoreError::Conflict);
        }
        Ok(())
    }

    async fn take(
        &self,
        owner_ref: &str,
        flow_id: &str,
    ) -> Result<Option<CodexPendingAuthorization>, CodexOAuthPendingStoreError> {
        let mut flows = self.flows.lock().await;
        if flows
            .get(flow_id)
            .is_some_and(|flow| flow.owner_ref() == owner_ref)
        {
            Ok(flows.remove(flow_id))
        } else {
            Ok(None)
        }
    }
}

/// Codex Provider 验证结果到 PostgreSQL 原子 revision + audit 的组合 adapter。
pub struct CodexAdminAdapter {
    accounts: Arc<dyn ProviderAccountRepository>,
    admin_accounts: Arc<dyn ProviderAccountAdminRepository>,
    core_store: Arc<dyn ProviderAccountStore>,
    control_plane: Arc<dyn ControlPlaneRepository>,
    verifier: Arc<dyn CodexTokenIdentityVerifier>,
    oauth: Arc<dyn CodexOAuthAdmin>,
    owner: Arc<CodexCredentialAdminService>,
    preparer: CodexCredentialAdmin,
    publisher: RuntimeSnapshotPublisher,
    pending_flows: AsyncMutex<BTreeMap<String, PendingCodexAdminFlow>>,
}

/// [`CodexAdminAdapter`] 的显式 owner 组合。
pub struct CodexAdminPorts {
    pub accounts: Arc<dyn ProviderAccountRepository>,
    pub admin_accounts: Arc<dyn ProviderAccountAdminRepository>,
    pub core_store: Arc<dyn ProviderAccountStore>,
    pub control_plane: Arc<dyn ControlPlaneRepository>,
    pub verifier: Arc<dyn CodexTokenIdentityVerifier>,
    pub oauth: Arc<dyn CodexOAuthAdmin>,
    pub owner: Arc<CodexCredentialAdminService>,
    pub publisher: RuntimeSnapshotPublisher,
}

impl CodexAdminAdapter {
    #[must_use]
    pub fn new(ports: CodexAdminPorts) -> Self {
        Self {
            accounts: ports.accounts,
            admin_accounts: ports.admin_accounts,
            core_store: ports.core_store,
            control_plane: ports.control_plane,
            verifier: ports.verifier,
            oauth: ports.oauth,
            owner: ports.owner,
            preparer: CodexCredentialAdmin,
            publisher: ports.publisher,
            pending_flows: AsyncMutex::new(BTreeMap::new()),
        }
    }

    async fn current_revision(&self) -> Result<Revision, AdminServiceError> {
        self.control_plane
            .load_control_plane()
            .await
            .map(|snapshot| snapshot.settings.config_revision)
            .map_err(|error| map_admin_store_error(error, "runtime settings"))
    }

    async fn require_revision(&self, expected: u64) -> Result<Revision, AdminServiceError> {
        let expected = revision(expected)?;
        let current = self.current_revision().await?;
        if current != expected {
            return Err(AdminServiceError::conflict(
                "Configuration revision is stale",
            ));
        }
        Ok(expected)
    }

    async fn import_prepared(
        &self,
        context: &AdminRequestContext,
        expected: Revision,
        provider_instance_id: String,
        prepared: Vec<CoreNewProviderAccount>,
        action: &'static str,
    ) -> Result<(Revision, Vec<String>), AdminServiceError> {
        let mut accounts = Vec::with_capacity(prepared.len());
        for account in prepared {
            accounts.push(store_new_provider_account(account)?);
        }
        let accounts = resolve_admin_import_accounts(
            self.accounts.as_ref(),
            "openai",
            &provider_instance_id,
            accounts,
        )
        .await?;
        let ids: Vec<String> = accounts.iter().map(|account| account.id.clone()).collect();
        let entity_ref = if ids.len() == 1 {
            ids[0].clone()
        } else {
            format!("{} credentials", ids.len())
        };
        let committed = self
            .admin_accounts
            .import_provider_accounts(
                expected,
                ImportProviderAccounts {
                    scope: provider_scope("openai", provider_instance_id),
                    accounts,
                    audit: admin_audit_event(
                        context,
                        action,
                        "provider_account",
                        entity_ref,
                        &["provider_credentials_json", "credential_revision"],
                    ),
                },
            )
            .await
            .map_err(|error| map_admin_store_error(error, "Codex credential"))?;
        self.publisher.publish_committed(committed).await;
        Ok((committed, ids))
    }

    async fn mutate_enabled(
        &self,
        context: &AdminRequestContext,
        request: CodexMutation,
        enabled: Option<bool>,
    ) -> Result<CodexCredentialMutationData, AdminServiceError> {
        let record = self
            .accounts
            .load_provider_account(&request.credential_id)
            .await
            .map_err(|error| map_admin_store_error(error, "Codex credential"))?
            .ok_or_else(|| AdminServiceError::not_found("Codex credential was not found"))?;
        require_provider(&record.summary, "openai")?;
        let expected = revision(request.expected_config_revision)?;
        let id = record.summary.id.clone();
        let scope = provider_scope("openai", record.summary.provider_instance_id);
        let committed = match enabled {
            Some(enabled) => {
                self.admin_accounts
                    .set_provider_account_enabled_admin(
                        expected,
                        SetProviderAccountEnabled {
                            scope,
                            account_id: id.clone(),
                            enabled,
                            audit: admin_audit_event(
                                context,
                                if enabled { "enable" } else { "disable" },
                                "provider_account",
                                id.clone(),
                                &["enabled"],
                            ),
                        },
                    )
                    .await
            }
            None => {
                self.admin_accounts
                    .delete_provider_accounts_admin(
                        expected,
                        DeleteProviderAccounts {
                            scope,
                            account_ids: vec![id.clone()],
                            audit: admin_audit_event(
                                context,
                                "delete",
                                "provider_account",
                                id.clone(),
                                &[],
                            ),
                        },
                    )
                    .await
            }
        }
        .map_err(|error| map_admin_store_error(error, "Codex credential"))?;
        self.publisher.publish_committed(committed).await;
        Ok(CodexCredentialMutationData {
            config_revision: committed.get(),
            credential_id: id,
        })
    }
}

#[async_trait]
impl CodexAdminService for CodexAdminAdapter {
    async fn list(
        &self,
        query: CodexListQuery,
    ) -> Result<CodexCredentialListData, AdminServiceError> {
        let cursor = query
            .cursor
            .as_deref()
            .map(decode_cursor::<CodexCredentialCursor>)
            .transpose()?;
        let mut accounts = self
            .accounts
            .list_provider_accounts(query.provider_instance_id.as_deref(), true)
            .await
            .map_err(|error| map_admin_store_error(error, "Codex credential"))?
            .into_iter()
            .filter(|account| account.provider_kind == "openai")
            .filter(|account| {
                query
                    .enabled
                    .is_none_or(|enabled| account.enabled == enabled)
            })
            .filter(|account| {
                query.availability.as_deref().is_none_or(|availability| {
                    codex_availability(account.availability) == availability
                })
            })
            .collect::<Vec<_>>();
        accounts.sort_by(|left, right| {
            (left.created_at, left.id.as_str()).cmp(&(right.created_at, right.id.as_str()))
        });
        if let Some(cursor) = cursor.as_ref() {
            accounts.retain(|account| {
                (account.created_at, account.id.as_str())
                    > (cursor.created_at, cursor.credential_id.as_str())
            });
        }
        let limit = usize::from(
            query
                .limit
                .unwrap_or(gateway_api::admin::openai::DEFAULT_PAGE_SIZE),
        );
        let has_more = accounts.len() > limit;
        accounts.truncate(limit);
        let next_cursor = has_more
            .then(|| accounts.last())
            .flatten()
            .map(|account| {
                encode_cursor(&CodexCredentialCursor {
                    created_at: account.created_at,
                    credential_id: account.id.clone(),
                })
            })
            .transpose()?;
        Ok(CodexCredentialListData {
            config_revision: self.current_revision().await?.get(),
            items: accounts.into_iter().map(codex_credential_view).collect(),
            next_cursor,
        })
    }

    async fn details(
        &self,
        credential_id: String,
    ) -> Result<CodexCredentialDetailsData, AdminServiceError> {
        let record = self
            .accounts
            .load_provider_account(&credential_id)
            .await
            .map_err(|error| map_admin_store_error(error, "Codex credential"))?
            .ok_or_else(|| AdminServiceError::not_found("Codex credential was not found"))?;
        require_provider(&record.summary, "openai")?;
        Ok(CodexCredentialDetailsData {
            config_revision: self.current_revision().await?.get(),
            credential: codex_credential_view(record.summary),
        })
    }

    async fn import_document(
        &self,
        context: &AdminRequestContext,
        request: CodexImportDocument,
    ) -> Result<CodexCredentialsDocumentImportData, AdminServiceError> {
        let expected = revision(request.expected_config_revision)?;
        let instance =
            gateway_core::routing::ProviderInstanceId::new(request.provider_instance_id.clone())
                .map_err(|_| AdminServiceError::invalid("Invalid Codex Provider instance"))?;
        let profile = self
            .owner
            .prepare_import_document(instance, request.document)
            .await
            .map_err(map_codex_admin_error)?;
        let (committed, ids) = self
            .import_prepared(
                context,
                expected,
                request.provider_instance_id,
                profile.into_accounts(),
                "import_document",
            )
            .await?;
        Ok(CodexCredentialsDocumentImportData {
            config_revision: committed.get(),
            credential_ids: ids,
        })
    }

    async fn start_authorization(
        &self,
        context: &AdminRequestContext,
        request: CodexStartAuthorization,
    ) -> Result<CodexOAuthAuthorizationStartedData, AdminServiceError> {
        let expected = self
            .require_revision(request.expected_config_revision)
            .await?;
        let owner_ref = admin_flow_owner(context);
        let binding = CodexOAuthFlowBinding::new(owner_ref.clone(), context.request_id())
            .map_err(map_codex_oauth_error)?;
        let (started, kind) = match (request.credential_id, request.expected_credential_revision) {
            (None, None) => {
                let provider_instance_id = request.provider_instance_id;
                let started = self
                    .oauth
                    .start_authorization(StartCodexOAuthAuthorization {
                        binding,
                        provider_instance_id: provider_instance_id.clone(),
                        name: request.name,
                    })
                    .await
                    .map_err(map_codex_oauth_error)?;
                (
                    started,
                    PendingCodexAdminFlowKind::Create {
                        provider_instance_id,
                    },
                )
            }
            (Some(account_id), Some(expected_credential_revision)) => {
                let record = self
                    .accounts
                    .load_provider_account(&account_id)
                    .await
                    .map_err(|error| map_admin_store_error(error, "Codex credential"))?
                    .ok_or_else(|| {
                        AdminServiceError::not_found("Codex credential was not found")
                    })?;
                require_provider(&record.summary, "openai")?;
                if record.summary.provider_instance_id != request.provider_instance_id {
                    return Err(AdminServiceError::conflict(
                        "Codex credential does not belong to the selected Provider instance",
                    ));
                }
                let account =
                    gateway_core::engine::credential::ProviderAccountId::new(account_id.clone())
                        .map_err(|_| AdminServiceError::invalid("Invalid Codex credential ID"))?;
                let credential_revision = CoreCredentialRevision::new(expected_credential_revision)
                    .map_err(|_| AdminServiceError::invalid("Invalid credential revision"))?;
                let started = self
                    .oauth
                    .start_reauthorization(StartCodexOAuthReauthorization {
                        binding,
                        account_id: account,
                        expected_credential_revision: credential_revision,
                    })
                    .await
                    .map_err(map_codex_oauth_error)?;
                (
                    started,
                    PendingCodexAdminFlowKind::Reauthorize {
                        account_id,
                        provider_instance_id: record.summary.provider_instance_id,
                    },
                )
            }
            _ => {
                return Err(AdminServiceError::invalid(
                    "Codex reauthorization target is incomplete",
                ));
            }
        };
        self.pending_flows.lock().await.insert(
            started.flow_id.clone(),
            PendingCodexAdminFlow {
                owner_ref,
                expected_config_revision: expected,
                kind,
            },
        );
        Ok(CodexOAuthAuthorizationStartedData {
            flow_id: started.flow_id,
            authorization_url: started.authorization_url,
            expires_at: started.expires_at,
        })
    }

    async fn complete_authorization(
        &self,
        context: &AdminRequestContext,
        request: CompleteOAuthAuthorizationRequest,
    ) -> Result<CodexCredentialMutationData, AdminServiceError> {
        let owner_ref = admin_flow_owner(context);
        let pending = {
            let mut flows = self.pending_flows.lock().await;
            if !flows
                .get(&request.flow_id)
                .is_some_and(|pending| pending.owner_ref == owner_ref)
            {
                return Err(AdminServiceError::not_found(
                    "Codex OAuth flow was not found",
                ));
            }
            flows
                .remove(&request.flow_id)
                .expect("checked Codex OAuth flow exists")
        };
        let command = CompleteCodexOAuthAuthorization {
            owner_ref,
            flow_id: request.flow_id,
            callback_url: SecretString::from(request.callback_url),
        };
        match pending.kind {
            PendingCodexAdminFlowKind::Create {
                provider_instance_id,
            } => {
                let prepared = self
                    .oauth
                    .complete_authorization(command)
                    .await
                    .map_err(map_codex_oauth_error)?;
                let (committed, ids) = self
                    .import_prepared(
                        context,
                        pending.expected_config_revision,
                        provider_instance_id,
                        vec![prepared],
                        "oauth_complete",
                    )
                    .await?;
                Ok(CodexCredentialMutationData {
                    config_revision: committed.get(),
                    credential_id: ids.into_iter().next().expect("single OAuth account"),
                })
            }
            PendingCodexAdminFlowKind::Reauthorize {
                account_id,
                provider_instance_id,
            } => {
                let prepared = self
                    .oauth
                    .complete_reauthorization(command)
                    .await
                    .map_err(map_codex_oauth_error)?;
                let (profile, credential, guard) = prepared.into_parts();
                let rotation = self
                    .admin_accounts
                    .rotate_provider_account(
                        pending.expected_config_revision,
                        RotateProviderAccount {
                            scope: provider_scope("openai", provider_instance_id),
                            profile: store_profile(profile),
                            credential: store_credential_update(credential)?,
                            audit: admin_audit_event(
                                context,
                                "oauth_reauthorize",
                                "provider_account",
                                account_id.clone(),
                                &["provider_credentials_json", "credential_revision"],
                            ),
                        },
                    )
                    .await
                    .map_err(|error| map_admin_store_error(error, "Codex credential"))?;
                drop(guard);
                self.publisher
                    .publish_committed(rotation.config_revision)
                    .await;
                Ok(CodexCredentialMutationData {
                    config_revision: rotation.config_revision.get(),
                    credential_id: account_id,
                })
            }
        }
    }

    async fn rotate(
        &self,
        context: &AdminRequestContext,
        request: CodexRotate,
    ) -> Result<CodexCredentialRotationData, AdminServiceError> {
        let account_id =
            gateway_core::engine::credential::ProviderAccountId::new(request.credential_id.clone())
                .map_err(|_| AdminServiceError::invalid("Invalid Codex credential ID"))?;
        let expected_credential = CoreCredentialRevision::new(request.expected_credential_revision)
            .map_err(|_| AdminServiceError::invalid("Invalid credential revision"))?;
        let current = self
            .core_store
            .load_credential(&account_id, expected_credential)
            .await
            .map_err(|_| AdminServiceError::conflict("Credential revision is stale"))?;
        let secret = CodexOAuthSecret {
            access_token: SecretString::from(request.access_token),
            refresh_token: request.refresh_token.map(SecretString::from),
            id_token: None,
        };
        let profile = self
            .verifier
            .verify(&secret)
            .await
            .map_err(map_codex_identity_error)?;
        let prepared = self
            .preparer
            .prepare_rotation(RotateManagedCodexCredential {
                current,
                secret,
                verified_account: profile,
            })
            .map_err(map_codex_admin_error)?;
        let scope = provider_scope(
            "openai",
            prepared_instance(&self.accounts, &request.credential_id).await?,
        );
        let rotation = self
            .admin_accounts
            .rotate_provider_account(
                revision(request.expected_config_revision)?,
                RotateProviderAccount {
                    scope,
                    profile: store_profile(prepared.profile),
                    credential: store_credential_update(prepared.credential)?,
                    audit: admin_audit_event(
                        context,
                        "rotate",
                        "provider_account",
                        request.credential_id.clone(),
                        &["provider_credentials_json", "credential_revision"],
                    ),
                },
            )
            .await
            .map_err(|error| map_admin_store_error(error, "Codex credential"))?;
        self.publisher
            .publish_committed(rotation.config_revision)
            .await;
        Ok(CodexCredentialRotationData {
            credential_id: request.credential_id,
            credential_revision: i64::try_from(rotation.credential_revision.get())
                .map_err(|_| AdminServiceError::internal("Credential revision overflow"))?,
        })
    }

    async fn enable(
        &self,
        context: &AdminRequestContext,
        request: CodexMutation,
    ) -> Result<CodexCredentialMutationData, AdminServiceError> {
        self.mutate_enabled(context, request, Some(true)).await
    }

    async fn disable(
        &self,
        context: &AdminRequestContext,
        request: CodexMutation,
    ) -> Result<CodexCredentialMutationData, AdminServiceError> {
        self.mutate_enabled(context, request, Some(false)).await
    }

    async fn delete(
        &self,
        context: &AdminRequestContext,
        request: CodexMutation,
    ) -> Result<CodexCredentialMutationData, AdminServiceError> {
        self.mutate_enabled(context, request, None).await
    }
}

fn provider_scope(
    provider_kind: impl Into<String>,
    provider_instance_id: impl Into<String>,
) -> ProviderAccountAdminScope {
    ProviderAccountAdminScope {
        provider_kind: provider_kind.into(),
        provider_instance_id: provider_instance_id.into(),
    }
}

async fn resolve_admin_import_accounts(
    repository: &dyn ProviderAccountRepository,
    provider_kind: &str,
    provider_instance_id: &str,
    mut accounts: Vec<StoreNewProviderAccount>,
) -> Result<Vec<StoreNewProviderAccount>, AdminServiceError> {
    let existing = repository
        .list_provider_accounts(None, true)
        .await
        .map_err(|error| map_admin_store_error(error, "Provider credential"))?;
    let mut resolved_ids = BTreeSet::new();
    let mut batch_identities = BTreeSet::new();

    for account in &mut accounts {
        if account.provider_kind != provider_kind
            || account.provider_instance_id != provider_instance_id
        {
            return Err(AdminServiceError::invalid(
                "Imported credential is outside the selected Provider instance",
            ));
        }
        let existing_by_id = existing.iter().find(|current| current.id == account.id);
        let existing_by_identity = existing.iter().find(|current| {
            current.provider_kind == provider_kind
                && current.upstream_user_id == account.upstream_user_id
                && current.upstream_account_id == account.upstream_account_id
        });

        if let Some(current) = existing_by_id
            && (current.provider_kind != provider_kind
                || current.provider_instance_id != provider_instance_id
                || current.upstream_user_id != account.upstream_user_id
                || current.upstream_account_id != account.upstream_account_id)
        {
            return Err(AdminServiceError::conflict(
                "Provider credential ID is already bound to another identity",
            ));
        }
        if let Some(current) = existing_by_identity {
            if current.provider_instance_id != provider_instance_id
                || existing_by_id.is_some_and(|by_id| by_id.id != current.id)
            {
                return Err(AdminServiceError::conflict(
                    "Provider credential identity is already bound to another instance",
                ));
            }
            account.id.clone_from(&current.id);
        }

        if !resolved_ids.insert(account.id.clone())
            || !batch_identities.insert((
                account.upstream_user_id.clone(),
                account.upstream_account_id.clone(),
            ))
        {
            return Err(AdminServiceError::invalid(
                "Import document contains duplicate Provider identities",
            ));
        }
    }
    Ok(accounts)
}

fn require_provider(
    account: &ProviderAccountSummary,
    provider: &str,
) -> Result<(), AdminServiceError> {
    if account.provider_kind == provider {
        Ok(())
    } else {
        Err(AdminServiceError::not_found(
            "Provider credential was not found",
        ))
    }
}

fn store_new_provider_account(
    prepared: CoreNewProviderAccount,
) -> Result<StoreNewProviderAccount, AdminServiceError> {
    let account = prepared.account;
    let credential = JsonObject::try_from_value(
        "provider credential",
        serde_json::Value::Object(prepared.credential.into_inner()),
        PROVIDER_CREDENTIAL_JSON_LIMIT,
    )
    .map_err(|error| map_admin_store_error(error, "Provider credential"))?;
    Ok(StoreNewProviderAccount {
        id: account.id().as_str().to_owned(),
        provider_instance_id: account.instance().as_str().to_owned(),
        provider_kind: account.provider().as_str().to_owned(),
        name: account.name().to_owned(),
        email: account.email().map(str::to_owned),
        upstream_user_id: account.upstream_user_id().to_owned(),
        upstream_account_id: account.upstream_account_id().map(str::to_owned),
        plan_type: account.plan_type().map(str::to_owned),
        provider_credentials_json: credential,
        has_refresh_token: account.has_refresh_token(),
        access_token_expires_at: DateTime::<Utc>::from(account.access_token_expires_at()),
        next_refresh_at: account.next_refresh_at().map(DateTime::<Utc>::from),
        enabled: account.enabled(),
        availability: store_account_availability(account.availability()),
        cooldown_until: account.cooldown_until().map(DateTime::<Utc>::from),
        availability_observed_at: Utc::now(),
    })
}

const fn store_account_availability(
    availability: gateway_core::engine::credential::AccountAvailability,
) -> ProviderAccountAvailability {
    match availability {
        gateway_core::engine::credential::AccountAvailability::Unknown => {
            ProviderAccountAvailability::Unknown
        }
        gateway_core::engine::credential::AccountAvailability::Ready => {
            ProviderAccountAvailability::Ready
        }
        gateway_core::engine::credential::AccountAvailability::Cooldown => {
            ProviderAccountAvailability::Cooldown
        }
        gateway_core::engine::credential::AccountAvailability::QuotaExhausted => {
            ProviderAccountAvailability::QuotaExhausted
        }
        gateway_core::engine::credential::AccountAvailability::Expired => {
            ProviderAccountAvailability::Expired
        }
        gateway_core::engine::credential::AccountAvailability::Banned => {
            ProviderAccountAvailability::Banned
        }
        gateway_core::engine::credential::AccountAvailability::Invalid => {
            ProviderAccountAvailability::Invalid
        }
    }
}

fn store_profile(
    profile: gateway_core::engine::credential::ProviderAccountUpdate,
) -> UpdateProviderAccount {
    UpdateProviderAccount {
        id: profile.account_id.as_str().to_owned(),
        name: profile.name,
        email: profile.email,
        plan_type: profile.plan_type,
    }
}

fn store_credential_update(
    update: CoreCredentialUpdate,
) -> Result<ProviderCredentialUpdate, AdminServiceError> {
    let (
        account_id,
        expected_revision,
        _profile,
        credential,
        has_refresh_token,
        access_token_expires_at,
        next_refresh_at,
    ) = update.into_parts();
    Ok(ProviderCredentialUpdate {
        account_id: account_id.as_str().to_owned(),
        expected_revision: revision(expected_revision.get())?,
        provider_credentials_json: JsonObject::try_from_value(
            "provider credential",
            serde_json::Value::Object(credential.into_inner()),
            PROVIDER_CREDENTIAL_JSON_LIMIT,
        )
        .map_err(|error| map_admin_store_error(error, "Provider credential"))?,
        has_refresh_token,
        access_token_expires_at: DateTime::<Utc>::from(access_token_expires_at),
        next_refresh_at: next_refresh_at.map(DateTime::<Utc>::from),
    })
}

async fn prepared_instance(
    accounts: &Arc<dyn ProviderAccountRepository>,
    account_id: &str,
) -> Result<String, AdminServiceError> {
    accounts
        .load_provider_account(account_id)
        .await
        .map_err(|error| map_admin_store_error(error, "Provider credential"))?
        .map(|record| record.summary.provider_instance_id)
        .ok_or_else(|| AdminServiceError::not_found("Provider credential was not found"))
}

fn codex_credential_view(account: ProviderAccountSummary) -> CodexCredentialView {
    CodexCredentialView {
        id: account.id,
        provider_instance_id: account.provider_instance_id,
        name: account.name,
        email: account.email,
        upstream_user_id: account.upstream_user_id,
        upstream_account_id: account.upstream_account_id,
        enabled: account.enabled,
        credential_revision: i64::try_from(account.credential_revision.get()).unwrap_or(i64::MAX),
        has_refresh_token: account.has_refresh_token,
        availability: codex_availability(account.availability).to_owned(),
        availability_reason: account.availability_reason,
        plan_type: account.plan_type,
        access_token_expires_at: account.access_token_expires_at,
        next_refresh_at: account.next_refresh_at,
        cooldown_until: account.cooldown_until,
        created_at: account.created_at,
        updated_at: account.updated_at,
    }
}

const fn codex_availability(value: ProviderAccountAvailability) -> &'static str {
    match value {
        ProviderAccountAvailability::Unknown => "unknown",
        ProviderAccountAvailability::Ready => "ready",
        ProviderAccountAvailability::Cooldown => "cooldown",
        ProviderAccountAvailability::QuotaExhausted => "exhausted",
        ProviderAccountAvailability::Expired
        | ProviderAccountAvailability::Banned
        | ProviderAccountAvailability::Invalid => "invalid",
    }
}

fn admin_flow_owner(context: &AdminRequestContext) -> String {
    context
        .admin_user_id()
        .map(|id| format!("admin:{id}"))
        .unwrap_or_else(|| "admin:api-key".to_owned())
}

fn map_codex_identity_error(
    error: provider_openai::credential::CodexIdentityVerificationError,
) -> AdminServiceError {
    match error {
        provider_openai::credential::CodexIdentityVerificationError::Rejected => {
            AdminServiceError::invalid("Codex OAuth identity was rejected")
        }
        provider_openai::credential::CodexIdentityVerificationError::Unavailable => {
            AdminServiceError::unavailable("Codex identity verification is unavailable")
        }
    }
}

fn map_codex_admin_error(error: CodexCredentialAdminError) -> AdminServiceError {
    match error {
        CodexCredentialAdminError::InvalidInput | CodexCredentialAdminError::InvalidCredential => {
            AdminServiceError::invalid("Codex credential is invalid")
        }
        CodexCredentialAdminError::IdentityMismatch => {
            AdminServiceError::conflict("Codex credential identity cannot be rebound")
        }
        CodexCredentialAdminError::NotFound => {
            AdminServiceError::not_found("Codex account was not found")
        }
        CodexCredentialAdminError::RevisionConflict
        | CodexCredentialAdminError::RefreshLeaseUnavailable
        | CodexCredentialAdminError::RefreshAmbiguous => {
            AdminServiceError::conflict("Codex account changed while the credential was refreshed")
        }
        CodexCredentialAdminError::MissingRefreshToken => {
            AdminServiceError::invalid("Codex account has no refresh token")
        }
        CodexCredentialAdminError::RefreshRejected
        | CodexCredentialAdminError::AccountBanned
        | CodexCredentialAdminError::IdentityRejected => {
            AdminServiceError::invalid("Codex refreshed credential was rejected")
        }
        CodexCredentialAdminError::StoreUnavailable
        | CodexCredentialAdminError::RefreshUnavailable
        | CodexCredentialAdminError::IdentityUnavailable => {
            AdminServiceError::unavailable("Codex credential refresh is unavailable")
        }
    }
}

fn map_codex_oauth_error(error: CodexOAuthAdminError) -> AdminServiceError {
    match error {
        CodexOAuthAdminError::InvalidInput | CodexOAuthAdminError::UpstreamRejected => {
            AdminServiceError::invalid("Codex OAuth operation was rejected")
        }
        CodexOAuthAdminError::NotFound | CodexOAuthAdminError::FlowExpired => {
            AdminServiceError::not_found("Codex OAuth flow was not found")
        }
        CodexOAuthAdminError::Conflict | CodexOAuthAdminError::Ambiguous => {
            AdminServiceError::conflict("Codex OAuth operation conflicts with current state")
        }
        CodexOAuthAdminError::UpstreamUnavailable | CodexOAuthAdminError::StorageUnavailable => {
            AdminServiceError::unavailable("Codex OAuth service is unavailable")
        }
        CodexOAuthAdminError::Credential => {
            AdminServiceError::internal("Codex credential preparation failed")
        }
    }
}

use gateway_api::admin::xai::{
    AuthorizationStartResult, CompleteAuthorizationRequest as XaiCompleteAuthorization,
    CredentialMutationRequest as XaiMutation, ListCredentialsQuery as XaiListQuery,
    StartAuthorizationRequest as XaiStartAuthorization, XaiCredentialImportData,
    XaiCredentialImportDocumentRequest, XaiCredentialListData, XaiCredentialMutationData,
    XaiCredentialViewData,
};
use provider_xai::{
    AuthorizationCallback, DiscoveryDocument, GrokCredentialAdmin, GrokOAuthClient,
    GrokOAuthImportDocument, OFFICIAL_REDIRECT_URI, PendingAuthorization, RedirectUriAllowlist,
    VerifiedGrokAccount,
};

struct PendingXaiAuthorizationFlow {
    owner_ref: String,
    expected_config_revision: Revision,
    provider_instance_id: String,
    name: String,
    expires_at: DateTime<Utc>,
    discovery: DiscoveryDocument,
    pending: PendingAuthorization,
}

/// xAI 官方 OAuth/OIDC 流到 Store 原子账号事务的组合 adapter。
pub struct XaiAdminAdapter {
    accounts: Arc<dyn ProviderAccountRepository>,
    admin_accounts: Arc<dyn ProviderAccountAdminRepository>,
    control_plane: Arc<dyn ControlPlaneRepository>,
    runtime_settings: Arc<dyn RuntimeSettingsRepository>,
    oauth: Arc<GrokOAuthClient>,
    preparer: GrokCredentialAdmin,
    publisher: RuntimeSnapshotPublisher,
    authorization_flows: AsyncMutex<BTreeMap<String, PendingXaiAuthorizationFlow>>,
}

impl XaiAdminAdapter {
    #[must_use]
    pub fn new(
        accounts: Arc<dyn ProviderAccountRepository>,
        admin_accounts: Arc<dyn ProviderAccountAdminRepository>,
        control_plane: Arc<dyn ControlPlaneRepository>,
        runtime_settings: Arc<dyn RuntimeSettingsRepository>,
        oauth: Arc<GrokOAuthClient>,
        publisher: RuntimeSnapshotPublisher,
    ) -> Self {
        Self {
            accounts,
            admin_accounts,
            control_plane,
            runtime_settings,
            oauth,
            preparer: GrokCredentialAdmin,
            publisher,
            authorization_flows: AsyncMutex::new(BTreeMap::new()),
        }
    }

    async fn current_revision(&self) -> Result<Revision, AdminServiceError> {
        self.control_plane
            .load_control_plane()
            .await
            .map(|snapshot| snapshot.settings.config_revision)
            .map_err(|error| map_admin_store_error(error, "runtime settings"))
    }

    async fn require_revision(&self, expected: u64) -> Result<Revision, AdminServiceError> {
        let expected = revision(expected)?;
        if self.current_revision().await? != expected {
            return Err(AdminServiceError::conflict(
                "Configuration revision is stale",
            ));
        }
        Ok(expected)
    }

    async fn refresh_margin(&self) -> Result<Duration, AdminServiceError> {
        self.runtime_settings
            .load_runtime_settings()
            .await
            .map(|settings| Duration::from_secs(settings.refresh_margin_seconds))
            .map_err(|error| map_admin_store_error(error, "runtime settings"))
    }

    async fn prepare_verified(
        &self,
        provider_instance_id: String,
        name: String,
        email: Option<String>,
        tokens: provider_xai::VerifiedTokenSet,
    ) -> Result<CoreNewProviderAccount, AdminServiceError> {
        let account_id = gateway_core::engine::credential::ProviderAccountId::new(format!(
            "acct_{}",
            Uuid::now_v7().simple()
        ))
        .map_err(|_| AdminServiceError::internal("Failed to allocate xAI account ID"))?;
        let provider_instance_id =
            gateway_core::routing::ProviderInstanceId::new(provider_instance_id)
                .map_err(|_| AdminServiceError::invalid("Invalid xAI Provider instance"))?;
        self.preparer
            .prepare_verified_account(&VerifiedGrokAccount {
                account_id,
                provider_instance_id,
                name,
                email,
                upstream_account_id: None,
                plan_type: None,
                tokens,
                enabled: true,
                refresh_margin: self.refresh_margin().await?,
            })
            .map_err(map_xai_repository_error)
    }

    async fn commit_import(
        &self,
        context: &AdminRequestContext,
        expected: Revision,
        provider_instance_id: String,
        prepared: Vec<CoreNewProviderAccount>,
        action: &'static str,
    ) -> Result<(Revision, Vec<String>), AdminServiceError> {
        let mut accounts = Vec::with_capacity(prepared.len());
        for account in prepared {
            accounts.push(store_new_provider_account(account)?);
        }
        let accounts = resolve_admin_import_accounts(
            self.accounts.as_ref(),
            "xai",
            &provider_instance_id,
            accounts,
        )
        .await?;
        let ids: Vec<String> = accounts.iter().map(|account| account.id.clone()).collect();
        let committed = self
            .admin_accounts
            .import_provider_accounts(
                expected,
                ImportProviderAccounts {
                    scope: provider_scope("xai", provider_instance_id),
                    accounts,
                    audit: admin_audit_event(
                        context,
                        action,
                        "provider_account",
                        if ids.len() == 1 {
                            ids[0].clone()
                        } else {
                            format!("{} credentials", ids.len())
                        },
                        &["provider_credentials_json", "credential_revision"],
                    ),
                },
            )
            .await
            .map_err(|error| map_admin_store_error(error, "xAI credential"))?;
        self.publisher.publish_committed(committed).await;
        Ok((committed, ids))
    }

    async fn mutate_xai_enabled(
        &self,
        context: &AdminRequestContext,
        request: XaiMutation,
        enabled: Option<bool>,
    ) -> Result<XaiCredentialMutationData, AdminServiceError> {
        let record = self
            .accounts
            .load_provider_account(&request.credential_id)
            .await
            .map_err(|error| map_admin_store_error(error, "xAI credential"))?
            .ok_or_else(|| AdminServiceError::not_found("xAI credential was not found"))?;
        require_provider(&record.summary, "xai")?;
        let id = record.summary.id.clone();
        let scope = provider_scope("xai", record.summary.provider_instance_id);
        let expected = revision(request.expected_config_revision)?;
        let committed = match enabled {
            Some(enabled) => {
                self.admin_accounts
                    .set_provider_account_enabled_admin(
                        expected,
                        SetProviderAccountEnabled {
                            scope,
                            account_id: id.clone(),
                            enabled,
                            audit: admin_audit_event(
                                context,
                                if enabled { "enable" } else { "disable" },
                                "provider_account",
                                id.clone(),
                                &["enabled"],
                            ),
                        },
                    )
                    .await
            }
            None => {
                self.admin_accounts
                    .delete_provider_accounts_admin(
                        expected,
                        DeleteProviderAccounts {
                            scope,
                            account_ids: vec![id.clone()],
                            audit: admin_audit_event(
                                context,
                                "delete",
                                "provider_account",
                                id.clone(),
                                &[],
                            ),
                        },
                    )
                    .await
            }
        }
        .map_err(|error| map_admin_store_error(error, "xAI credential"))?;
        self.publisher.publish_committed(committed).await;
        Ok(XaiCredentialMutationData {
            config_revision: committed.get(),
            credential_id: id,
        })
    }
}

#[async_trait]
impl XaiAdminService for XaiAdminAdapter {
    async fn list(&self, query: XaiListQuery) -> Result<XaiCredentialListData, AdminServiceError> {
        let items = self
            .accounts
            .list_provider_accounts(query.provider_instance_id.as_deref(), true)
            .await
            .map_err(|error| map_admin_store_error(error, "xAI credential"))?
            .into_iter()
            .filter(|account| account.provider_kind == "xai")
            .map(xai_credential_view)
            .collect();
        Ok(XaiCredentialListData {
            config_revision: self.current_revision().await?.get(),
            items,
        })
    }

    async fn import_document(
        &self,
        context: &AdminRequestContext,
        request: XaiCredentialImportDocumentRequest,
    ) -> Result<XaiCredentialImportData, AdminServiceError> {
        let expected = revision(request.expected_config_revision)?;
        let encoded = serde_json::to_vec(&request.document)
            .map_err(|_| AdminServiceError::invalid("xAI import document is invalid"))?;
        let document =
            GrokOAuthImportDocument::parse_json(&encoded).map_err(map_xai_import_error)?;
        let discovery = self.oauth.discover().await.map_err(map_xai_oauth_error)?;
        let entries = document.into_entries();
        let mut prepared = Vec::with_capacity(entries.len());
        for entry in entries {
            let name = entry.name().to_owned();
            let email = entry.email().map(str::to_owned);
            let tokens = self
                .oauth
                .verify_imported_credential(&discovery, entry.into_candidate())
                .await
                .map_err(map_xai_import_error)?;
            prepared.push(
                self.prepare_verified(request.provider_instance_id.clone(), name, email, tokens)
                    .await?,
            );
        }
        let (committed, ids) = self
            .commit_import(
                context,
                expected,
                request.provider_instance_id,
                prepared,
                "import_document",
            )
            .await?;
        Ok(XaiCredentialImportData::new(committed.get(), ids))
    }

    async fn start_authorization(
        &self,
        context: &AdminRequestContext,
        request: XaiStartAuthorization,
    ) -> Result<AuthorizationStartResult, AdminServiceError> {
        let expected = self
            .require_revision(request.credential.expected_config_revision)
            .await?;
        let discovery = self.oauth.discover().await.map_err(map_xai_oauth_error)?;
        let allowlist = RedirectUriAllowlist::new([OFFICIAL_REDIRECT_URI])
            .map_err(|_| AdminServiceError::invalid("xAI redirect URI is invalid"))?;
        let redirect_uri = allowlist
            .authorize(OFFICIAL_REDIRECT_URI)
            .map_err(|_| AdminServiceError::invalid("xAI redirect URI is not allowed"))?;
        let pending = self
            .oauth
            .start_authorization_code(&discovery, redirect_uri, None)
            .map_err(map_xai_oauth_error)?;
        let authorization_url = pending.authorization_url().to_string();
        let flow_id = format!("flow_{}", Uuid::now_v7().simple());
        let expires_at = Utc::now() + ChronoDuration::minutes(30);
        self.authorization_flows.lock().await.insert(
            flow_id.clone(),
            PendingXaiAuthorizationFlow {
                owner_ref: admin_flow_owner(context),
                expected_config_revision: expected,
                provider_instance_id: request.credential.provider_instance_id,
                name: request.credential.name,
                expires_at,
                discovery,
                pending,
            },
        );
        Ok(AuthorizationStartResult {
            flow_id,
            authorization_url,
            expires_at,
        })
    }

    async fn complete_authorization(
        &self,
        context: &AdminRequestContext,
        request: XaiCompleteAuthorization,
    ) -> Result<XaiCredentialMutationData, AdminServiceError> {
        let owner_ref = admin_flow_owner(context);
        let flow = {
            let mut flows = self.authorization_flows.lock().await;
            if !flows
                .get(&request.flow_id)
                .is_some_and(|flow| flow.owner_ref == owner_ref)
            {
                return Err(AdminServiceError::not_found(
                    "xAI authorization flow was not found",
                ));
            }
            flows
                .remove(&request.flow_id)
                .expect("checked xAI authorization flow exists")
        };
        if flow.expires_at <= Utc::now() {
            return Err(AdminServiceError::not_found(
                "xAI authorization flow expired",
            ));
        }
        let callback_url = url::Url::parse(request.callback_url.trim())
            .map_err(|_| AdminServiceError::invalid("xAI OAuth callback URL is invalid"))?;
        let expected_callback = url::Url::parse(OFFICIAL_REDIRECT_URI)
            .map_err(|_| AdminServiceError::internal("xAI OAuth callback is invalid"))?;
        if callback_url.scheme() != expected_callback.scheme()
            || callback_url.host_str() != expected_callback.host_str()
            || callback_url.port_or_known_default() != expected_callback.port_or_known_default()
            || callback_url.path() != expected_callback.path()
            || !callback_url.username().is_empty()
            || callback_url.password().is_some()
            || callback_url.fragment().is_some()
        {
            return Err(AdminServiceError::invalid(
                "xAI OAuth callback URL is not allowed",
            ));
        }
        let callback = AuthorizationCallback::parse(callback_url.query().unwrap_or_default())
            .map_err(|_| AdminServiceError::invalid("xAI OAuth callback was rejected"))?;
        let grant = flow
            .pending
            .accept_callback(callback)
            .map_err(map_xai_oauth_error)?;
        let tokens = self
            .oauth
            .exchange_authorization_code(&flow.discovery, grant)
            .await
            .map_err(map_xai_oauth_error)?;
        let prepared = self
            .prepare_verified(flow.provider_instance_id.clone(), flow.name, None, tokens)
            .await?;
        let (committed, ids) = self
            .commit_import(
                context,
                flow.expected_config_revision,
                flow.provider_instance_id,
                vec![prepared],
                "authorization_oauth_complete",
            )
            .await?;
        Ok(XaiCredentialMutationData {
            config_revision: committed.get(),
            credential_id: ids
                .into_iter()
                .next()
                .expect("single authorization account"),
        })
    }

    async fn disable(
        &self,
        context: &AdminRequestContext,
        request: XaiMutation,
    ) -> Result<XaiCredentialMutationData, AdminServiceError> {
        self.mutate_xai_enabled(context, request, Some(false)).await
    }

    async fn enable(
        &self,
        context: &AdminRequestContext,
        request: XaiMutation,
    ) -> Result<XaiCredentialMutationData, AdminServiceError> {
        self.mutate_xai_enabled(context, request, Some(true)).await
    }

    async fn delete(
        &self,
        context: &AdminRequestContext,
        request: XaiMutation,
    ) -> Result<XaiCredentialMutationData, AdminServiceError> {
        self.mutate_xai_enabled(context, request, None).await
    }
}

fn xai_credential_view(account: ProviderAccountSummary) -> XaiCredentialViewData {
    XaiCredentialViewData {
        id: account.id,
        provider_instance_id: account.provider_instance_id,
        name: account.name,
        email: account.email,
        upstream_user_id: account.upstream_user_id,
        upstream_account_id: account.upstream_account_id,
        plan_type: account.plan_type,
        enabled: account.enabled,
        credential_revision: i64::try_from(account.credential_revision.get()).unwrap_or(i64::MAX),
        has_refresh_token: account.has_refresh_token,
        availability: account.availability.as_str().to_owned(),
        availability_reason: account.availability_reason,
        access_token_expires_at: account.access_token_expires_at,
        next_refresh_at: account.next_refresh_at,
        cooldown_until: account.cooldown_until,
        created_at: account.created_at,
        updated_at: account.updated_at,
    }
}

fn map_xai_oauth_error(error: provider_xai::OAuthError) -> AdminServiceError {
    match error.class() {
        provider_xai::FailureClass::Transient => {
            AdminServiceError::unavailable("xAI OAuth service is unavailable")
        }
        provider_xai::FailureClass::Ambiguous => {
            AdminServiceError::conflict("xAI OAuth send state is ambiguous")
        }
        provider_xai::FailureClass::CredentialPermanent
        | provider_xai::FailureClass::ConfigurationPermanent
        | provider_xai::FailureClass::UserActionRequired
        | provider_xai::FailureClass::Security
        | provider_xai::FailureClass::Unsupported => {
            AdminServiceError::invalid("xAI OAuth operation was rejected")
        }
    }
}

fn map_xai_import_error(error: provider_xai::GrokOAuthImportError) -> AdminServiceError {
    match error {
        provider_xai::GrokOAuthImportError::OAuth(error) => map_xai_oauth_error(error),
        provider_xai::GrokOAuthImportError::InvalidField(_) => {
            AdminServiceError::invalid("xAI OAuth import is invalid")
        }
    }
}

fn map_xai_repository_error(
    error: provider_xai::GrokCredentialRepositoryError,
) -> AdminServiceError {
    use provider_xai::GrokCredentialRepositoryError as Error;
    match error {
        Error::InvalidInput(_) | Error::InvalidCredentialData | Error::WrongProviderKind => {
            AdminServiceError::invalid("xAI credential is invalid")
        }
        Error::CredentialNotFound => AdminServiceError::not_found("xAI credential was not found"),
        Error::StaleCredentialRevision | Error::IdentityRebind | Error::Conflict => {
            AdminServiceError::conflict("xAI credential conflicts with current state")
        }
        Error::RevisionOverflow | Error::Store => {
            AdminServiceError::unavailable("xAI credential store is unavailable")
        }
    }
}

use gateway_store::redis::CredentialLeaseRepository as _;

const PROVIDER_ACCOUNT_LEASE_TTL: Duration = Duration::from_secs(10 * 60);
const OAUTH_REFRESH_LEASE_TTL: Duration = Duration::from_secs(5 * 60);
const XAI_CATALOG_CACHE_TTL_SECONDS: u64 = 5 * 60;

/// Provider 调度和 OAuth refresh 共用的 Redis lease 组合器。
pub struct ProviderLeaseAdapter {
    repository: gateway_store::redis::RedisCredentialLeaseRepository,
    process_id: String,
    sequence: std::sync::atomic::AtomicU64,
    scheduling_cursors: std::sync::Mutex<BTreeMap<ProviderInstanceId, u64>>,
}

impl ProviderLeaseAdapter {
    #[must_use]
    pub fn new(repository: gateway_store::redis::RedisCredentialLeaseRepository) -> Self {
        Self {
            repository,
            process_id: format!("gateway_{}", Uuid::now_v7().simple()),
            sequence: std::sync::atomic::AtomicU64::new(0),
            scheduling_cursors: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    fn owner_id(&self, operation: &str) -> String {
        let sequence = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("{}:{operation}:{sequence}", self.process_id)
    }

    fn next_scheduling_cursor(&self, provider_instance_id: &ProviderInstanceId) -> Option<u64> {
        let mut cursors = self.scheduling_cursors.lock().ok()?;
        let cursor = cursors.entry(provider_instance_id.clone()).or_default();
        let current = *cursor;
        *cursor = cursor.wrapping_add(1);
        Some(current)
    }

    async fn scheduling_signals(
        &self,
        accounts: &[gateway_core::engine::credential::ProviderAccountId],
    ) -> Result<
        BTreeMap<
            gateway_core::engine::credential::ProviderAccountId,
            gateway_core::engine::credential::AccountRuntimeSignals,
        >,
        gateway_store::StoreError,
    > {
        let ids = accounts
            .iter()
            .map(|account| account.as_str().to_owned())
            .collect::<Vec<_>>();
        let signals = self.repository.credential_runtime_signals(&ids).await?;
        signals
            .into_iter()
            .map(|signal| {
                let account =
                    gateway_core::engine::credential::ProviderAccountId::new(signal.resource_id)
                        .map_err(|_| gateway_store::StoreError::InvalidData {
                            entity: "credential runtime signal",
                            message: "Redis returned an invalid account ID".to_owned(),
                        })?;
                Ok((
                    account,
                    gateway_core::engine::credential::AccountRuntimeSignals {
                        in_flight: signal.in_flight,
                        last_started_at: signal.last_started_at.map(Into::into),
                        quota_reset_at: None,
                        quota_remaining_rank: None,
                    },
                ))
            })
            .collect()
    }

    async fn scheduling_lease(
        &self,
        account_id: &gateway_core::engine::credential::ProviderAccountId,
        max_concurrent: u32,
        request_interval: Duration,
        ttl: Duration,
    ) -> Result<gateway_store::redis::CredentialSchedulingLeaseAcquisition, gateway_store::StoreError>
    {
        self.repository
            .try_acquire_scheduling_lease(&gateway_store::redis::CredentialSchedulingLeaseRequest {
                resource_id: account_id.as_str().to_owned(),
                owner_id: self.owner_id("request"),
                max_concurrent,
                request_interval,
                ttl,
            })
            .await
    }

    async fn refresh_lease(
        &self,
        account_id: &gateway_core::engine::credential::ProviderAccountId,
    ) -> Result<Option<gateway_store::redis::CredentialLeaseGuard>, gateway_store::StoreError> {
        let request = gateway_store::redis::CredentialLeaseRequest {
            scope: gateway_store::redis::CredentialLeaseScope::OAuthRefresh,
            resource_id: account_id.as_str().to_owned(),
            owner_id: self.owner_id("refresh"),
            ttl: OAUTH_REFRESH_LEASE_TTL,
        };
        self.repository.try_acquire_guard(request).await
    }
}

fn request_lease_ttl(deadline: SystemTime) -> Option<Duration> {
    deadline
        .duration_since(SystemTime::now())
        .ok()
        .filter(|remaining| !remaining.is_zero())
        .map(|remaining| remaining.min(PROVIDER_ACCOUNT_LEASE_TTL))
}

#[async_trait]
impl provider_openai::credential::CredentialLeaseCoordinator for ProviderLeaseAdapter {
    async fn runtime_signals(
        &self,
        accounts: &[gateway_core::engine::credential::ProviderAccountId],
    ) -> Result<
        BTreeMap<
            gateway_core::engine::credential::ProviderAccountId,
            gateway_core::engine::credential::AccountRuntimeSignals,
        >,
        provider_openai::credential::CredentialLeaseCoordinatorError,
    > {
        self.scheduling_signals(accounts)
            .await
            .map_err(|_| provider_openai::credential::CredentialLeaseCoordinatorError::Unavailable)
    }

    fn next_round_robin_cursor(
        &self,
        provider_instance_id: &ProviderInstanceId,
    ) -> Result<u64, provider_openai::credential::CredentialLeaseCoordinatorError> {
        self.next_scheduling_cursor(provider_instance_id)
            .ok_or(provider_openai::credential::CredentialLeaseCoordinatorError::Unavailable)
    }

    async fn try_acquire(
        &self,
        request: provider_openai::credential::CredentialLeaseRequest,
    ) -> Result<
        provider_openai::credential::LeaseAcquisition,
        provider_openai::credential::CredentialLeaseCoordinatorError,
    > {
        let ttl = request_lease_ttl(request.deadline)
            .ok_or(provider_openai::credential::CredentialLeaseCoordinatorError::Unavailable)?;
        match self
            .scheduling_lease(
                &request.account_id,
                request.max_concurrent,
                request.request_interval,
                ttl,
            )
            .await
            .map_err(|_| {
                provider_openai::credential::CredentialLeaseCoordinatorError::Unavailable
            })? {
            gateway_store::redis::CredentialSchedulingLeaseAcquisition::Acquired(guard) => Ok(
                provider_openai::credential::LeaseAcquisition::Acquired(Box::new(guard)),
            ),
            gateway_store::redis::CredentialSchedulingLeaseAcquisition::Busy { retry_after } => {
                Ok(provider_openai::credential::LeaseAcquisition::Busy { retry_after })
            }
        }
    }
}

#[async_trait]
impl provider_xai::GrokCredentialLeaseCoordinator for ProviderLeaseAdapter {
    async fn load_scheduling_state(
        &self,
        provider_instance_id: &gateway_core::routing::ProviderInstanceId,
        accounts: &[gateway_core::engine::credential::ProviderAccountId],
    ) -> Result<
        provider_xai::GrokAccountSchedulingState,
        provider_xai::GrokCredentialLeaseCoordinatorError,
    > {
        let signals = self
            .scheduling_signals(accounts)
            .await
            .map_err(|_| provider_xai::GrokCredentialLeaseCoordinatorError::Unavailable)?;
        Ok(provider_xai::GrokAccountSchedulingState {
            signals,
            sticky_account: None,
            round_robin_cursor: self
                .next_scheduling_cursor(provider_instance_id)
                .ok_or(provider_xai::GrokCredentialLeaseCoordinatorError::Unavailable)?,
        })
    }

    async fn try_acquire(
        &self,
        request: &provider_xai::GrokCredentialLeaseRequest,
    ) -> Result<
        provider_xai::GrokCredentialLeaseAcquisition,
        provider_xai::GrokCredentialLeaseCoordinatorError,
    > {
        match self
            .scheduling_lease(
                &request.account_id,
                request.max_concurrent_per_account,
                request.request_interval,
                PROVIDER_ACCOUNT_LEASE_TTL,
            )
            .await
            .map_err(|_| provider_xai::GrokCredentialLeaseCoordinatorError::Unavailable)?
        {
            gateway_store::redis::CredentialSchedulingLeaseAcquisition::Acquired(guard) => Ok(
                provider_xai::GrokCredentialLeaseAcquisition::Acquired(Box::new(guard)),
            ),
            gateway_store::redis::CredentialSchedulingLeaseAcquisition::Busy { retry_after } => {
                Ok(provider_xai::GrokCredentialLeaseAcquisition::Unavailable { retry_after })
            }
        }
    }
}

#[async_trait]
impl provider_openai::credential::CodexRefreshLeaseCoordinator for ProviderLeaseAdapter {
    async fn try_acquire(
        &self,
        request: &provider_openai::credential::CodexRefreshLeaseRequest,
    ) -> Result<
        provider_openai::credential::CodexRefreshLeaseAcquisition,
        provider_openai::credential::CodexRefreshLeaseError,
    > {
        self.refresh_lease(&request.account_id)
            .await
            .map(|guard| match guard {
                Some(guard) => provider_openai::credential::CodexRefreshLeaseAcquisition::Acquired(
                    Box::new(guard),
                ),
                None => provider_openai::credential::CodexRefreshLeaseAcquisition::Unavailable,
            })
            .map_err(|_| provider_openai::credential::CodexRefreshLeaseError::Unavailable)
    }
}

#[async_trait]
impl provider_xai::GrokRefreshLeaseCoordinator for ProviderLeaseAdapter {
    async fn try_acquire(
        &self,
        request: &provider_xai::GrokRefreshLeaseRequest,
    ) -> Result<provider_xai::GrokRefreshLeaseAcquisition, provider_xai::GrokRefreshLeaseError>
    {
        self.refresh_lease(&request.account_id)
            .await
            .map(|guard| match guard {
                Some(guard) => provider_xai::GrokRefreshLeaseAcquisition::Acquired(Box::new(guard)),
                None => provider_xai::GrokRefreshLeaseAcquisition::Unavailable,
            })
            .map_err(|_| provider_xai::GrokRefreshLeaseError::Unavailable)
    }
}

/// xAI Provider-owned catalog 与 Redis opaque cache 的唯一转换边界。
pub struct XaiCatalogCacheAdapter {
    repository: Arc<dyn gateway_store::redis::ProviderAccountCatalogCacheRepository>,
}

impl XaiCatalogCacheAdapter {
    #[must_use]
    pub const fn new(
        repository: Arc<dyn gateway_store::redis::ProviderAccountCatalogCacheRepository>,
    ) -> Self {
        Self { repository }
    }

    fn key(
        account_id: &gateway_core::engine::credential::ProviderAccountId,
        revision: gateway_core::engine::credential::CredentialRevision,
    ) -> Result<
        gateway_store::redis::ProviderAccountCatalogCacheKey,
        provider_xai::GrokCatalogCacheError,
    > {
        Ok(gateway_store::redis::ProviderAccountCatalogCacheKey {
            provider_kind: "xai".to_owned(),
            provider_account_id: account_id.as_str().to_owned(),
            credential_revision: Revision::new(revision.get())
                .map_err(|_| provider_xai::GrokCatalogCacheError::InvalidData)?,
        })
    }
}

#[async_trait]
impl provider_xai::GrokCredentialCatalogCache for XaiCatalogCacheAdapter {
    async fn replace(
        &self,
        catalog: provider_xai::GrokAccountCatalog,
    ) -> Result<(), provider_xai::GrokCatalogCacheError> {
        let mut document = serde_json::Map::new();
        document.insert("version".to_owned(), serde_json::Value::from(1));
        document.insert(
            "observedAt".to_owned(),
            serde_json::Value::String(catalog.observed_at().to_rfc3339()),
        );
        if let Some(etag) = catalog.seed().etag() {
            document.insert(
                "etag".to_owned(),
                serde_json::Value::String(etag.to_owned()),
            );
        }
        document.insert(
            "models".to_owned(),
            serde_json::Value::Array(
                catalog
                    .seed()
                    .models()
                    .iter()
                    .cloned()
                    .map(serde_json::Value::String)
                    .collect(),
            ),
        );
        self.repository
            .replace_provider_account_catalog(
                &Self::key(catalog.account_id(), catalog.revision())?,
                &gateway_core::engine::credential::OpaqueProviderData::new(document),
                XAI_CATALOG_CACHE_TTL_SECONDS,
            )
            .await
            .map_err(|_| provider_xai::GrokCatalogCacheError::Unavailable)
    }

    async fn read(
        &self,
        account_id: &gateway_core::engine::credential::ProviderAccountId,
        revision: gateway_core::engine::credential::CredentialRevision,
    ) -> Result<Option<provider_xai::GrokAccountCatalog>, provider_xai::GrokCatalogCacheError> {
        let Some(document) = self
            .repository
            .get_provider_account_catalog(&Self::key(account_id, revision)?)
            .await
            .map_err(|_| provider_xai::GrokCatalogCacheError::Unavailable)?
        else {
            return Ok(None);
        };
        let mut fields = document.into_inner();
        if fields.remove("version").and_then(|value| value.as_u64()) != Some(1) {
            return Err(provider_xai::GrokCatalogCacheError::InvalidData);
        }
        let observed_at = fields
            .remove("observedAt")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
            .map(|value| value.with_timezone(&Utc))
            .ok_or(provider_xai::GrokCatalogCacheError::InvalidData)?;
        let etag = match fields.remove("etag") {
            None => None,
            Some(serde_json::Value::String(value)) => Some(value),
            Some(_) => return Err(provider_xai::GrokCatalogCacheError::InvalidData),
        };
        let models = fields
            .remove("models")
            .and_then(|value| value.as_array().cloned())
            .ok_or(provider_xai::GrokCatalogCacheError::InvalidData)?
            .into_iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or(provider_xai::GrokCatalogCacheError::InvalidData)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !fields.is_empty() {
            return Err(provider_xai::GrokCatalogCacheError::InvalidData);
        }
        let seed = provider_xai::GrokCredentialCatalogSeed::new(models, etag)
            .map_err(|_| provider_xai::GrokCatalogCacheError::InvalidData)?;
        Ok(Some(provider_xai::GrokAccountCatalog::new(
            account_id.clone(),
            revision,
            observed_at,
            seed,
        )))
    }

    async fn permits(
        &self,
        account_id: &gateway_core::engine::credential::ProviderAccountId,
        revision: gateway_core::engine::credential::CredentialRevision,
        model: &str,
    ) -> Result<bool, provider_xai::GrokCatalogCacheError> {
        Ok(self
            .read(account_id, revision)
            .await?
            .is_some_and(|catalog| catalog.seed().permits(model)))
    }
}

use gateway_api::admin::system::{
    SystemAdminError, SystemAdminErrorKind, SystemUpdateEvent, SystemUpdateEventStream,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken as TokioCancellationToken;

const SYSTEM_UPDATE_APP_BINARY_NAME: &str = "codex-proxy-rs";
const SYSTEM_UPDATE_DEFAULT_WEB_DIST_DIR: &str = "/app/web/dist";
const SYSTEM_UPDATE_GITHUB_API_BASE: &str = "https://api.github.com/repos";
const SYSTEM_UPDATE_CACHE_TTL: Duration = Duration::from_secs(20 * 60);
const SYSTEM_UPDATE_MAX_DOWNLOAD_SIZE: u64 = 500 * 1024 * 1024;
const SYSTEM_UPDATE_MAX_CHECKSUM_SIZE: u64 = 1024 * 1024;
const SYSTEM_UPDATE_MAX_EXTRACTED_SIZE: u64 = 1024 * 1024 * 1024;
const SYSTEM_UPDATE_MAX_ARCHIVE_FILES: usize = 20_000;
const SYSTEM_UPDATE_RESTART_DELAY_ENV: &str = "CPR_RESTART_DELAY_MS";
const SYSTEM_UPDATE_REPLACEMENT_START_DELAY_MS: &str = "1200";

/// 系统更新运行参数；生产从既有 `CPR_UPDATE_*` 环境变量装配。
#[derive(Debug, Clone)]
pub struct SystemUpdateConfig {
    pub version: String,
    pub git_sha: String,
    pub build_time: String,
    pub deployment_mode: String,
    pub build_type: String,
    pub update_channel: String,
    pub update_repository: Option<String>,
    pub github_api_base: String,
    pub executable_path: Option<PathBuf>,
    pub web_dist_dir: PathBuf,
    pub update_state_file: PathBuf,
    pub update_lock_file: PathBuf,
    pub update_temp_dir: PathBuf,
    pub self_restart_enabled: bool,
}

impl SystemUpdateConfig {
    fn from_environment() -> Self {
        let update_repository = system_update_environment_value("CPR_UPDATE_REPOSITORY");
        let deployment_mode = system_update_environment_value("CPR_DEPLOYMENT_MODE")
            .unwrap_or_else(|| "source".to_owned());
        let executable_path = system_update_environment_value("CPR_UPDATE_EXE_PATH")
            .map(PathBuf::from)
            .or_else(|| {
                (deployment_mode == "docker")
                    .then(|| PathBuf::from("/app/bin").join(SYSTEM_UPDATE_APP_BINARY_NAME))
            });
        let update_state_file = system_update_environment_value("CPR_UPDATE_STATE_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/app/.runtime/data/update-state.json"));
        let update_lock_file = system_update_environment_value("CPR_UPDATE_LOCK_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| update_state_file.with_extension("lock"));
        let update_temp_dir = system_update_environment_value("CPR_UPDATE_TEMP_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| system_update_default_temp_dir(&update_state_file));
        Self {
            version: option_env!("CPR_VERSION")
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .to_owned(),
            git_sha: option_env!("CPR_GIT_SHA").unwrap_or("unknown").to_owned(),
            build_time: option_env!("CPR_BUILD_TIME")
                .unwrap_or("unknown")
                .to_owned(),
            deployment_mode,
            build_type: option_env!("CPR_BUILD_TYPE").unwrap_or("source").to_owned(),
            update_channel: system_update_environment_value("CPR_UPDATE_CHANNEL")
                .unwrap_or_else(|| "stable".to_owned()),
            update_repository,
            github_api_base: system_update_environment_value("CPR_GITHUB_API_BASE")
                .unwrap_or_else(|| SYSTEM_UPDATE_GITHUB_API_BASE.to_owned()),
            executable_path,
            web_dist_dir: system_update_environment_value("CPR_WEB_DIST_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(SYSTEM_UPDATE_DEFAULT_WEB_DIST_DIR)),
            update_state_file,
            update_lock_file,
            update_temp_dir,
            self_restart_enabled: system_update_environment_value("CPR_ENABLE_SELF_RESTART")
                .as_deref()
                == Some("true"),
        }
    }

    fn update_support_error(&self) -> Option<String> {
        if self.build_type != "release" {
            return Some("一键更新需要正式构建包".to_owned());
        }
        let Some(repository) = self.update_repository.as_deref() else {
            return Some("检查更新需要配置 CPR_UPDATE_REPOSITORY".to_owned());
        };
        if let Err(error) = system_update_validate_repository(repository) {
            return Some(error.to_string());
        }
        if let Err(error) = system_update_validate_github_api_base(&self.github_api_base) {
            return Some(error);
        }
        None
    }

    fn release_cache_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.update_repository.as_deref().unwrap_or_default(),
            self.github_api_base,
            self.version,
            self.deployment_mode,
            self.build_type,
            self.update_channel,
        )
    }

    fn executable_path(&self) -> Result<PathBuf, SystemAdminError> {
        if let Some(path) = &self.executable_path {
            return Ok(path.clone());
        }
        env::current_exe()
            .and_then(fs::canonicalize)
            .map_err(system_update_internal_with("Failed to resolve executable"))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemUpdateInfo {
    current_version: String,
    latest_version: String,
    has_update: bool,
    deployment_mode: String,
    deployment_mode_label: String,
    build_type: String,
    build_type_label: String,
    release_url: Option<String>,
    notes: Option<String>,
    cached: bool,
    update_supported: bool,
    unsupported_reason: Option<String>,
    warning: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedSystemUpdateInfo {
    key: String,
    info: SystemUpdateInfo,
    cached_at: std::time::Instant,
}

struct SystemReleaseCache {
    entry: AsyncMutex<Option<CachedSystemUpdateInfo>>,
}

impl Default for SystemReleaseCache {
    fn default() -> Self {
        Self {
            entry: AsyncMutex::const_new(None),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct SystemGitHubRelease {
    tag_name: String,
    name: Option<String>,
    body: Option<String>,
    html_url: Option<String>,
    prerelease: bool,
    #[serde(default)]
    assets: Vec<SystemGitHubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct SystemGitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemVersionData {
    version: String,
    git_sha: String,
    build_time: String,
    deployment_mode: String,
    deployment_mode_label: String,
    update_channel: String,
    latest_version: String,
    has_update: bool,
    update_cached: bool,
    update_warning: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemUpdateStartedData {
    operation_id: String,
    deployment_mode: String,
    message: String,
    need_restart: bool,
    target_version: String,
}

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct SystemUpdateStatusData {
    previous_version: Option<String>,
    current_version: Option<String>,
    #[serde(default)]
    operation: SystemOperationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum SystemOperationKind {
    Update,
    Rollback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
enum SystemOperationStatus {
    #[default]
    Idle,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemOperationState {
    operation_id: Option<String>,
    kind: Option<SystemOperationKind>,
    status: SystemOperationStatus,
    target_version: Option<String>,
    message: Option<String>,
    error: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
}

impl Default for SystemOperationState {
    fn default() -> Self {
        Self {
            operation_id: None,
            kind: None,
            status: SystemOperationStatus::Idle,
            target_version: None,
            message: None,
            error: None,
            started_at: None,
            finished_at: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum SystemUpdateLogLevel {
    Info,
    Success,
    Warning,
    Error,
}

struct SystemUpdateEventSender {
    sender: broadcast::Sender<(String, serde_json::Value)>,
    sequence: AtomicU64,
}

impl Default for SystemUpdateEventSender {
    fn default() -> Self {
        let (sender, _receiver) = broadcast::channel(256);
        Self {
            sender,
            sequence: AtomicU64::new(0),
        }
    }
}

impl SystemUpdateEventSender {
    fn subscribe(&self) -> broadcast::Receiver<(String, serde_json::Value)> {
        self.sender.subscribe()
    }

    fn emit(
        &self,
        level: SystemUpdateLogLevel,
        operation_id: Option<&str>,
        step: Option<&str>,
        message: impl Into<String>,
    ) {
        self.emit_with_terminal(level, operation_id, step, message, false);
    }

    fn emit_terminal(
        &self,
        level: SystemUpdateLogLevel,
        operation_id: Option<&str>,
        step: Option<&str>,
        message: impl Into<String>,
    ) {
        self.emit_with_terminal(level, operation_id, step, message, true);
    }

    fn emit_with_terminal(
        &self,
        level: SystemUpdateLogLevel,
        operation_id: Option<&str>,
        step: Option<&str>,
        message: impl Into<String>,
        terminal: bool,
    ) {
        let now = Utc::now();
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let id = format!(
            "update-log-{}-{sequence}",
            now.timestamp_nanos_opt()
                .unwrap_or_else(|| now.timestamp_millis())
        );
        let data = serde_json::json!({
            "operationId": operation_id,
            "level": level,
            "step": step,
            "message": message.into(),
            "terminal": terminal,
            "at": now.to_rfc3339(),
        });
        let _ = self.sender.send((id, data));
    }
}

/// 当前进程版本、更新、回滚与真实 shutdown 信号的系统端口。
pub struct ProcessSystemAdminService {
    shutdown: TokioCancellationToken,
    events: SystemUpdateEventSender,
    config: SystemUpdateConfig,
    operation_lock: AsyncMutex<()>,
    release_cache: SystemReleaseCache,
}

impl ProcessSystemAdminService {
    #[must_use]
    pub fn new(shutdown: TokioCancellationToken) -> Self {
        Self::with_config(shutdown, SystemUpdateConfig::from_environment())
    }

    /// 使用显式配置装配系统更新端口，供组合根和黑盒边界测试使用。
    #[must_use]
    pub fn with_config(shutdown: TokioCancellationToken, config: SystemUpdateConfig) -> Self {
        Self {
            shutdown,
            events: SystemUpdateEventSender::default(),
            config,
            operation_lock: AsyncMutex::const_new(()),
            release_cache: SystemReleaseCache::default(),
        }
    }

    async fn version_value(&self) -> Result<serde_json::Value, SystemAdminError> {
        let info =
            system_update_check_latest_release(&self.release_cache, &self.config, false).await;
        system_update_json_value(SystemVersionData {
            version: self.config.version.clone(),
            git_sha: self.config.git_sha.clone(),
            build_time: self.config.build_time.clone(),
            deployment_mode: self.config.deployment_mode.clone(),
            deployment_mode_label: deployment_mode_label(&self.config.deployment_mode).to_owned(),
            update_channel: self.config.update_channel.clone(),
            latest_version: info.latest_version,
            has_update: info.has_update,
            update_cached: info.cached,
            update_warning: info.warning,
        })
    }

    async fn perform_update_inner(
        &self,
        target_version: Option<String>,
    ) -> Result<SystemUpdateStartedData, SystemAdminError> {
        let _operation_guard = self
            .operation_lock
            .try_lock()
            .map_err(|_| system_conflict("System update already running"))?;
        if let Some(reason) = self.config.update_support_error() {
            self.events.emit_terminal(
                SystemUpdateLogLevel::Error,
                None,
                Some("preflight"),
                reason.clone(),
            );
            return Err(system_conflict(reason));
        }
        let confirmed_target = system_update_confirmed_target(target_version)?;
        let repository = self
            .config
            .update_repository
            .as_deref()
            .ok_or_else(|| system_conflict("检查更新需要配置 CPR_UPDATE_REPOSITORY"))?;

        self.events.emit(
            SystemUpdateLogLevel::Info,
            None,
            Some("release"),
            "正在获取最新 Release 信息",
        );
        let release = system_update_fetch_latest_release(&self.config.github_api_base, repository)
            .await
            .inspect_err(|error| {
                self.events.emit_terminal(
                    SystemUpdateLogLevel::Error,
                    None,
                    Some("release"),
                    error.to_string(),
                );
            })?;
        let info = system_update_info_from_release(&self.config, release.clone());
        if info.latest_version != confirmed_target {
            let message = format!(
                "远端最新版本已变更为 v{}，请重新检查并确认",
                info.latest_version
            );
            self.events.emit_terminal(
                SystemUpdateLogLevel::Warning,
                None,
                Some("release"),
                message.clone(),
            );
            return Err(system_conflict(message));
        }
        if !info.has_update {
            self.events.emit_terminal(
                SystemUpdateLogLevel::Warning,
                None,
                Some("release"),
                "当前版本已是最新",
            );
            return Err(system_conflict("Already up to date"));
        }

        let target_version = info.latest_version;
        let operation_id = system_update_operation_id("update");
        let file_lock = SystemUpdateFileLock::acquire(&self.config.update_lock_file)?;
        system_update_set_operation_running(
            &self.config.update_state_file,
            &operation_id,
            SystemOperationKind::Update,
            Some(&target_version),
            &self.config.version,
        )?;
        self.events.emit(
            SystemUpdateLogLevel::Info,
            Some(&operation_id),
            Some("prepare"),
            format!("准备更新到 v{target_version}"),
        );
        let result = self
            .perform_release_update(&release, &target_version, &operation_id)
            .await;
        match &result {
            Ok(()) => self.events.emit_terminal(
                SystemUpdateLogLevel::Success,
                Some(&operation_id),
                Some("done"),
                "更新文件已替换，等待服务重启生效",
            ),
            Err(error) => self.events.emit_terminal(
                SystemUpdateLogLevel::Error,
                Some(&operation_id),
                Some("failed"),
                error.to_string(),
            ),
        }
        system_update_finish_operation(
            &self.config.update_state_file,
            &operation_id,
            SystemOperationKind::Update,
            result.as_ref().ok().map(|()| target_version.clone()),
            result.as_ref().err().map(ToString::to_string),
        );
        drop(file_lock);
        result?;

        Ok(SystemUpdateStartedData {
            operation_id,
            deployment_mode: self.config.deployment_mode.clone(),
            message: "更新完成，请重启服务。".to_owned(),
            need_restart: true,
            target_version,
        })
    }

    async fn perform_release_update(
        &self,
        release: &SystemGitHubRelease,
        version: &str,
        operation_id: &str,
    ) -> Result<(), SystemAdminError> {
        self.events.emit(
            SystemUpdateLogLevel::Info,
            Some(operation_id),
            Some("asset"),
            "正在选择匹配当前平台的更新包",
        );
        let archive = system_update_select_release_archive(release, version)?;
        self.events.emit(
            SystemUpdateLogLevel::Info,
            Some(operation_id),
            Some("asset"),
            format!(
                "已选择更新包 {} ({})",
                archive.name,
                system_update_format_bytes(archive.size)
            ),
        );
        system_update_validate_download_url(
            &archive.browser_download_url,
            &self.config.github_api_base,
        )?;
        if archive.size == 0 || archive.size > SYSTEM_UPDATE_MAX_DOWNLOAD_SIZE {
            return Err(system_invalid("Release archive has an invalid size"));
        }
        let checksum = release
            .assets
            .iter()
            .find(|asset| asset.name == "checksums.txt")
            .ok_or_else(|| system_bad_gateway("Release checksums.txt is required"))?;
        if checksum.size > SYSTEM_UPDATE_MAX_CHECKSUM_SIZE {
            return Err(system_invalid("Release checksums.txt is too large"));
        }
        system_update_validate_download_url(
            &checksum.browser_download_url,
            &self.config.github_api_base,
        )?;

        self.events.emit(
            SystemUpdateLogLevel::Info,
            Some(operation_id),
            Some("prepare"),
            "正在创建临时更新目录",
        );
        fs::create_dir_all(&self.config.update_temp_dir).map_err(system_update_internal_with(
            "Failed to prepare update temp dir",
        ))?;
        let update_temp_root = fs::canonicalize(&self.config.update_temp_dir).map_err(
            system_update_internal_with("Failed to resolve update temp dir"),
        )?;
        let temp_dir = system_update_unique_temp_dir(&update_temp_root)?;
        let temp_guard = SystemUpdateTempDir::new(temp_dir.clone());
        let archive_path = temp_dir.join(&archive.name);

        self.events.emit(
            SystemUpdateLogLevel::Info,
            Some(operation_id),
            Some("download"),
            "开始下载更新包",
        );
        system_update_download_file(
            &archive.browser_download_url,
            &archive_path,
            archive.size,
            &self.config.github_api_base,
            Some(SystemDownloadProgress {
                operation_id,
                events: &self.events,
            }),
        )
        .await?;
        self.events.emit(
            SystemUpdateLogLevel::Success,
            Some(operation_id),
            Some("download"),
            "更新包下载完成",
        );

        self.events.emit(
            SystemUpdateLogLevel::Info,
            Some(operation_id),
            Some("checksum"),
            "正在校验 checksum",
        );
        system_update_verify_checksum(
            &archive_path,
            &archive.name,
            &checksum.browser_download_url,
            &self.config.github_api_base,
        )
        .await?;
        self.events.emit(
            SystemUpdateLogLevel::Success,
            Some(operation_id),
            Some("checksum"),
            "checksum 校验通过",
        );

        self.events.emit(
            SystemUpdateLogLevel::Info,
            Some(operation_id),
            Some("extract"),
            "正在解压更新包",
        );
        let extracted = system_update_extract_release_archive(&archive_path, &temp_dir)?;
        self.events.emit(
            SystemUpdateLogLevel::Success,
            Some(operation_id),
            Some("extract"),
            "更新包解压完成",
        );

        self.events.emit(
            SystemUpdateLogLevel::Info,
            Some(operation_id),
            Some("replace"),
            "正在替换应用文件",
        );
        let executable_path = self.config.executable_path()?;
        system_update_replace_release_files(
            &executable_path,
            &self.config.web_dist_dir,
            extracted,
        )?;
        self.events.emit(
            SystemUpdateLogLevel::Success,
            Some(operation_id),
            Some("replace"),
            "应用文件替换完成",
        );
        drop(temp_guard);
        Ok(())
    }

    async fn rollback_inner(&self) -> Result<String, SystemAdminError> {
        let _operation_guard = self
            .operation_lock
            .try_lock()
            .map_err(|_| system_conflict("系统操作正在执行中"))?;
        if let Some(reason) = self.config.update_support_error() {
            return Err(system_conflict(reason));
        }
        let operation_id = system_update_operation_id("rollback");
        let file_lock = SystemUpdateFileLock::acquire(&self.config.update_lock_file)?;
        system_update_set_operation_running(
            &self.config.update_state_file,
            &operation_id,
            SystemOperationKind::Rollback,
            None,
            &self.config.version,
        )?;
        self.events.emit(
            SystemUpdateLogLevel::Info,
            Some(&operation_id),
            Some("rollback"),
            "正在恢复上一版本",
        );
        let result = system_update_rollback_release(&self.config);
        match &result {
            Ok(()) => self.events.emit_terminal(
                SystemUpdateLogLevel::Success,
                Some(&operation_id),
                Some("done"),
                "上一版本已恢复，等待服务重启生效",
            ),
            Err(error) => self.events.emit_terminal(
                SystemUpdateLogLevel::Error,
                Some(&operation_id),
                Some("failed"),
                error.to_string(),
            ),
        }
        system_update_finish_operation(
            &self.config.update_state_file,
            &operation_id,
            SystemOperationKind::Rollback,
            None,
            result.as_ref().err().map(ToString::to_string),
        );
        drop(file_lock);
        result?;
        Ok(operation_id)
    }
}

fn deployment_mode_label(mode: &str) -> &'static str {
    match mode {
        "docker" => "Docker",
        "binary" => "二进制",
        _ => "源码运行",
    }
}

#[async_trait]
impl SystemAdminService for ProcessSystemAdminService {
    async fn version(&self) -> Result<serde_json::Value, SystemAdminError> {
        self.version_value().await
    }

    async fn update_detail(&self, refresh: bool) -> Result<serde_json::Value, SystemAdminError> {
        system_update_json_value(
            system_update_check_latest_release(&self.release_cache, &self.config, refresh).await,
        )
    }

    fn update_events(&self) -> SystemUpdateEventStream {
        let receiver = self.events.subscribe();
        Box::pin(futures::stream::unfold(
            (receiver, false),
            |(mut receiver, close_after_send)| async move {
                if close_after_send {
                    return None;
                }
                loop {
                    match receiver.recv().await {
                        Ok((id, data)) => {
                            let terminal = data
                                .get("terminal")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false);
                            return Some((SystemUpdateEvent { id, data }, (receiver, terminal)));
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            },
        ))
    }

    async fn perform_update(
        &self,
        target_version: Option<String>,
    ) -> Result<serde_json::Value, SystemAdminError> {
        system_update_json_value(self.perform_update_inner(target_version).await?)
    }

    async fn update_status(&self) -> Result<serde_json::Value, SystemAdminError> {
        system_update_json_value(system_update_read_state(&self.config.update_state_file)?)
    }

    async fn rollback(&self) -> Result<serde_json::Value, SystemAdminError> {
        let operation_id = self.rollback_inner().await?;
        Ok(serde_json::json!({
            "message": "回滚完成，请重启服务。",
            "needRestart": true,
            "operationId": operation_id,
        }))
    }

    async fn restart(&self) -> Result<serde_json::Value, SystemAdminError> {
        if !self.config.self_restart_enabled {
            return Err(system_conflict(
                "自重启未启用，请设置 CPR_ENABLE_SELF_RESTART=true",
            ));
        }
        let message = if self.config.deployment_mode == "docker" {
            "已安排进程内重启"
        } else {
            system_update_spawn_replacement_process(&self.config)?;
            "已安排自重启"
        };
        let operation_id = system_update_operation_id("restart");
        let shutdown = self.shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            shutdown.cancel();
        });
        Ok(serde_json::json!({
            "message": message,
            "operationId": operation_id,
        }))
    }
}

fn system_conflict(message: impl Into<String>) -> SystemAdminError {
    SystemAdminError::new(SystemAdminErrorKind::Conflict, message)
}

fn system_invalid(message: impl Into<String>) -> SystemAdminError {
    SystemAdminError::new(SystemAdminErrorKind::Invalid, message)
}

fn system_bad_gateway(message: impl Into<String>) -> SystemAdminError {
    SystemAdminError::new(SystemAdminErrorKind::BadGateway, message)
}

fn system_internal(message: impl Into<String>) -> SystemAdminError {
    SystemAdminError::new(SystemAdminErrorKind::Internal, message)
}

fn system_update_internal_with<E: fmt::Display>(
    context: &'static str,
) -> impl FnOnce(E) -> SystemAdminError {
    move |error| system_internal(format!("{context}: {error}"))
}

fn system_update_bad_gateway_with<E: fmt::Display>(
    context: &'static str,
) -> impl FnOnce(E) -> SystemAdminError {
    move |error| system_bad_gateway(format!("{context}: {error}"))
}

fn system_update_json_value(value: impl Serialize) -> Result<serde_json::Value, SystemAdminError> {
    serde_json::to_value(value).map_err(system_update_internal_with(
        "Failed to encode system update response",
    ))
}

async fn system_update_check_latest_release(
    cache: &SystemReleaseCache,
    config: &SystemUpdateConfig,
    force: bool,
) -> SystemUpdateInfo {
    let Some(repository) = config.update_repository.as_deref() else {
        return system_update_unavailable_info(
            config,
            "检查更新需要配置 CPR_UPDATE_REPOSITORY".to_owned(),
            None,
        );
    };
    if let Err(error) = system_update_validate_repository(repository) {
        return system_update_unavailable_info(config, error.to_string(), None);
    }
    if let Err(reason) = system_update_validate_github_api_base(&config.github_api_base) {
        return system_update_unavailable_info(config, reason, None);
    }
    let cache_key = config.release_cache_key();
    if !force && let Some(info) = system_update_cached_release_info(cache, &cache_key).await {
        return info;
    }

    match system_update_fetch_latest_release(&config.github_api_base, repository).await {
        Ok(release) => {
            let info = system_update_info_from_release(config, release);
            system_update_cache_release_info(cache, cache_key, &info).await;
            info
        }
        Err(error) => system_update_cached_release_info(cache, &cache_key)
            .await
            .unwrap_or_else(|| {
                let mut info = system_update_base_info(config);
                info.update_supported = false;
                info.unsupported_reason = config.update_support_error();
                info.warning = Some(error.to_string());
                info
            }),
    }
}

fn system_update_base_info(config: &SystemUpdateConfig) -> SystemUpdateInfo {
    SystemUpdateInfo {
        current_version: config.version.clone(),
        latest_version: config.version.clone(),
        has_update: false,
        deployment_mode: config.deployment_mode.clone(),
        deployment_mode_label: deployment_mode_label(&config.deployment_mode).to_owned(),
        build_type: config.build_type.clone(),
        build_type_label: system_update_build_type_label(&config.build_type).to_owned(),
        release_url: None,
        notes: None,
        cached: false,
        update_supported: config.update_support_error().is_none(),
        unsupported_reason: config.update_support_error(),
        warning: None,
    }
}

fn system_update_unavailable_info(
    config: &SystemUpdateConfig,
    reason: String,
    warning: Option<String>,
) -> SystemUpdateInfo {
    let mut info = system_update_base_info(config);
    info.update_supported = false;
    info.unsupported_reason = Some(reason);
    info.warning = warning;
    info
}

async fn system_update_cached_release_info(
    cache: &SystemReleaseCache,
    cache_key: &str,
) -> Option<SystemUpdateInfo> {
    let entry = cache.entry.lock().await;
    let cached = entry.as_ref()?;
    if cached.key != cache_key || cached.cached_at.elapsed() > SYSTEM_UPDATE_CACHE_TTL {
        return None;
    }
    let mut info = cached.info.clone();
    drop(entry);
    info.cached = true;
    Some(info)
}

async fn system_update_cache_release_info(
    cache: &SystemReleaseCache,
    cache_key: String,
    info: &SystemUpdateInfo,
) {
    let mut entry = cache.entry.lock().await;
    *entry = Some(CachedSystemUpdateInfo {
        key: cache_key,
        info: info.clone(),
        cached_at: std::time::Instant::now(),
    });
}

async fn system_update_fetch_latest_release(
    api_base: &str,
    repository: &str,
) -> Result<SystemGitHubRelease, SystemAdminError> {
    system_update_validate_github_api_base(api_base).map_err(system_conflict)?;
    system_update_validate_repository(repository)?;
    let url = format!(
        "{}/{repository}/releases/latest",
        api_base.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(system_update_internal_with(
            "Failed to create release HTTP client",
        ))?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, SYSTEM_UPDATE_APP_BINARY_NAME)
        .send()
        .await
        .map_err(system_update_bad_gateway_with(
            "GitHub release check failed",
        ))?;
    let status = response.status();
    if !status.is_success() {
        return Err(system_bad_gateway(format!(
            "GitHub release check failed with {status}"
        )));
    }
    response
        .json::<SystemGitHubRelease>()
        .await
        .map_err(system_update_bad_gateway_with(
            "Invalid GitHub release response",
        ))
}

fn system_update_info_from_release(
    config: &SystemUpdateConfig,
    release: SystemGitHubRelease,
) -> SystemUpdateInfo {
    let latest_version = system_update_normalize_version_tag(&release.tag_name);
    let has_update = system_update_release_allowed(config, &release)
        && system_update_version_is_newer(&config.version, &latest_version).unwrap_or(false);
    let unsupported_reason = config.update_support_error();
    SystemUpdateInfo {
        current_version: config.version.clone(),
        latest_version,
        has_update,
        deployment_mode: config.deployment_mode.clone(),
        deployment_mode_label: deployment_mode_label(&config.deployment_mode).to_owned(),
        build_type: config.build_type.clone(),
        build_type_label: system_update_build_type_label(&config.build_type).to_owned(),
        release_url: release.html_url,
        notes: release.body.or(release.name),
        cached: false,
        update_supported: unsupported_reason.is_none(),
        unsupported_reason,
        warning: None,
    }
}

fn system_update_release_allowed(
    config: &SystemUpdateConfig,
    release: &SystemGitHubRelease,
) -> bool {
    config.update_channel != "stable" || !release.prerelease
}

fn system_update_version_is_newer(current: &str, latest: &str) -> Option<bool> {
    let current = semver::Version::parse(&system_update_normalize_version_tag(current)).ok()?;
    let latest = semver::Version::parse(&system_update_normalize_version_tag(latest)).ok()?;
    Some(latest > current)
}

fn system_update_confirmed_target(
    target_version: Option<String>,
) -> Result<String, SystemAdminError> {
    let Some(target_version) = target_version else {
        return Err(system_conflict("更新前需要确认目标版本"));
    };
    let target_version = system_update_normalize_version_tag(&target_version);
    if target_version.is_empty() {
        return Err(system_invalid("目标版本不能为空"));
    }
    semver::Version::parse(&target_version).map_err(|_| system_invalid("目标版本格式无效"))?;
    Ok(target_version)
}

fn system_update_normalize_version_tag(version: &str) -> String {
    version.trim().trim_start_matches('v').to_owned()
}

fn system_update_select_release_archive<'a>(
    release: &'a SystemGitHubRelease,
    version: &str,
) -> Result<&'a SystemGitHubAsset, SystemAdminError> {
    let os_aliases = system_update_platform_os_aliases();
    let arch_aliases = system_update_platform_arch_aliases();
    let normalized = system_update_normalize_version_tag(version);
    release
        .assets
        .iter()
        .find(|asset| {
            let name = asset.name.as_str();
            name.contains(SYSTEM_UPDATE_APP_BINARY_NAME)
                && name.contains(&normalized)
                && system_update_asset_matches_platform(name, os_aliases, arch_aliases)
                && !name.ends_with(".txt")
        })
        .or_else(|| {
            release.assets.iter().find(|asset| {
                let name = asset.name.as_str();
                system_update_asset_matches_platform(name, os_aliases, arch_aliases)
                    && !name.ends_with(".txt")
            })
        })
        .ok_or_else(|| {
            system_conflict(format!(
                "No compatible release archive found for {}/{}",
                env::consts::OS,
                env::consts::ARCH
            ))
        })
}

fn system_update_platform_os_aliases() -> &'static [&'static str] {
    match env::consts::OS {
        "macos" => &["macos", "darwin"],
        "windows" => &["windows", "win32"],
        "linux" => &["linux"],
        _ => &[env::consts::OS],
    }
}

fn system_update_platform_arch_aliases() -> &'static [&'static str] {
    match env::consts::ARCH {
        "x86_64" => &["x86_64", "amd64"],
        "aarch64" => &["aarch64", "arm64"],
        _ => &[env::consts::ARCH],
    }
}

fn system_update_asset_matches_platform(
    name: &str,
    os_aliases: &[&str],
    arch_aliases: &[&str],
) -> bool {
    os_aliases.iter().any(|alias| name.contains(alias))
        && arch_aliases.iter().any(|alias| name.contains(alias))
}

fn system_update_validate_repository(repository: &str) -> Result<(), SystemAdminError> {
    let mut segments = repository.split('/');
    let owner = segments.next().unwrap_or_default();
    let name = segments.next().unwrap_or_default();
    if owner.is_empty()
        || name.is_empty()
        || segments.next().is_some()
        || !owner.chars().all(system_update_repository_character)
        || !name.chars().all(system_update_repository_character)
    {
        return Err(system_conflict(
            "CPR_UPDATE_REPOSITORY must use the owner/repository form",
        ));
    }
    Ok(())
}

fn system_update_repository_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

fn system_update_validate_github_api_base(raw_url: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(raw_url)
        .map_err(|error| format!("Invalid GitHub API base: {error}"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("GitHub API base must not include credentials, query, or fragment".to_owned());
    }
    if url.path().trim_end_matches('/') != "/repos" {
        return Err("GitHub API base path must be /repos".to_owned());
    }
    if system_update_url_host_is_loopback(&url) {
        return if url.scheme() == "http" || url.scheme() == "https" {
            Ok(())
        } else {
            Err("Loopback GitHub API base must use HTTP or HTTPS".to_owned())
        };
    }
    if url.scheme() != "https" || url.host_str() != Some("api.github.com") {
        return Err("GitHub API base must be https://api.github.com/repos".to_owned());
    }
    Ok(())
}

fn system_update_validate_download_url(
    raw_url: &str,
    github_api_base: &str,
) -> Result<(), SystemAdminError> {
    let url = reqwest::Url::parse(raw_url)
        .map_err(|error| system_invalid(format!("Invalid download URL: {error}")))?;
    if system_update_local_download_allowed(&url, github_api_base) {
        return Ok(());
    }
    if url.scheme() != "https" {
        return Err(system_invalid("Only HTTPS release downloads are allowed"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| system_invalid("Download URL is missing host"))?;
    if system_update_github_download_host_allowed(host) {
        Ok(())
    } else {
        Err(system_invalid(format!(
            "Download host is not allowed: {host}"
        )))
    }
}

fn system_update_github_download_host_allowed(host: &str) -> bool {
    host == "github.com"
        || host.ends_with(".github.com")
        || host == "objects.githubusercontent.com"
        || host.ends_with(".objects.githubusercontent.com")
}

fn system_update_local_download_allowed(url: &reqwest::Url, github_api_base: &str) -> bool {
    if !matches!(url.scheme(), "http" | "https") || !system_update_url_host_is_loopback(url) {
        return false;
    }
    let Ok(api_base) = reqwest::Url::parse(github_api_base) else {
        return false;
    };
    system_update_url_host_is_loopback(&api_base)
        && url.scheme() == api_base.scheme()
        && url.host_str() == api_base.host_str()
        && url.port_or_known_default() == api_base.port_or_known_default()
}

fn system_update_url_host_is_loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host == "localhost"
            || host == "127.0.0.1"
            || host == "::1"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

fn system_update_download_client(
    github_api_base: &str,
    timeout: Duration,
) -> Result<reqwest::Client, SystemAdminError> {
    let github_api_base = github_api_base.to_owned();
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if system_update_download_url_allowed(attempt.url(), &github_api_base) {
                attempt.follow()
            } else {
                attempt.error(std::io::Error::other(
                    "release redirect target is not trusted",
                ))
            }
        }))
        .build()
        .map_err(system_update_internal_with(
            "Failed to create download HTTP client",
        ))
}

fn system_update_download_url_allowed(url: &reqwest::Url, github_api_base: &str) -> bool {
    system_update_local_download_allowed(url, github_api_base)
        || (url.scheme() == "https"
            && url
                .host_str()
                .is_some_and(system_update_github_download_host_allowed))
}

#[derive(Clone, Copy)]
struct SystemDownloadProgress<'a> {
    operation_id: &'a str,
    events: &'a SystemUpdateEventSender,
}

async fn system_update_download_file(
    url: &str,
    destination: &Path,
    expected_size: u64,
    github_api_base: &str,
    progress: Option<SystemDownloadProgress<'_>>,
) -> Result<(), SystemAdminError> {
    let client = system_update_download_client(github_api_base, Duration::from_secs(120))?;
    let mut response = client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(system_update_bad_gateway_with("Download failed"))?;
    if !response.status().is_success() {
        return Err(system_bad_gateway(format!(
            "Download failed with {}",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|size| size != expected_size)
    {
        return Err(system_bad_gateway(
            "Downloaded size does not match release metadata",
        ));
    }

    let mut file = fs::File::create(destination).map_err(system_update_internal_with(
        "Failed to create download file",
    ))?;
    let mut downloaded = 0_u64;
    let mut next_progress = 10_u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(system_update_bad_gateway_with("Download stream failed"))?
    {
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        if downloaded > expected_size || downloaded > SYSTEM_UPDATE_MAX_DOWNLOAD_SIZE {
            return Err(system_invalid("Download exceeds declared release size"));
        }
        file.write_all(&chunk)
            .map_err(system_update_internal_with("Failed to write download"))?;
        if let Some(progress) = progress {
            let percent = system_update_download_percent(downloaded, expected_size);
            if percent >= next_progress || downloaded == expected_size {
                progress.events.emit(
                    SystemUpdateLogLevel::Info,
                    Some(progress.operation_id),
                    Some("download"),
                    format!(
                        "已下载 {} / {} ({percent}%)",
                        system_update_format_bytes(downloaded),
                        system_update_format_bytes(expected_size)
                    ),
                );
                next_progress = percent.saturating_add(10);
            }
        }
    }
    file.sync_all()
        .map_err(system_update_internal_with("Failed to sync download"))?;
    if downloaded != expected_size {
        return Err(system_bad_gateway(
            "Downloaded size does not match release metadata",
        ));
    }
    Ok(())
}

async fn system_update_verify_checksum(
    file_path: &Path,
    file_name: &str,
    checksum_url: &str,
    github_api_base: &str,
) -> Result<(), SystemAdminError> {
    let client = system_update_download_client(github_api_base, Duration::from_secs(30))?;
    let mut response = client
        .get(checksum_url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(system_update_bad_gateway_with("Checksum download failed"))?;
    if !response.status().is_success() {
        return Err(system_bad_gateway(format!(
            "Checksum download failed with {}",
            response.status()
        )));
    }
    if response
        .content_length()
        .is_some_and(|size| size > SYSTEM_UPDATE_MAX_CHECKSUM_SIZE)
    {
        return Err(system_bad_gateway("Checksum document is too large"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(system_update_bad_gateway_with("Checksum read failed"))?
    {
        if body.len().saturating_add(chunk.len()) > SYSTEM_UPDATE_MAX_CHECKSUM_SIZE as usize {
            return Err(system_bad_gateway("Checksum document is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    let body = std::str::from_utf8(&body)
        .map_err(system_update_bad_gateway_with("Invalid checksum document"))?;
    let expected = body.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?;
        (Path::new(name).file_name()?.to_string_lossy() == file_name).then(|| hash.to_owned())
    });
    let expected = expected.ok_or_else(|| system_bad_gateway("Checksum not found"))?;
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(system_bad_gateway("Invalid SHA-256 checksum"));
    }
    let actual = system_update_sha256_file(file_path)?;
    if expected.eq_ignore_ascii_case(&actual) {
        Ok(())
    } else {
        Err(system_bad_gateway("Checksum mismatch"))
    }
}

fn system_update_sha256_file(path: &Path) -> Result<String, SystemAdminError> {
    use sha2::Digest as _;

    let mut file = fs::File::open(path)
        .map_err(system_update_internal_with("Failed to open checksum file"))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)
            .map_err(system_update_internal_with("Failed to read checksum file"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn system_update_download_percent(downloaded: u64, total_size: u64) -> u64 {
    if total_size == 0 {
        return 0;
    }
    downloaded
        .saturating_mul(100)
        .saturating_div(total_size)
        .min(100)
}

fn system_update_format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    if bytes >= MIB {
        return format!("{:.1} MiB", bytes as f64 / MIB as f64);
    }
    if bytes >= KIB {
        return format!("{:.1} KiB", bytes as f64 / KIB as f64);
    }
    format!("{bytes} B")
}

fn system_update_build_type_label(value: &str) -> &str {
    match value {
        "release" => "正式构建",
        "source" => "源码构建",
        "dev" => "开发构建",
        _ => value,
    }
}

#[derive(Debug)]
struct SystemExtractedRelease {
    binary_path: PathBuf,
    web_dist_dir: Option<PathBuf>,
}

fn system_update_extract_release_archive(
    archive_path: &Path,
    temp_dir: &Path,
) -> Result<SystemExtractedRelease, SystemAdminError> {
    let file = fs::File::open(archive_path)
        .map_err(system_update_internal_with("Failed to open archive"))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let binary_path = temp_dir.join(SYSTEM_UPDATE_APP_BINARY_NAME);
    let web_dist_dir = temp_dir.join("web-dist");
    let mut found_binary = false;
    let mut found_web = false;
    let mut extracted_size = 0_u64;
    let mut file_count = 0_usize;

    for entry in archive
        .entries()
        .map_err(system_update_internal_with("Failed to read archive"))?
    {
        let mut entry = entry.map_err(system_update_internal_with("Invalid archive entry"))?;
        let path = entry
            .path()
            .map_err(system_update_internal_with("Invalid archive path"))?
            .to_path_buf();
        if system_update_unsafe_archive_path(&path) {
            return Err(system_invalid("Unsafe archive path"));
        }
        if !entry.header().entry_type().is_file() {
            continue;
        }
        file_count = file_count.saturating_add(1);
        extracted_size = extracted_size.saturating_add(entry.header().size().unwrap_or(u64::MAX));
        if file_count > SYSTEM_UPDATE_MAX_ARCHIVE_FILES
            || extracted_size > SYSTEM_UPDATE_MAX_EXTRACTED_SIZE
        {
            return Err(system_invalid(
                "Release archive expands beyond safety limits",
            ));
        }

        if path
            .file_name()
            .is_some_and(|name| name == SYSTEM_UPDATE_APP_BINARY_NAME)
        {
            if found_binary {
                return Err(system_invalid(
                    "Release archive contains duplicate binaries",
                ));
            }
            entry
                .unpack(&binary_path)
                .map_err(system_update_internal_with("Failed to extract binary"))?;
            found_binary = true;
            continue;
        }

        if let Some(relative) = system_update_web_dist_relative_path(&path) {
            if relative.as_os_str().is_empty() {
                continue;
            }
            let target = web_dist_dir.join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(system_update_internal_with(
                    "Failed to create web asset dir",
                ))?;
            }
            entry
                .unpack(&target)
                .map_err(system_update_internal_with("Failed to extract web asset"))?;
            found_web = true;
        }
    }

    if !found_binary {
        return Err(system_invalid(
            "Release archive does not contain codex-proxy-rs",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(&binary_path, fs::Permissions::from_mode(0o755))
            .map_err(system_update_internal_with("Failed to chmod binary"))?;
    }
    Ok(SystemExtractedRelease {
        binary_path,
        web_dist_dir: found_web.then_some(web_dist_dir),
    })
}

fn system_update_replace_release_files(
    executable_path: &Path,
    web_dist_dir: &Path,
    extracted: SystemExtractedRelease,
) -> Result<(), SystemAdminError> {
    let web_backup = system_update_backup_path_for(web_dist_dir);
    let web_replaced = if let Some(new_web) = extracted.web_dist_dir {
        system_update_replace_dir(web_dist_dir, &web_backup, &new_web)?;
        true
    } else {
        false
    };

    let binary_backup = system_update_backup_path_for(executable_path);
    if binary_backup.exists()
        && let Err(error) = fs::remove_file(&binary_backup)
    {
        let mut rollback_errors = Vec::new();
        if web_replaced {
            system_update_collect_rollback_error(
                &mut rollback_errors,
                "restore web assets",
                system_update_restore_dir(web_dist_dir, &web_backup),
            );
        }
        return Err(system_update_error_with_rollback(
            "Failed to remove old binary backup",
            error,
            rollback_errors,
        ));
    }
    if let Err(error) = system_update_move_file(executable_path, &binary_backup) {
        let mut rollback_errors = Vec::new();
        if web_replaced {
            system_update_collect_rollback_error(
                &mut rollback_errors,
                "restore web assets",
                system_update_restore_dir(web_dist_dir, &web_backup),
            );
        }
        return Err(system_update_error_with_rollback(
            "Binary backup failed",
            error,
            rollback_errors,
        ));
    }
    if let Err(error) = system_update_move_file(&extracted.binary_path, executable_path) {
        let mut rollback_errors = Vec::new();
        if executable_path.exists() {
            system_update_collect_rollback_error(
                &mut rollback_errors,
                "remove partial replacement binary",
                fs::remove_file(executable_path),
            );
        }
        system_update_collect_rollback_error(
            &mut rollback_errors,
            "restore previous binary",
            system_update_move_file(&binary_backup, executable_path),
        );
        if web_replaced {
            system_update_collect_rollback_error(
                &mut rollback_errors,
                "restore web assets",
                system_update_restore_dir(web_dist_dir, &web_backup),
            );
        }
        return Err(system_update_error_with_rollback(
            "Binary replace failed",
            error,
            rollback_errors,
        ));
    }
    Ok(())
}

fn system_update_replace_dir(
    current: &Path,
    backup: &Path,
    replacement: &Path,
) -> Result<(), SystemAdminError> {
    if backup.exists() {
        fs::remove_dir_all(backup).map_err(system_update_internal_with(
            "Failed to remove old web backup",
        ))?;
    }
    if current.exists() {
        system_update_move_dir(current, backup)
            .map_err(system_update_internal_with("Failed to backup web assets"))?;
    }
    if let Err(error) = system_update_move_dir(replacement, current) {
        let mut rollback_errors = Vec::new();
        if backup.exists() {
            system_update_collect_rollback_error(
                &mut rollback_errors,
                "restore previous web assets",
                system_update_restore_dir(current, backup),
            );
        }
        return Err(system_update_error_with_rollback(
            "Failed to replace web assets",
            error,
            rollback_errors,
        ));
    }
    Ok(())
}

fn system_update_rollback_release(config: &SystemUpdateConfig) -> Result<(), SystemAdminError> {
    let executable_path = config.executable_path()?;
    let binary_backup = system_update_backup_path_for(&executable_path);
    if !binary_backup.exists() {
        return Err(system_conflict("No binary backup found for rollback"));
    }
    system_update_swap_file(&executable_path, &binary_backup)
        .map_err(system_update_internal_with("Binary rollback failed"))?;

    let web_backup = system_update_backup_path_for(&config.web_dist_dir);
    if web_backup.exists()
        && let Err(error) = system_update_swap_dir(&config.web_dist_dir, &web_backup)
    {
        let binary_restore = system_update_swap_file(&executable_path, &binary_backup);
        let mut rollback_errors = Vec::new();
        system_update_collect_rollback_error(
            &mut rollback_errors,
            "restore binary after web rollback failure",
            binary_restore,
        );
        return Err(system_update_error_with_rollback(
            "Web rollback failed",
            error,
            rollback_errors,
        ));
    }
    Ok(())
}

fn system_update_swap_file(current: &Path, backup: &Path) -> io::Result<()> {
    if !current.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("current binary is missing: {}", current.display()),
        ));
    }
    let swap = system_update_swap_path_for(current);
    if swap.exists() {
        fs::remove_file(&swap)?;
    }
    system_update_move_file(current, &swap)?;
    if let Err(error) = system_update_move_file(backup, current) {
        let _ = system_update_move_file(&swap, current);
        return Err(error);
    }
    if let Err(error) = system_update_move_file(&swap, backup) {
        let _ = system_update_move_file(current, &swap);
        let _ = system_update_move_file(backup, current);
        let _ = system_update_move_file(&swap, backup);
        return Err(error);
    }
    Ok(())
}

fn system_update_swap_dir(current: &Path, backup: &Path) -> io::Result<()> {
    if !current.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("current web directory is missing: {}", current.display()),
        ));
    }
    let swap = system_update_swap_path_for(current);
    if swap.exists() {
        fs::remove_dir_all(&swap)?;
    }
    system_update_move_dir(current, &swap)?;
    if let Err(error) = system_update_move_dir(backup, current) {
        let _ = system_update_move_dir(&swap, current);
        return Err(error);
    }
    if let Err(error) = system_update_move_dir(&swap, backup) {
        let _ = system_update_move_dir(current, &swap);
        let _ = system_update_move_dir(backup, current);
        let _ = system_update_move_dir(&swap, backup);
        return Err(error);
    }
    Ok(())
}

fn system_update_collect_rollback_error(
    errors: &mut Vec<String>,
    action: &'static str,
    result: io::Result<()>,
) {
    if let Err(error) = result {
        errors.push(format!("{action}: {error}"));
    }
}

fn system_update_error_with_rollback(
    context: &'static str,
    error: impl fmt::Display,
    rollback_errors: Vec<String>,
) -> SystemAdminError {
    if rollback_errors.is_empty() {
        return system_internal(format!("{context}: {error}"));
    }
    system_internal(format!(
        "{context}: {error}; rollback failed: {}",
        rollback_errors.join("; ")
    ))
}

fn system_update_restore_dir(current: &Path, backup: &Path) -> io::Result<()> {
    if current.exists() {
        fs::remove_dir_all(current)?;
    }
    if backup.exists() {
        system_update_move_dir(backup, current)?;
    }
    Ok(())
}

fn system_update_move_file(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            if let Err(copy_error) = fs::copy(from, to) {
                let _ = fs::remove_file(to);
                return Err(copy_error);
            }
            fs::remove_file(from)
        }
        Err(error) => Err(error),
    }
}

fn system_update_move_dir(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            if let Err(copy_error) = system_update_copy_dir_all(from, to) {
                let _ = fs::remove_dir_all(to);
                return Err(copy_error);
            }
            fs::remove_dir_all(from)
        }
        Err(error) => Err(error),
    }
}

fn system_update_copy_dir_all(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = to.join(entry.file_name());
        if file_type.is_dir() {
            system_update_copy_dir_all(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target)?;
            fs::set_permissions(&target, entry.metadata()?.permissions())?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("unsupported file type in {}", entry.path().display()),
            ));
        }
    }
    Ok(())
}

fn system_update_backup_path_for(path: &Path) -> PathBuf {
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".backup");
    PathBuf::from(backup)
}

fn system_update_swap_path_for(path: &Path) -> PathBuf {
    let mut swap = path.as_os_str().to_os_string();
    swap.push(".rollback-swap");
    PathBuf::from(swap)
}

fn system_update_web_dist_relative_path(path: &Path) -> Option<PathBuf> {
    let components = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_os_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for index in 0..components.len() {
        if components[index] == "web"
            && components
                .get(index + 1)
                .is_some_and(|value| value == "dist")
        {
            return Some(components[index + 2..].iter().collect());
        }
        if components[index] == "dist" {
            return Some(components[index + 1..].iter().collect());
        }
    }
    None
}

fn system_update_unsafe_archive_path(path: &Path) -> bool {
    path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::Prefix(_)
                    | std::path::Component::RootDir
            )
        })
}

struct SystemUpdateTempDir {
    path: PathBuf,
}

impl SystemUpdateTempDir {
    const fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for SystemUpdateTempDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.path.display(),
                error = %error,
                "清理系统更新临时目录失败"
            );
        }
    }
}

fn system_update_unique_temp_dir(parent: &Path) -> Result<PathBuf, SystemAdminError> {
    for attempt in 0..100_u8 {
        let path = parent.join(format!(
            ".codex-proxy-rs-update-{}-{attempt}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(system_internal(format!(
                    "Failed to create update temp dir: {error}"
                )));
            }
        }
    }
    Err(system_internal("Failed to create unique update temp dir"))
}

#[derive(Debug)]
struct SystemUpdateFileLock {
    path: PathBuf,
}

impl SystemUpdateFileLock {
    fn acquire(path: &Path) -> Result<Self, SystemAdminError> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(system_update_internal_with(
                "Failed to create update lock directory",
            ))?;
        }
        match Self::try_create(path) {
            Ok(()) => Ok(Self {
                path: path.to_path_buf(),
            }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !system_update_stale_lock(path)? {
                    return Err(system_conflict("System update already running"));
                }
                fs::remove_file(path).map_err(system_update_internal_with(
                    "Failed to remove stale update lock",
                ))?;
                match Self::try_create(path) {
                    Ok(()) => Ok(Self {
                        path: path.to_path_buf(),
                    }),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        Err(system_conflict("System update already running"))
                    }
                    Err(error) => Err(system_internal(format!(
                        "Failed to create update lock: {error}"
                    ))),
                }
            }
            Err(error) => Err(system_internal(format!(
                "Failed to create update lock: {error}"
            ))),
        }
    }

    fn try_create(path: &Path) -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        writeln!(
            file,
            "pid={}\ncreated_at={}",
            std::process::id(),
            Utc::now().to_rfc3339()
        )?;
        file.sync_all()
    }
}

impl Drop for SystemUpdateFileLock {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.path.display(),
                error = %error,
                "清理系统更新文件锁失败"
            );
        }
    }
}

fn system_update_stale_lock(path: &Path) -> Result<bool, SystemAdminError> {
    let metadata =
        fs::metadata(path).map_err(system_update_internal_with("Failed to read update lock"))?;
    let modified = metadata.modified().map_err(system_update_internal_with(
        "Failed to read update lock timestamp",
    ))?;
    modified
        .elapsed()
        .map(|age| age > Duration::from_secs(30 * 60))
        .map_err(system_update_internal_with(
            "Failed to calculate update lock age",
        ))
}

fn system_update_read_state(path: &Path) -> Result<SystemUpdateStatusData, SystemAdminError> {
    if !path.exists() {
        return Ok(SystemUpdateStatusData::default());
    }
    let data = fs::read_to_string(path)
        .map_err(system_update_internal_with("Failed to read update state"))?;
    serde_json::from_str(&data).map_err(system_update_internal_with("Invalid update state"))
}

fn system_update_write_state(
    path: &Path,
    state: &SystemUpdateStatusData,
) -> Result<(), SystemAdminError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(system_update_internal_with(
            "Failed to create update state directory",
        ))?;
    }
    let data = serde_json::to_vec_pretty(state)
        .map_err(system_update_internal_with("Failed to encode update state"))?;
    let temporary_path = system_update_state_temporary_path(path);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)?;
        file.write_all(&data)?;
        file.sync_all()?;
        fs::rename(&temporary_path, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result.map_err(system_update_internal_with("Failed to write update state"))
}

fn system_update_state_temporary_path(path: &Path) -> PathBuf {
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    PathBuf::from(temporary)
}

fn system_update_set_operation_running(
    path: &Path,
    operation_id: &str,
    kind: SystemOperationKind,
    version: Option<&str>,
    current_version: &str,
) -> Result<(), SystemAdminError> {
    let mut state = system_update_read_state(path)?;
    if state.current_version.is_none() {
        state.current_version = Some(current_version.to_owned());
    }
    state.operation = SystemOperationState {
        operation_id: Some(operation_id.to_owned()),
        kind: Some(kind),
        status: SystemOperationStatus::Running,
        target_version: version.map(ToOwned::to_owned),
        message: Some("operation running".to_owned()),
        error: None,
        started_at: Some(Utc::now().to_rfc3339()),
        finished_at: None,
    };
    system_update_write_state(path, &state)
}

fn system_update_finish_operation(
    path: &Path,
    operation_id: &str,
    kind: SystemOperationKind,
    version: Option<String>,
    error: Option<String>,
) {
    let mut state = match system_update_read_state(path) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(error = %error, "读取系统更新状态失败");
            return;
        }
    };
    if state.operation.operation_id.as_deref() != Some(operation_id) {
        return;
    }
    if let Some(error) = error {
        state.operation.status = SystemOperationStatus::Failed;
        state.operation.message = Some("operation failed".to_owned());
        state.operation.error = Some(error);
    } else {
        state.operation.status = SystemOperationStatus::Succeeded;
        state.operation.message = Some("operation succeeded".to_owned());
        state.operation.error = None;
        match kind {
            SystemOperationKind::Update => {
                state.previous_version = state.current_version.take();
                state.current_version = version.clone();
            }
            SystemOperationKind::Rollback => {
                let current = state.current_version.take();
                state.current_version = state.previous_version.take();
                state.previous_version = current;
            }
        }
        state.operation.target_version = version;
    }
    state.operation.finished_at = Some(Utc::now().to_rfc3339());
    if let Err(error) = system_update_write_state(path, &state) {
        tracing::warn!(error = %error, "写入系统更新状态失败");
    }
}

fn system_update_operation_id(kind: &str) -> String {
    format!("sysop-{kind}-{}", Utc::now().timestamp_millis())
}

fn system_update_spawn_replacement_process(
    config: &SystemUpdateConfig,
) -> Result<(), SystemAdminError> {
    let executable_path = config.executable_path()?;
    let metadata = fs::metadata(&executable_path).map_err(system_update_internal_with(
        "Failed to schedule replacement process",
    ))?;
    if !metadata.is_file() {
        return Err(system_internal(
            "Failed to schedule replacement process: executable path is not a file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(system_internal(
                "Failed to schedule replacement process: executable is not executable",
            ));
        }
    }

    let delay_ms = system_update_environment_value(SYSTEM_UPDATE_RESTART_DELAY_ENV)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(|| {
            SYSTEM_UPDATE_REPLACEMENT_START_DELAY_MS
                .parse::<u64>()
                .unwrap_or(1_200)
        });
    #[cfg(unix)]
    let mut command = {
        let delay = format!("{}.{:03}", delay_ms / 1_000, delay_ms % 1_000);
        let mut command = std::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep \"$1\"; shift; exec \"$@\"")
            .arg("codex-proxy-rs-restart")
            .arg(delay)
            .arg(&executable_path)
            .args(env::args_os().skip(1));
        command
    };
    #[cfg(not(unix))]
    let mut command = {
        let mut command = std::process::Command::new(&executable_path);
        command
            .args(env::args_os().skip(1))
            .env(SYSTEM_UPDATE_RESTART_DELAY_ENV, delay_ms.to_string());
        command
    };
    command.stdin(std::process::Stdio::null());
    command
        .spawn()
        .map(|_| ())
        .map_err(system_update_internal_with(
            "Failed to schedule replacement process",
        ))
}

fn system_update_default_temp_dir(state_file: &Path) -> PathBuf {
    state_file
        .parent()
        .map(|parent| parent.join("update-tmp"))
        .unwrap_or_else(|| env::temp_dir().join("codex-proxy-rs-update"))
}

fn system_update_environment_value(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// OpenAI 与 Admin router 共用的应用状态。
pub struct ApplicationState<O> {
    admin: Arc<AdminServices>,
    openai: O,
    health: Arc<HealthStatus>,
}

impl<O> ApplicationState<O> {
    #[must_use]
    pub const fn new(admin: Arc<AdminServices>, openai: O, health: Arc<HealthStatus>) -> Self {
        Self {
            admin,
            openai,
            health,
        }
    }
}

impl<O> Clone for ApplicationState<O>
where
    O: Clone,
{
    fn clone(&self) -> Self {
        Self {
            admin: Arc::clone(&self.admin),
            openai: self.openai.clone(),
            health: Arc::clone(&self.health),
        }
    }
}

impl<O> AdminSessionState for ApplicationState<O> {
    fn admin_session_resolver(&self) -> &dyn AdminSessionResolver {
        self.admin.sessions.as_ref()
    }
}

impl<O> AdminAuthState for ApplicationState<O> {
    fn admin_auth_service(&self) -> &dyn AdminAuthService {
        self.admin.sessions.as_ref()
    }
}

impl<O> AccountAdminState for ApplicationState<O> {
    fn account_admin_service(&self) -> &dyn AccountAdminService {
        self.admin.accounts.as_ref()
    }
}

impl<O> CatalogAdminState for ApplicationState<O> {
    fn catalog_admin_service(&self) -> &dyn CatalogAdminService {
        self.admin.catalog.as_ref()
    }
}

impl<O> ClientKeyAdminState for ApplicationState<O> {
    fn client_key_admin_service(&self) -> &dyn ClientKeyAdminService {
        self.admin.client_keys.as_ref()
    }
}

impl<O> CodexAdminState for ApplicationState<O> {
    fn codex_admin_service(&self) -> &dyn CodexAdminService {
        self.admin.codex.as_ref()
    }
}

impl<O> ObservabilityAdminState for ApplicationState<O> {
    fn observability_admin_service(&self) -> &dyn ObservabilityAdminService {
        self.admin.observability.as_ref()
    }
}

impl<O> AdminSettingsState for ApplicationState<O> {
    fn admin_settings_service(&self) -> &dyn AdminSettingsService {
        self.admin.settings.as_ref()
    }
}

impl<O> SystemAdminState for ApplicationState<O> {
    fn system_admin_service(&self) -> &dyn SystemAdminService {
        self.admin.system.as_ref()
    }
}

impl<O> XaiAdminState for ApplicationState<O> {
    fn xai_admin_service(&self) -> &dyn XaiAdminService {
        self.admin.xai.as_ref()
    }
}

impl<O> OpenAiApiState for ApplicationState<O>
where
    O: OpenAiClientService,
{
    type Service = O;

    fn openai_client_api(&self) -> Self::Service {
        self.openai.clone()
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroU32;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use gateway_api::openai::{ConnectionTask, auth::ClientApiKeyAuthError};
use gateway_core::{
    engine::{
        credential::{AccountSelectionPolicy, RotationStrategy},
        provider::ProviderRegistry,
    },
    policy::ClientPolicy,
    routing::{
        ConfigRevision, InstanceHealth, ProviderInstance, ProviderInstanceId, ProviderModel,
        PublicModelId, RuntimeSnapshot, UpstreamModelId,
    },
};
use gateway_store::postgres::{RuntimeSnapshotData, RuntimeSnapshotRepository};
use tokio_util::task::TaskTracker;

/// 一个配置 revision 的完整数据面快照。
#[derive(Debug)]
pub struct CompiledRuntimeSnapshot {
    routing: RuntimeSnapshot,
}

impl CompiledRuntimeSnapshot {
    #[must_use]
    pub const fn new(routing: RuntimeSnapshot) -> Self {
        Self { routing }
    }

    #[must_use]
    pub const fn routing(&self) -> &RuntimeSnapshot {
        &self.routing
    }
}

/// PostgreSQL 一致性事实与 Provider 实时目录的唯一快照编译器。
pub struct RuntimeSnapshotCompiler {
    repository: Arc<dyn RuntimeSnapshotRepository>,
    providers: ProviderRegistry,
}

impl RuntimeSnapshotCompiler {
    #[must_use]
    pub const fn new(
        repository: Arc<dyn RuntimeSnapshotRepository>,
        providers: ProviderRegistry,
    ) -> Self {
        Self {
            repository,
            providers,
        }
    }

    /// 读取一个 PostgreSQL revision，并为启用的 instance 查询实时模型目录。
    pub async fn compile(&self) -> Result<CompiledRuntimeSnapshot, RuntimeSnapshotCompileError> {
        let data = self
            .repository
            .load_runtime_snapshot()
            .await
            .map_err(|_| RuntimeSnapshotCompileError::StoreUnavailable)?;
        if data.config_revision != data.observed_current_revision {
            return Err(RuntimeSnapshotCompileError::RevisionChanged);
        }
        compile_runtime_snapshot(data, &self.providers).await
    }
}

/// 快照未发布时可安全记录的稳定错误。
#[derive(Debug, thiserror::Error)]
pub enum RuntimeSnapshotCompileError {
    #[error("runtime snapshot store is unavailable")]
    StoreUnavailable,
    #[error("runtime configuration changed while the snapshot was loading")]
    RevisionChanged,
    #[error("runtime snapshot contains invalid frozen data")]
    InvalidData,
}

async fn compile_runtime_snapshot(
    data: RuntimeSnapshotData,
    providers: &ProviderRegistry,
) -> Result<CompiledRuntimeSnapshot, RuntimeSnapshotCompileError> {
    let mut instances_by_id = BTreeMap::new();
    for record in &data.provider_instances {
        let instance = ProviderInstance::new(
            ProviderInstanceId::new(record.id.clone())
                .map_err(|_| RuntimeSnapshotCompileError::InvalidData)?,
            gateway_core::routing::ProviderKind::new(record.provider_kind.clone())
                .map_err(|_| RuntimeSnapshotCompileError::InvalidData)?,
            record.base_url.clone(),
            record.enabled,
            InstanceHealth::Healthy,
        );
        instances_by_id.insert(record.id.clone(), instance.clone());
    }

    // Provider catalog 只用于公开模型列表和能力提示；未知模型仍允许透传，
    // 最终是否支持由对应上游返回。
    let mut provider_models = Vec::new();
    for record in &data.provider_instances {
        let instance = instances_by_id
            .get(record.id.as_str())
            .ok_or(RuntimeSnapshotCompileError::InvalidData)?;
        let models = match providers.query_model_capabilities(instance).await {
            Ok(models) => models,
            Err(error) => {
                tracing::warn!(
                    provider_instance_id = %record.id,
                    provider_kind = %record.provider_kind,
                    error = ?error,
                    "Provider model catalog unavailable; requests remain pass-through"
                );
                continue;
            }
        };
        for model in models {
            provider_models.push(ProviderModel::new(
                instance.id().clone(),
                model.upstream_model().clone(),
                model.capabilities().clone(),
            ));
        }
    }

    let provider_model_mappings = data
        .settings
        .provider_model_mappings
        .into_iter()
        .map(|(provider, mapping)| {
            Ok((
                gateway_core::routing::ProviderKind::new(provider)
                    .map_err(|_| RuntimeSnapshotCompileError::InvalidData)?,
                mapping,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, RuntimeSnapshotCompileError>>()?;

    let rotation_strategy = match data.settings.rotation_strategy.as_str() {
        "smart" => RotationStrategy::Smart,
        "quota_reset_priority" => RotationStrategy::QuotaResetPriority,
        "round_robin" => RotationStrategy::RoundRobin,
        "sticky" => RotationStrategy::Sticky,
        _ => return Err(RuntimeSnapshotCompileError::InvalidData),
    };
    let selection_policy = AccountSelectionPolicy::new(
        rotation_strategy,
        NonZeroU32::new(data.settings.max_concurrent_per_account)
            .ok_or(RuntimeSnapshotCompileError::InvalidData)?,
        Duration::from_millis(data.settings.request_interval_ms),
    );
    let client_policies = data
        .client_api_keys
        .into_iter()
        .map(|key| {
            let provider_kind = gateway_core::routing::ProviderKind::new(key.provider_kind)
                .map_err(|_| RuntimeSnapshotCompileError::InvalidData)?;
            Ok(ClientPolicy::new(
                key.id,
                key.plaintext_key,
                provider_kind,
                true,
                key.limits,
            ))
        })
        .collect::<Result<Vec<_>, RuntimeSnapshotCompileError>>()?;
    let instances = instances_by_id.into_values().collect();
    let routing = RuntimeSnapshot::new(
        ConfigRevision::new(data.config_revision.get())
            .map_err(|_| RuntimeSnapshotCompileError::InvalidData)?,
        selection_policy,
        instances,
        provider_models,
        client_policies,
    )
    .map_err(|_| RuntimeSnapshotCompileError::InvalidData)?
    .with_provider_model_mappings(provider_model_mappings);
    Ok(CompiledRuntimeSnapshot::new(routing))
}

/// RuntimeSnapshot 发布和请求级冻结句柄。
#[derive(Clone, Default)]
pub struct RuntimeSnapshotHandle {
    current: Arc<RwLock<Option<Arc<CompiledRuntimeSnapshot>>>>,
}

impl RuntimeSnapshotHandle {
    #[must_use]
    pub fn new(initial: CompiledRuntimeSnapshot) -> Self {
        Self {
            current: Arc::new(RwLock::new(Some(Arc::new(initial)))),
        }
    }

    pub fn publish(&self, snapshot: CompiledRuntimeSnapshot) {
        *write_unpoisoned(&self.current) = Some(Arc::new(snapshot));
    }

    pub fn suspend(&self) {
        *write_unpoisoned(&self.current) = None;
    }

    #[must_use]
    pub fn revision(&self) -> Option<u64> {
        read_unpoisoned(&self.current)
            .as_ref()
            .map(|snapshot| snapshot.routing().revision().get())
    }

    fn acquire(&self) -> Result<Arc<CompiledRuntimeSnapshot>, ClientApiKeyAuthError> {
        read_unpoisoned(&self.current)
            .clone()
            .ok_or(ClientApiKeyAuthError::RuntimeUnavailable)
    }
}

/// 配置提交后的本进程快照发布与跨进程失效通知。
#[derive(Clone)]
pub struct RuntimeSnapshotPublisher {
    compiler: Arc<RuntimeSnapshotCompiler>,
    snapshots: RuntimeSnapshotHandle,
    runtime_changes: Arc<dyn gateway_store::redis::RuntimeChangeRepository>,
}

impl RuntimeSnapshotPublisher {
    #[must_use]
    pub const fn new(
        compiler: Arc<RuntimeSnapshotCompiler>,
        snapshots: RuntimeSnapshotHandle,
        runtime_changes: Arc<dyn gateway_store::redis::RuntimeChangeRepository>,
    ) -> Self {
        Self {
            compiler,
            snapshots,
            runtime_changes,
        }
    }

    /// 重新编译并原子替换本进程快照。
    pub async fn refresh(&self) -> Result<Revision, RuntimeSnapshotCompileError> {
        let snapshot = self.compiler.compile().await?;
        let revision = Revision::new(snapshot.routing().revision().get())
            .map_err(|_| RuntimeSnapshotCompileError::InvalidData)?;
        self.snapshots.publish(snapshot);
        Ok(revision)
    }

    fn suspend(&self) {
        self.snapshots.suspend();
    }

    fn published_revision(&self) -> Option<u64> {
        self.snapshots.revision()
    }

    /// 数据库提交不能被外部目录或 Redis 的暂时故障伪装成回滚。
    pub async fn publish_committed(&self, committed_revision: Revision) {
        match self.refresh().await {
            Ok(published_revision) if published_revision == committed_revision => {}
            Ok(published_revision) => {
                tracing::warn!(
                    committed_revision = committed_revision.get(),
                    published_revision = published_revision.get(),
                    "本进程跳过了已经被更新版本取代的配置快照"
                );
            }
            Err(error) => {
                self.suspend();
                tracing::error!(
                    committed_revision = committed_revision.get(),
                    error = %error,
                    "配置已经提交，但本进程快照暂未刷新"
                );
            }
        }
        let change = gateway_store::redis::RuntimeChange::SnapshotPublished {
            config_revision: committed_revision,
        };
        if self
            .runtime_changes
            .publish_runtime_change(&change)
            .await
            .is_err()
        {
            tracing::warn!(
                config_revision = committed_revision.get(),
                "配置已经提交，但 Redis 失效通知发布失败"
            );
        }
    }
}

/// 已认证 Client 与认证时冻结的完整配置 revision。
#[derive(Clone)]
pub struct AuthenticatedClient {
    snapshot: Arc<CompiledRuntimeSnapshot>,
    policy: ClientPolicy,
}

impl AuthenticatedClient {
    #[must_use]
    pub const fn snapshot(&self) -> &Arc<CompiledRuntimeSnapshot> {
        &self.snapshot
    }

    #[must_use]
    pub const fn policy(&self) -> &ClientPolicy {
        &self.policy
    }
}

impl fmt::Debug for AuthenticatedClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedClient")
            .field("key_id", &self.policy.key_id())
            .field("revision", &self.snapshot.routing.revision())
            .finish_non_exhaustive()
    }
}

fn authenticate_client_key(
    snapshots: &RuntimeSnapshotHandle,
    plaintext: &str,
) -> Result<AuthenticatedClient, ClientApiKeyAuthError> {
    if !valid_client_api_key_shape(plaintext) {
        return Err(ClientApiKeyAuthError::InvalidKey);
    }
    let snapshot = snapshots.acquire()?;
    let mut matched = None;
    for policy in snapshot.routing.client_policies() {
        let expected = policy.plaintext_key().expose_for_auth();
        let equal = plaintext.len() == expected.len()
            && bool::from(plaintext.as_bytes().ct_eq(expected.as_bytes()));
        if equal && policy.authorize().is_ok() {
            matched = Some(policy.clone());
        }
    }
    matched
        .map(|policy| AuthenticatedClient { snapshot, policy })
        .ok_or(ClientApiKeyAuthError::InvalidKey)
}

fn valid_client_api_key_shape(value: &str) -> bool {
    let Some(encoded) = value.strip_prefix("sk_") else {
        return false;
    };
    URL_SAFE_NO_PAD
        .decode(encoded)
        .is_ok_and(|decoded| decoded.len() == 32)
}

/// HTTP/WS 长连接的统一接入、编号和 drain 生命周期。
#[derive(Clone)]
pub struct ConnectionLifecycle {
    shutting_down: Arc<AtomicBool>,
    sequence: Arc<AtomicU64>,
    tasks: TaskTracker,
}

impl Default for ConnectionLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionLifecycle {
    #[must_use]
    pub fn new() -> Self {
        Self {
            shutting_down: Arc::new(AtomicBool::new(false)),
            sequence: Arc::new(AtomicU64::new(0)),
            tasks: TaskTracker::new(),
        }
    }

    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }

    pub fn spawn(&self, task: ConnectionTask) {
        if self.is_shutting_down() {
            return;
        }
        self.tasks.spawn(task);
    }

    #[must_use]
    pub fn next_id(&self, prefix: &str) -> String {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}_{}_{sequence}", Uuid::now_v7().simple())
    }

    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.tasks.close();
        self.tasks.wait().await;
    }
}

fn read_unpoisoned<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_unpoisoned<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

use axum::{Router, extract::State, http::StatusCode, routing::get};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

use crate::workers::WorkerHealthRegistry;

/// `/healthz` 同时验证快照、worker 新鲜度和 PostgreSQL/Redis 真实连通性。
pub struct HealthStatus {
    snapshots: RuntimeSnapshotHandle,
    workers: WorkerHealthRegistry,
    postgres: sqlx::PgPool,
    redis: redis::aio::ConnectionManager,
}

impl HealthStatus {
    #[must_use]
    pub fn new(
        snapshots: RuntimeSnapshotHandle,
        workers: WorkerHealthRegistry,
        postgres: sqlx::PgPool,
        redis: redis::aio::ConnectionManager,
    ) -> Self {
        Self {
            snapshots,
            workers,
            postgres,
            redis,
        }
    }

    #[must_use]
    pub async fn healthy(&self) -> bool {
        if self.snapshots.acquire().is_err() || !self.workers.all_healthy() {
            return false;
        }
        let postgres = async {
            let mut connection = self.postgres.acquire().await?;
            sqlx::Connection::ping(&mut *connection).await
        };
        let mut redis = self.redis.clone();
        let infrastructure = tokio::time::timeout(Duration::from_secs(2), async move {
            let ping = redis::cmd("PING");
            let redis_ping = ping.query_async::<String>(&mut redis);
            tokio::join!(postgres, redis_ping)
        })
        .await;
        matches!(
            infrastructure,
            Ok((Ok(()), Ok(response))) if response == "PONG"
        )
    }
}

/// 组合固定 OpenAI/Admin 路由、健康检查与旧前端静态资源。
pub fn application_router<O>(
    state: ApplicationState<O>,
    asset_directory: impl Into<PathBuf>,
) -> Router
where
    O: OpenAiClientService,
{
    let asset_directory = asset_directory.into();
    let index = asset_directory.join("index.html");
    let request_id_header = axum::http::HeaderName::from_static("x-request-id");
    Router::new()
        .route("/healthz", get(healthz::<O>))
        .merge(gateway_api::openai::router::<ApplicationState<O>>())
        .merge(gateway_api::admin::router::<ApplicationState<O>>())
        .fallback_service(ServeDir::new(asset_directory).fallback(ServeFile::new(index)))
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn healthz<O>(State(state): State<ApplicationState<O>>) -> StatusCode {
    if state.health.healthy().await {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gateway_api::openai::{
    DeliveryEvent, ResponseExecutionSession as ApiResponseExecutionSession, ResponsesTransport,
    StartedResponse,
    error::gateway_error_from_engine,
    responses::{ContinuationIntent, DecodedResponsesRequest},
};
use gateway_core::{
    engine::{
        AttemptCoordinator, CancellationToken as CoreCancellationToken, EngineError, GatewayEngine,
        ModelRequestId, NewModelRequest, ProviderAttemptOutcome,
        continuation::NativeContinuationPin,
        coordinator::ResponseExecutionSession as CoreResponseExecutionSession,
    },
    error::{GatewayError, GatewayErrorKind, ProviderErrorKind},
    policy::ClientApiKeyId,
    routing::RoutingContext,
};
use gateway_store::{
    postgres::PgExecutionStore,
    redis::{
        ClientAdmissionDecision, ClientAdmissionLimits, ClientAdmissionRejection,
        ClientAdmissionRepository, ClientAdmissionRequest, ProviderCircuitDecision,
        ProviderCircuitRepository,
    },
};

const MODEL_REQUEST_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// 已认证 previous-response 到 Core pin 的唯一应用端口。
#[async_trait]
pub trait NativeContinuationResolver: Send + Sync {
    async fn resolve_native_continuation(
        &self,
        client_api_key_id: &ClientApiKeyId,
        client_response_id: &str,
    ) -> Result<NativeContinuationPin, GatewayError>;
}

/// PostgreSQL 历史事实到 Codex native previous-response pin 的组合 adapter。
pub struct PgNativeContinuationResolver {
    history: Arc<dyn gateway_store::postgres::ModelRequestHistoryRepository>,
}

impl PgNativeContinuationResolver {
    #[must_use]
    pub const fn new(
        history: Arc<dyn gateway_store::postgres::ModelRequestHistoryRepository>,
    ) -> Self {
        Self { history }
    }
}

#[async_trait]
impl NativeContinuationResolver for PgNativeContinuationResolver {
    async fn resolve_native_continuation(
        &self,
        client_api_key_id: &ClientApiKeyId,
        client_response_id: &str,
    ) -> Result<NativeContinuationPin, GatewayError> {
        let history = self
            .history
            .find_model_request_by_client_response_id(
                client_response_id,
                client_api_key_id.as_str(),
            )
            .await
            .map_err(|_| {
                GatewayError::new(
                    GatewayErrorKind::Internal,
                    "previous response history is temporarily unavailable",
                )
            })?
            .ok_or_else(|| {
                GatewayError::new(
                    GatewayErrorKind::InvalidRequest,
                    "previous response was not found",
                )
            })?;
        if history.provider_kind.as_deref() != Some("openai") {
            return Err(GatewayError::new(
                GatewayErrorKind::Unsupported,
                "the previous response does not support native continuation",
            ));
        }
        self.history
            .resolve_native_continuation_pin(
                client_response_id,
                client_api_key_id.as_str(),
                gateway_core::engine::continuation::NativeContinuationReuse::Reusable,
            )
            .await
            .map_err(|_| {
                GatewayError::new(
                    GatewayErrorKind::Internal,
                    "previous response history is temporarily unavailable",
                )
            })?
            .ok_or_else(|| {
                GatewayError::new(
                    GatewayErrorKind::InvalidRequest,
                    "previous response is not eligible for native continuation",
                )
            })
    }
}

/// OpenAI API 与 Core streaming session 之间的薄组合 adapter。
#[derive(Clone)]
pub struct GatewayOpenAiService {
    snapshots: RuntimeSnapshotHandle,
    coordinator: Arc<AttemptCoordinator<PgExecutionStore>>,
    admissions: Arc<dyn ClientAdmissionRepository>,
    circuits: Arc<dyn ProviderCircuitRepository>,
    history: Arc<dyn NativeContinuationResolver>,
    connections: ConnectionLifecycle,
}

impl GatewayOpenAiService {
    #[must_use]
    pub fn new(
        snapshots: RuntimeSnapshotHandle,
        execution_store: Arc<PgExecutionStore>,
        providers: ProviderRegistry,
        admissions: Arc<dyn ClientAdmissionRepository>,
        circuits: Arc<dyn ProviderCircuitRepository>,
        history: Arc<dyn NativeContinuationResolver>,
        connections: ConnectionLifecycle,
    ) -> Self {
        let engine = GatewayEngine::new(execution_store, providers);
        Self {
            snapshots,
            coordinator: Arc::new(AttemptCoordinator::new(engine)),
            admissions,
            circuits,
            history,
            connections,
        }
    }

    /// 管理端账号探测仍走唯一 Router -> Engine -> Provider -> persistence 链，
    /// 仅增加 required-account fence，并禁用账号重试与 Provider instance fallback。
    async fn test_account(
        &self,
        account_id: gateway_core::engine::credential::ProviderAccountId,
        provider_instance_id: ProviderInstanceId,
        upstream_model: UpstreamModelId,
    ) -> Result<Vec<String>, GatewayError> {
        let snapshot = self.snapshots.acquire().map_err(|_| {
            GatewayError::new(
                GatewayErrorKind::Internal,
                "runtime snapshot is unavailable",
            )
        })?;
        let message = gateway_core::operation::Message::new(
            gateway_core::operation::MessageRole::User,
            vec![gateway_core::operation::ContentPart::Text(
                "Reply with exactly OK.".to_owned(),
            )],
        )
        .map_err(|_| {
            GatewayError::new(
                GatewayErrorKind::Internal,
                "connection test request is invalid",
            )
        })?;
        let operation = gateway_core::operation::Operation::Generate(
            gateway_core::operation::GenerateRequest::new(vec![message]).map_err(|_| {
                GatewayError::new(
                    GatewayErrorKind::Internal,
                    "connection test request is invalid",
                )
            })?,
        );
        let model_request_id = ModelRequestId::new(self.connections.next_id("req"))
            .map_err(|_| GatewayError::new(GatewayErrorKind::Internal, "invalid request ID"))?;
        let started_at = SystemTime::now();
        let deadline_at = started_at
            .checked_add(MODEL_REQUEST_DEADLINE)
            .ok_or_else(|| GatewayError::new(GatewayErrorKind::Internal, "invalid system clock"))?;
        let provider_kind = snapshot
            .routing()
            .provider_for_instance(&provider_instance_id)
            .cloned()
            .ok_or_else(|| {
                GatewayError::new(
                    GatewayErrorKind::Unsupported,
                    "Provider instance is unavailable",
                )
            })?;
        let routing_context = RoutingContext {
            provider_kind: Some(provider_kind),
            allowed_instances: Some(BTreeSet::from([provider_instance_id])),
            ..RoutingContext::default()
        };
        let public_model =
            PublicModelId::new(upstream_model.as_str().to_owned()).map_err(|_| {
                GatewayError::new(GatewayErrorKind::Unsupported, "requested model is invalid")
            })?;
        let plan = snapshot
            .routing()
            .plan(&public_model, &operation, &routing_context)
            .map_err(map_routing_error)?;
        let client_ref = ClientApiKeyId::new("admin_connection_test")
            .map_err(|_| GatewayError::new(GatewayErrorKind::Internal, "invalid admin actor"))?;
        let request = NewModelRequest {
            id: model_request_id.clone(),
            client_api_key_id: None,
            client_api_key_ref: client_ref,
            config_revision: plan.config_revision(),
            protocol: "admin_connection_test".to_owned(),
            operation: operation.kind(),
            endpoint: "/api/admin/accounts/test".to_owned(),
            client_transport: "internal".to_owned(),
            requested_model: public_model,
            input_token_estimate: operation.capability_requirements().minimum_context_tokens(),
            client_ip: None,
            user_agent: None,
            reasoning_effort: None,
            reasoning_preset: None,
            request_kind: Some("account_connection_test".to_owned()),
            subagent_kind: None,
            compact: false,
            started_at,
            deadline_at,
        };
        let cancellation = CoreCancellationToken::new();
        let mut session = self
            .coordinator
            .start(
                request,
                operation,
                plan,
                Some(account_id),
                None,
                cancellation,
            )
            .await
            .map_err(|error| gateway_error_from_engine(&error))?;
        let collected = session.collect_uncommitted().await;
        publish_provider_attempt_outcomes(
            self.circuits.as_ref(),
            session.provider_attempt_outcomes(),
        )
        .await;
        let events = collected.map_err(|error| gateway_error_from_engine(&error))?;
        session
            .commit_downstream(Some(200))
            .await
            .map_err(|error| gateway_error_from_engine(&error))?;
        Ok(events
            .into_iter()
            .filter_map(|event| match event {
                gateway_core::event::GatewayEvent::TextDelta(delta) => Some(delta.text),
                _ => None,
            })
            .collect())
    }

    async fn route_context(
        &self,
        snapshot: &RuntimeSnapshot,
        provider_kind: &gateway_core::routing::ProviderKind,
        model: &PublicModelId,
        operation: &gateway_core::operation::Operation,
    ) -> Result<RoutingContext, GatewayError> {
        let preliminary = snapshot
            .plan(
                model,
                operation,
                &RoutingContext {
                    provider_kind: Some(provider_kind.clone()),
                    allowed_instances: Some(snapshot.instance_ids_for_provider(provider_kind)),
                    ..RoutingContext::default()
                },
            )
            .map_err(map_routing_error)?;
        let mut blocked_instances = std::collections::BTreeSet::new();
        let mut checked = std::collections::BTreeSet::<ProviderInstanceId>::new();
        for candidate in preliminary.candidates() {
            let instance = candidate.instance();
            if !checked.insert(instance.clone()) {
                continue;
            }
            match self
                .circuits
                .provider_circuit_decision(instance.as_str())
                .await
                .map_err(|_| {
                    GatewayError::new(
                        GatewayErrorKind::NoAvailableProvider,
                        "provider health state is temporarily unavailable",
                    )
                })? {
                ProviderCircuitDecision::Allow => {}
                ProviderCircuitDecision::BlockedUntil(_) => {
                    blocked_instances.insert(instance.clone());
                }
            }
        }
        Ok(RoutingContext {
            provider_kind: Some(provider_kind.clone()),
            allowed_instances: Some(snapshot.instance_ids_for_provider(provider_kind)),
            blocked_instances,
            ..RoutingContext::default()
        })
    }
}

/// 只有 Provider instance 可归因的瞬态故障才影响 circuit。
#[must_use]
pub const fn provider_failure_affects_circuit(error_kind: ProviderErrorKind) -> bool {
    matches!(
        error_kind,
        ProviderErrorKind::Timeout
            | ProviderErrorKind::Transport
            | ProviderErrorKind::Protocol
            | ProviderErrorKind::Unavailable
    )
}

async fn publish_provider_attempt_outcomes(
    circuits: &dyn ProviderCircuitRepository,
    outcomes: &[ProviderAttemptOutcome],
) {
    for outcome in outcomes {
        let instance_id = outcome.provider_instance_id().as_str();
        let result = match outcome.error_kind() {
            None => circuits.observe_provider_success(instance_id).await,
            Some(error_kind) if provider_failure_affects_circuit(error_kind) => circuits
                .observe_provider_failure(instance_id)
                .await
                .map(|_| ()),
            Some(_) => continue,
        };
        if result.is_err() {
            tracing::warn!(
                provider_instance_id = instance_id,
                "Provider circuit 结果写入失败"
            );
        }
    }
}

struct ClientAdmissionLease {
    repository: Arc<dyn ClientAdmissionRepository>,
    client_api_key_ref: String,
    model_request_id: String,
}

impl ClientAdmissionLease {
    async fn release(self) {
        if self
            .repository
            .release_client_request(&self.client_api_key_ref, &self.model_request_id)
            .await
            .is_err()
        {
            tracing::warn!("客户端准入租约释放失败，将由 Redis TTL 收敛");
        }
    }
}

/// API handler 持有的 Core session 与 Redis admission lease。
pub struct GatewayResponseExecution {
    core: Option<CoreResponseExecutionSession<PgExecutionStore>>,
    admission: Option<ClientAdmissionLease>,
    circuits: Arc<dyn ProviderCircuitRepository>,
    observed_provider_outcomes: usize,
}

impl GatewayResponseExecution {
    #[must_use]
    fn new(
        core: CoreResponseExecutionSession<PgExecutionStore>,
        admission: ClientAdmissionLease,
        circuits: Arc<dyn ProviderCircuitRepository>,
    ) -> Self {
        Self {
            core: Some(core),
            admission: Some(admission),
            circuits,
            observed_provider_outcomes: 0,
        }
    }

    fn core_mut(
        &mut self,
    ) -> Result<&mut CoreResponseExecutionSession<PgExecutionStore>, EngineError> {
        self.core.as_mut().ok_or(EngineError::InvalidDeliveryState)
    }

    async fn settle_if_finalized(&mut self) {
        if !self
            .core
            .as_ref()
            .is_some_and(CoreResponseExecutionSession::is_finalized)
        {
            return;
        }
        if let Some(admission) = self.admission.take() {
            admission.release().await;
        }
    }

    async fn observe_provider_outcomes(&mut self) {
        let Some(core) = self.core.as_ref() else {
            return;
        };
        let outcomes = core.provider_attempt_outcomes();
        let new_outcomes = outcomes
            .get(self.observed_provider_outcomes..)
            .unwrap_or_default()
            .to_vec();
        self.observed_provider_outcomes = outcomes.len();
        publish_provider_attempt_outcomes(self.circuits.as_ref(), &new_outcomes).await;
    }

    fn detach_inner(&mut self) {
        let Some(mut core) = self.core.take() else {
            return;
        };
        core.cancel();
        let admission = self.admission.take();
        let circuits = Arc::clone(&self.circuits);
        let observed_provider_outcomes = self.observed_provider_outcomes;
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        std::mem::drop(runtime.spawn(async move {
            if !core.is_finalized() {
                let _ = core.cancel_and_finalize().await;
            }
            let pending_outcomes = core
                .provider_attempt_outcomes()
                .get(observed_provider_outcomes..)
                .unwrap_or_default();
            publish_provider_attempt_outcomes(circuits.as_ref(), pending_outcomes).await;
            if let Some(admission) = admission {
                admission.release().await;
            }
        }));
    }
}

impl Drop for GatewayResponseExecution {
    fn drop(&mut self) {
        self.detach_inner();
    }
}

#[async_trait]
impl ApiResponseExecutionSession for GatewayResponseExecution {
    async fn next_delivery_event(&mut self) -> Result<Option<DeliveryEvent>, EngineError> {
        let event = self.core_mut()?.next_event().await.map(|event| {
            event.map(|event| {
                let requirement = event.commit_requirement();
                DeliveryEvent::new(event.into_event(), requirement)
            })
        });
        self.observe_provider_outcomes().await;
        self.settle_if_finalized().await;
        event
    }

    async fn collect_uncommitted(
        &mut self,
    ) -> Result<Vec<gateway_core::event::GatewayEvent>, EngineError> {
        let events = self.core_mut()?.collect_uncommitted().await;
        self.observe_provider_outcomes().await;
        self.settle_if_finalized().await;
        events
    }

    async fn commit_downstream(
        &mut self,
        client_status_code: Option<u16>,
    ) -> Result<(), EngineError> {
        let result = self.core_mut()?.commit_downstream(client_status_code).await;
        self.observe_provider_outcomes().await;
        self.settle_if_finalized().await;
        result
    }

    async fn record_client_status(&mut self, client_status_code: u16) -> Result<(), EngineError> {
        let result = self
            .core_mut()?
            .record_client_status(client_status_code)
            .await;
        self.observe_provider_outcomes().await;
        self.settle_if_finalized().await;
        result
    }

    fn is_finalized(&self) -> bool {
        self.core
            .as_ref()
            .is_none_or(CoreResponseExecutionSession::is_finalized)
    }

    fn cancel(&self) {
        if let Some(core) = &self.core {
            core.cancel();
        }
    }

    fn detach_finalize(mut self) {
        self.detach_inner();
    }
}

#[async_trait]
impl OpenAiClientService for GatewayOpenAiService {
    type Client = AuthenticatedClient;
    type Session = GatewayResponseExecution;

    fn authenticate(&self, plaintext: &str) -> Result<Self::Client, ClientApiKeyAuthError> {
        authenticate_client_key(&self.snapshots, plaintext)
    }

    fn public_models(&self, client: &Self::Client) -> Vec<String> {
        client
            .snapshot()
            .routing()
            .public_models_for_provider(client.policy().provider_kind())
            .into_iter()
            .map(|model| model.as_str().to_owned())
            .collect()
    }

    fn contains_public_model(&self, client: &Self::Client, model: &PublicModelId) -> bool {
        client
            .snapshot()
            .routing()
            .contains_public_model_for_provider(model, client.policy().provider_kind())
    }

    async fn start_response(
        &self,
        client: Self::Client,
        request: DecodedResponsesRequest,
        transport: ResponsesTransport,
    ) -> Result<StartedResponse<Self::Session>, GatewayError> {
        client.policy().authorize().map_err(|_| {
            GatewayError::new(GatewayErrorKind::PolicyDenied, "client API key is disabled")
        })?;
        let (operation, metadata) = request.into_parts();
        let (reasoning_effort, semantics) = match &operation {
            gateway_core::operation::Operation::Generate(request) => (
                request
                    .reasoning()
                    .and_then(|reasoning| reasoning.effort)
                    .map(reasoning_effort_name)
                    .map(str::to_owned),
                provider_openai::codex_request_semantics(request),
            ),
            _ => (
                None,
                provider_openai::transport::protocol::responses::CodexRequestSemantics::default(),
            ),
        };
        let public_model =
            PublicModelId::new(metadata.public_model().to_owned()).map_err(|_| {
                GatewayError::new(
                    GatewayErrorKind::ModelNotFound,
                    "requested model was not found",
                )
            })?;
        let model_request_id =
            ModelRequestId::new(self.connections.next_id("req")).map_err(|_| {
                GatewayError::new(GatewayErrorKind::Internal, "failed to allocate request ID")
            })?;
        let started_at = SystemTime::now();
        let deadline_at = started_at
            .checked_add(MODEL_REQUEST_DEADLINE)
            .ok_or_else(|| {
                GatewayError::new(GatewayErrorKind::Internal, "system clock is invalid")
            })?;
        let routing_context = self
            .route_context(
                client.snapshot().routing(),
                client.policy().provider_kind(),
                &public_model,
                &operation,
            )
            .await?;
        let plan = client
            .snapshot()
            .routing()
            .plan(&public_model, &operation, &routing_context)
            .map_err(map_routing_error)?;
        let continuation = match metadata.continuation() {
            ContinuationIntent::None => None,
            ContinuationIntent::PreviousResponseId(response_id) => Some(
                self.history
                    .resolve_native_continuation(client.policy().key_id(), response_id)
                    .await?,
            ),
        };
        let limits = client.policy().limits();
        let admission_request = ClientAdmissionRequest {
            model_request_id: model_request_id.as_str().to_owned(),
            client_api_key_ref: client.policy().key_id().as_str().to_owned(),
            input_token_estimate: operation.capability_requirements().minimum_context_tokens(),
            lease_ttl: MODEL_REQUEST_DEADLINE,
            limits: ClientAdmissionLimits {
                max_concurrency: limits.max_concurrency,
                requests_per_minute: limits.requests_per_minute,
                tokens_per_minute: limits.tokens_per_minute,
            },
        };
        match self
            .admissions
            .admit_client_request(&admission_request)
            .await
            .map_err(|_| {
                GatewayError::new(
                    GatewayErrorKind::NoAvailableProvider,
                    "request admission is temporarily unavailable",
                )
            })? {
            ClientAdmissionDecision::Granted => {}
            ClientAdmissionDecision::Rejected(
                ClientAdmissionRejection::RateLimited
                | ClientAdmissionRejection::ConcurrencyLimited,
            ) => {
                return Err(GatewayError::new(
                    GatewayErrorKind::RateLimited,
                    "request exceeds client API key limits",
                ));
            }
        }
        let admission = ClientAdmissionLease {
            repository: Arc::clone(&self.admissions),
            client_api_key_ref: client.policy().key_id().as_str().to_owned(),
            model_request_id: model_request_id.as_str().to_owned(),
        };
        let client_transport = match (transport, metadata.stream()) {
            (ResponsesTransport::WebSocket, _) => "websocket",
            (ResponsesTransport::Http, true) => "http_sse",
            (ResponsesTransport::Http, false) => "http_json",
        };
        let new_request = NewModelRequest {
            id: model_request_id.clone(),
            client_api_key_id: Some(client.policy().key_id().clone()),
            client_api_key_ref: client.policy().key_id().clone(),
            config_revision: plan.config_revision(),
            protocol: "openai_responses".to_owned(),
            operation: operation.kind(),
            endpoint: "/v1/responses".to_owned(),
            client_transport: client_transport.to_owned(),
            requested_model: public_model,
            input_token_estimate: admission_request.input_token_estimate,
            client_ip: metadata.client_ip(),
            user_agent: metadata.user_agent().map(str::to_owned),
            reasoning_effort,
            reasoning_preset: semantics.reasoning_preset.map(str::to_owned),
            request_kind: semantics.request_kind,
            subagent_kind: semantics.subagent_kind,
            compact: semantics.compact,
            started_at,
            deadline_at,
        };
        let cancellation = CoreCancellationToken::new();
        let core = match self
            .coordinator
            .start(
                new_request,
                operation,
                plan,
                None,
                continuation,
                cancellation,
            )
            .await
        {
            Ok(core) => core,
            Err(error) => {
                admission.release().await;
                return Err(gateway_error_from_engine(&error));
            }
        };
        let created_at = started_at
            .duration_since(UNIX_EPOCH)
            .map_err(|_| GatewayError::new(GatewayErrorKind::Internal, "system clock is invalid"))?
            .as_secs();
        Ok(StartedResponse::new(
            model_request_id.as_str().to_owned(),
            GatewayResponseExecution::new(core, admission, Arc::clone(&self.circuits)),
            created_at,
            metadata.stream(),
        ))
    }

    fn is_shutting_down(&self) -> bool {
        self.connections.is_shutting_down()
    }

    fn spawn_connection(&self, task: ConnectionTask) {
        self.connections.spawn(task);
    }

    fn next_connection_id(&self) -> String {
        self.connections.next_id("ws")
    }

    fn next_request_id(&self) -> String {
        self.connections.next_id("req")
    }
}

const fn reasoning_effort_name(effort: gateway_core::operation::ReasoningEffort) -> &'static str {
    match effort {
        gateway_core::operation::ReasoningEffort::Minimal => "minimal",
        gateway_core::operation::ReasoningEffort::Low => "low",
        gateway_core::operation::ReasoningEffort::Medium => "medium",
        gateway_core::operation::ReasoningEffort::High => "high",
        gateway_core::operation::ReasoningEffort::ExtraHigh => "xhigh",
    }
}

fn map_routing_error(error: gateway_core::error::RoutingError) -> GatewayError {
    match error {
        gateway_core::error::RoutingError::NoCapableProvider { .. } => GatewayError::new(
            GatewayErrorKind::NoAvailableProvider,
            "no Provider instance can execute this request",
        ),
        _ => GatewayError::new(
            GatewayErrorKind::Internal,
            "runtime routing configuration is invalid",
        ),
    }
}

/// 监听端口前完成的客户端准入恢复结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientAdmissionStartupRecoveryReport {
    pub expired_model_requests: u64,
    pub restored_clients: u64,
    pub restored_recent_requests: u64,
    pub restored_running_requests: u64,
}

/// 先收敛 PostgreSQL 过期请求，再把一分钟窗口和仍在运行的租约恢复到 Redis。
///
/// 任一步失败都直接返回错误，调用方不得启动监听端口。
pub async fn restore_client_admission_startup(
    model_requests: &dyn gateway_store::postgres::ModelRequestRepository,
    recovery_repository: &dyn gateway_store::postgres::ClientAdmissionRecoveryRepository,
    admissions: &dyn gateway_store::redis::ClientAdmissionRepository,
    now: DateTime<Utc>,
) -> gateway_store::StoreResult<ClientAdmissionStartupRecoveryReport> {
    let expired = model_requests.recover_expired_model_requests(now).await?;
    let recoveries = recovery_repository
        .load_client_admission_recovery(now - ChronoDuration::seconds(61))
        .await?;
    let restored_clients = u64::try_from(recoveries.len()).unwrap_or(u64::MAX);
    let mut restored_recent_requests = 0_u64;
    let mut restored_running_requests = 0_u64;
    for recovery in recoveries {
        let restore = gateway_store::redis::ClientAdmissionRestore {
            client_api_key_ref: recovery.client_api_key_ref,
            recent_requests: recovery
                .recent_requests
                .into_iter()
                .map(
                    |request| gateway_store::redis::ClientAdmissionRecentRequest {
                        model_request_id: request.model_request_id,
                        started_at: request.started_at,
                        input_token_estimate: request.input_token_estimate,
                    },
                )
                .collect(),
            running_requests: recovery
                .running_requests
                .into_iter()
                .map(
                    |request| gateway_store::redis::ClientAdmissionRunningRequest {
                        model_request_id: request.model_request_id,
                        expires_at: request.deadline_at,
                    },
                )
                .collect(),
        };
        let result = admissions.restore_client_admission(&restore).await?;
        restored_recent_requests =
            restored_recent_requests.saturating_add(result.restored_recent_requests);
        restored_running_requests =
            restored_running_requests.saturating_add(result.restored_running_requests);
    }
    Ok(ClientAdmissionStartupRecoveryReport {
        expired_model_requests: expired.requests,
        restored_clients,
        restored_recent_requests,
        restored_running_requests,
    })
}

/// 装配并运行唯一生产网关进程。
pub async fn run(config: BootstrapConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    const REDIS_NAMESPACE: &str = "codex-proxy-rs";

    let BootstrapConfigParts {
        app,
        database_url,
        redis_url,
        admin_default_password,
    } = config.into_parts();
    let _log_guard = initialize_logging(&app.logging)?;
    let pool = gateway_store::postgres::connect_and_migrate(database_url.expose_secret()).await?;
    let redis_client = redis::Client::open(redis_url.expose_secret().clone())?;
    let redis_connection = redis_client.get_connection_manager().await?;

    let control_plane = Arc::new(gateway_store::postgres::PgControlPlaneRepository::new(
        pool.clone(),
    ));
    let instances = Arc::new(gateway_store::postgres::PgConfigCatalogRepository::new(
        pool.clone(),
    ));
    let runtime_settings = Arc::new(gateway_store::postgres::PgRuntimeSettingsRepository::new(
        pool.clone(),
    ));
    let client_keys = Arc::new(gateway_store::postgres::PgClientApiKeyRepository::new(
        pool.clone(),
    ));
    let security =
        Arc::new(gateway_store::postgres::PgAdminSecurityAuditRepository::new(pool.clone()));
    let provider_accounts = Arc::new(gateway_store::postgres::PgProviderAccountRepository::new(
        pool.clone(),
    ));
    let snapshot_repository = Arc::new(gateway_store::postgres::PgRuntimeSnapshotRepository::new(
        pool.clone(),
    ));
    let execution_store = Arc::new(gateway_store::postgres::PgExecutionStore::new(pool.clone()));
    let history = Arc::new(gateway_store::postgres::PgHistoryRepository::new(
        pool.clone(),
    ));
    let observability = Arc::new(gateway_store::postgres::PgObservabilityRepository::new(
        pool.clone(),
    ));
    let retention = Arc::new(gateway_store::postgres::PgRetentionRepository::new(
        pool.clone(),
    ));

    let admin_auth_state = Arc::new(gateway_store::redis::RedisAdminAuthStateRepository::new(
        redis_connection.clone(),
        REDIS_NAMESPACE,
    )?);
    let credential_leases = gateway_store::redis::RedisCredentialLeaseRepository::new(
        redis_connection.clone(),
        REDIS_NAMESPACE,
    )?;
    let credential_cooldowns = Arc::new(
        gateway_store::redis::RedisCredentialCooldownRepository::new(
            redis_connection.clone(),
            REDIS_NAMESPACE,
        )?,
    );
    let core_account_store = Arc::new(
        gateway_store::redis::CooldownCachingProviderAccountStore::new(
            provider_accounts.clone(),
            credential_cooldowns,
        ),
    );
    let hydrated_cooldowns = core_account_store.hydrate(SystemTime::now()).await;
    tracing::info!(hydrated_cooldowns, "账号 cooldown Redis 热缓存重建完成");
    let provider_leases = Arc::new(ProviderLeaseAdapter::new(credential_leases.clone()));
    let credential_state = Arc::new(gateway_store::redis::RedisCredentialStateRepository::new(
        redis_connection.clone(),
        REDIS_NAMESPACE,
    )?);
    let xai_cache = Arc::new(XaiCatalogCacheAdapter::new(credential_state));
    let admissions = Arc::new(gateway_store::redis::RedisClientAdmissionRepository::new(
        redis_connection.clone(),
        REDIS_NAMESPACE,
    )?);
    let admission_recovery =
        gateway_store::postgres::PgClientAdmissionRecoveryRepository::new(pool.clone());
    let admission_recovery_report = restore_client_admission_startup(
        execution_store.as_ref(),
        &admission_recovery,
        admissions.as_ref(),
        Utc::now(),
    )
    .await?;
    tracing::info!(
        expired_model_requests = admission_recovery_report.expired_model_requests,
        restored_clients = admission_recovery_report.restored_clients,
        restored_recent_requests = admission_recovery_report.restored_recent_requests,
        restored_running_requests = admission_recovery_report.restored_running_requests,
        "客户端准入热状态恢复完成"
    );
    let circuits = Arc::new(gateway_store::redis::RedisProviderCircuitRepository::new(
        redis_connection.clone(),
        REDIS_NAMESPACE,
        gateway_store::redis::ProviderCircuitPolicy::default(),
    )?);
    let runtime_changes = Arc::new(gateway_store::redis::RedisRuntimeChangeRepository::new(
        redis_client,
        REDIS_NAMESPACE,
    )?);

    let settings = runtime_settings.load_runtime_settings().await?;
    let refresh_margin = Duration::from_secs(settings.refresh_margin_seconds);
    let refresh_concurrency = settings.refresh_concurrency;
    let wire_profile = provider_openai::transport::profile::CodexWireProfileState::new(
        provider_openai::transport::profile::CodexWireProfile {
            originator: app.wire_profile.originator.clone(),
            codex_version: app.wire_profile.codex_version.clone(),
            desktop_version: app.wire_profile.desktop_version.clone(),
            desktop_build: app.wire_profile.desktop_build.clone(),
            os_type: app.wire_profile.os_type.clone(),
            os_version: app.wire_profile.os_version.clone(),
            arch: app.wire_profile.arch.clone(),
            terminal: app.wire_profile.terminal.clone(),
            verified_at: app.wire_profile.verified_at,
        },
    );
    let core_account_store: Arc<dyn ProviderAccountStore> = core_account_store;
    let codex_repository = provider_openai::credential::CodexCredentialRepository::new(Arc::clone(
        &core_account_store,
    ));
    let codex_catalog = Arc::new(
        provider_openai::credential::CodexCredentialCatalogService::new(
            codex_repository.clone(),
            wire_profile.clone(),
        )?,
    );
    let codex_selector = Arc::new(provider_openai::credential::CodexCredentialSelector::new(
        codex_repository.clone(),
        provider_leases.clone(),
        provider_openai::credential::CodexCookiePolicy::official()?,
    ));
    let codex_provider = Arc::new(provider_openai::CodexProvider::new(
        codex_selector,
        Arc::clone(&codex_catalog),
        wire_profile.clone(),
    )?);
    let codex_token_client =
        Arc::new(provider_openai::credential::token_client::official_openai_token_client()?);
    let codex_verifier = Arc::new(provider_openai::credential::CodexJwtIdentityVerifier::new(
        Box::new(provider_openai::credential::ReqwestOpenAiJwksSource::new()?),
    ));
    let codex_owner = Arc::new(
        provider_openai::credential::CodexCredentialAdminService::new(
            codex_repository.clone(),
            codex_token_client.clone(),
            codex_verifier.clone(),
            provider_leases.clone(),
            refresh_margin,
        )?,
    );
    let codex_runtime_refresh = Arc::new(
        provider_openai::credential::CodexCredentialRefreshService::new(
            codex_repository.clone(),
            codex_token_client.clone(),
            provider_leases.clone(),
            refresh_margin,
        )?,
    );
    let codex_quota = Arc::new(
        provider_openai::credential::CodexCredentialQuotaService::new(
            codex_repository.clone(),
            wire_profile,
        )?,
    );
    let codex_oauth: Arc<dyn provider_openai::credential::CodexOAuthAdmin> =
        Arc::new(provider_openai::credential::CodexOAuthAdminService::new(
            Arc::new(InMemoryCodexOAuthPendingStore::new()),
            codex_token_client,
            codex_verifier.clone(),
            Arc::clone(&core_account_store),
            provider_openai::credential::CodexCredentialAdmin,
        ));

    let xai_oauth = Arc::new(provider_xai::GrokOAuthClient::new(
        provider_xai::GrokOAuthConfig::official(provider_xai::transport::GROK_CLIENT_VERSION)?,
        Arc::new(provider_xai::ReqwestOAuthTransport::new()?),
        Arc::new(provider_xai::ReqwestOidcTokenVerifier::new()?),
    ));
    let xai_repository =
        provider_xai::GrokCredentialRepository::new(Arc::clone(&core_account_store));
    let xai_catalog_transport = Arc::new(provider_xai::ReqwestGrokModelCatalogTransport::new()?);
    let xai_catalog = Arc::new(provider_xai::GrokCredentialCatalogService::new(
        xai_repository.clone(),
        xai_catalog_transport.clone(),
        xai_cache.clone(),
    ));
    let xai_quota = Arc::new(provider_xai::GrokCredentialQuotaService::new(
        xai_repository.clone(),
        xai_catalog_transport,
    ));
    let xai_refresh = Arc::new(provider_xai::GrokCredentialRefreshService::new(
        xai_repository.clone(),
        Arc::new(provider_xai::GrokOAuthRefreshClient::new(Arc::clone(
            &xai_oauth,
        ))),
        Arc::clone(&xai_catalog),
        provider_leases.clone(),
        refresh_margin,
    )?);
    let xai_selector = Arc::new(provider_xai::GrokAccountSessionSelector::new(
        xai_repository.clone(),
        xai_cache,
        provider_leases,
    ));
    let xai_provider = Arc::new(provider_xai::GrokBuildProvider::new(
        xai_selector,
        Arc::new(provider_xai::ReqwestGrokInferenceTransport::new()?),
        Arc::clone(&xai_catalog),
    ));

    let mut registry = ProviderRegistry::builder();
    registry.register(codex_provider)?;
    registry.register(xai_provider)?;
    let providers = registry.build();
    let compiler = Arc::new(RuntimeSnapshotCompiler::new(
        snapshot_repository,
        providers.clone(),
    ));
    let snapshots = RuntimeSnapshotHandle::new(compiler.compile().await?);
    let publisher = RuntimeSnapshotPublisher::new(
        Arc::clone(&compiler),
        snapshots.clone(),
        runtime_changes.clone(),
    );
    let connections = ConnectionLifecycle::new();
    let openai = Arc::new(GatewayOpenAiService::new(
        snapshots.clone(),
        execution_store.clone(),
        providers,
        admissions,
        circuits,
        Arc::new(PgNativeContinuationResolver::new(history)),
        connections.clone(),
    ));

    let admin_backend = Arc::new(StoreAdminAuthBackend::new(
        security.clone(),
        runtime_settings.clone(),
        admin_auth_state,
    ));
    let sessions = Arc::new(DefaultAdminAuthService::new(
        app.admin.default_username.clone(),
        app.admin.session_ttl_minutes,
        admin_backend,
    ));
    sessions
        .ensure_default_admin(admin_default_password.expose_secret())
        .await?;
    let shutdown = TokioCancellationToken::new();
    let accounts_admin = Arc::new(AccountAdminAdapter::new(AccountAdminPorts {
        accounts: provider_accounts.clone(),
        admin_accounts: provider_accounts.clone(),
        core_store: core_account_store,
        control_plane: control_plane.clone(),
        instances: instances.clone(),
        observability: observability.clone(),
        security,
        codex_owner: Arc::clone(&codex_owner),
        codex_quota: Arc::clone(&codex_quota),
        codex_catalog: Arc::clone(&codex_catalog),
        xai_repository,
        xai_refresh: Arc::clone(&xai_refresh),
        xai_quota: Arc::clone(&xai_quota),
        xai_catalog: Arc::clone(&xai_catalog),
        connection_test: Arc::clone(&openai),
        publisher: publisher.clone(),
    }));
    let admin = Arc::new(AdminServices::new(AdminServicePorts {
        sessions,
        accounts: accounts_admin,
        catalog: Arc::new(CatalogAdminAdapter::new(
            control_plane.clone(),
            instances.clone(),
            publisher.clone(),
        )),
        client_keys: Arc::new(ClientKeyAdminAdapter::new(
            control_plane.clone(),
            client_keys,
            publisher.clone(),
        )),
        codex: Arc::new(CodexAdminAdapter::new(CodexAdminPorts {
            accounts: provider_accounts.clone(),
            admin_accounts: provider_accounts.clone(),
            core_store: provider_accounts.clone(),
            control_plane: control_plane.clone(),
            verifier: codex_verifier,
            oauth: codex_oauth,
            owner: codex_owner,
            publisher: publisher.clone(),
        })),
        observability: Arc::new(ObservabilityAdminAdapter::new(
            observability,
            provider_accounts.clone(),
            runtime_settings.clone(),
            app.wire_profile.dashboard_view(),
        )),
        settings: Arc::new(RuntimeSettingsAdminAdapter::new(
            control_plane.clone(),
            publisher.clone(),
        )),
        system: Arc::new(ProcessSystemAdminService::new(shutdown.clone())),
        xai: Arc::new(XaiAdminAdapter::new(
            provider_accounts.clone(),
            provider_accounts,
            control_plane,
            Arc::new(gateway_store::postgres::PgRuntimeSettingsRepository::new(
                pool.clone(),
            )),
            xai_oauth,
            publisher.clone(),
        )),
    }));

    let supervisor = start_workers(WorkerOwners {
        leases: credential_leases,
        codex_refresh: codex_runtime_refresh,
        codex_quota,
        codex_catalog,
        xai_refresh,
        xai_quota,
        xai_catalog,
        snapshot_publisher: publisher.clone(),
        instances: Arc::new(gateway_store::postgres::PgConfigCatalogRepository::new(
            pool.clone(),
        )),
        accounts: Arc::new(gateway_store::postgres::PgProviderAccountRepository::new(
            pool.clone(),
        )),
        ops_events: Arc::new(gateway_store::postgres::PgOpsEventRepository::new(
            pool.clone(),
        )),
        refresh_concurrency,
        execution: execution_store,
        retention,
    })?;
    let health = Arc::new(HealthStatus::new(
        snapshots.clone(),
        supervisor.health().clone(),
        pool.clone(),
        redis_connection,
    ));
    let runtime_settings_reader: Arc<dyn RuntimeSettingsRepository> = runtime_settings;
    let runtime_change_task = spawn_runtime_change_consumer(
        runtime_changes,
        publisher,
        runtime_settings_reader,
        shutdown.child_token(),
    );
    spawn_shutdown_signal(shutdown.clone());

    let state = ApplicationState::new(admin, openai.as_ref().clone(), health);
    let assets = env::var_os("CPR_WEB_DIST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_asset_directory);
    let router = application_router(state, assets);
    let listener =
        tokio::net::TcpListener::bind((app.server.host.as_str(), app.server.port)).await?;
    tracing::info!(host = %app.server.host, port = app.server.port, "网关开始监听");
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown.clone().cancelled_owned())
    .await?;

    shutdown.cancel();
    connections.shutdown().await;
    supervisor.shutdown(Duration::from_secs(30)).await;
    if let Err(error) = runtime_change_task.await {
        tracing::warn!(error = %error, "runtime change consumer did not stop cleanly");
    }
    pool.close().await;
    Ok(())
}

fn default_asset_directory() -> PathBuf {
    env::current_dir()
        .ok()
        .and_then(|directory| {
            directory
                .ancestors()
                .map(|ancestor| ancestor.join("frontend/dist"))
                .find(|candidate| candidate.is_dir())
        })
        .unwrap_or_else(|| PathBuf::from("web/dist"))
}

fn spawn_shutdown_signal(shutdown: TokioCancellationToken) {
    drop(tokio::spawn(async move {
        #[cfg(unix)]
        {
            let terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
            match terminate {
                Ok(mut terminate) => {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = terminate.recv() => {}
                        _ = shutdown.cancelled() => return,
                    }
                }
                Err(_) => {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = shutdown.cancelled() => return,
                    }
                }
            }
        }
        #[cfg(not(unix))]
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = shutdown.cancelled() => return,
        }
        shutdown.cancel();
    }));
}

fn spawn_runtime_change_consumer(
    repository: Arc<dyn gateway_store::redis::RuntimeChangeRepository>,
    publisher: RuntimeSnapshotPublisher,
    runtime_settings: Arc<dyn RuntimeSettingsRepository>,
    shutdown: TokioCancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let notification_consumer =
            consume_runtime_change_notifications(repository, publisher.clone(), shutdown.clone());
        let revision_reconciler = reconcile_runtime_revision(runtime_settings, publisher, shutdown);
        tokio::join!(notification_consumer, revision_reconciler);
    })
}

async fn consume_runtime_change_notifications(
    repository: Arc<dyn gateway_store::redis::RuntimeChangeRepository>,
    publisher: RuntimeSnapshotPublisher,
    shutdown: TokioCancellationToken,
) {
    let mut retry_delay = Duration::from_secs(1);
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        let subscription = repository.subscribe_runtime_changes().await;
        let mut subscription = match subscription {
            Ok(subscription) => {
                retry_delay = Duration::from_secs(1);
                subscription
            }
            Err(_) => {
                tracing::warn!("Redis runtime change 订阅失败，稍后重连");
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = tokio::time::sleep(retry_delay) => {}
                }
                retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
                continue;
            }
        };
        loop {
            let next = tokio::select! {
                _ = shutdown.cancelled() => return,
                next = futures::StreamExt::next(&mut subscription) => next,
            };
            match next {
                Some(Ok(gateway_store::redis::RuntimeChange::SnapshotPublished {
                    config_revision,
                })) => match publisher.refresh().await {
                    Ok(revision) if revision == config_revision => {}
                    Ok(revision) => tracing::debug!(
                        notified_revision = config_revision.get(),
                        loaded_revision = revision.get(),
                        "runtime change 通知已被更新 revision 覆盖"
                    ),
                    Err(error) => {
                        publisher.suspend();
                        tracing::warn!(error = %error, "跨进程 RuntimeSnapshot 刷新失败，数据面已暂停")
                    }
                },
                Some(Ok(gateway_store::redis::RuntimeChange::ProviderAccountChanged {
                    credential_revision,
                    ..
                })) => tracing::debug!(
                    credential_revision = credential_revision.get(),
                    "已消费账号 revision 通知；Provider 下次读取使用新 revision"
                ),
                Some(Err(_)) | None => break,
            }
        }
    }
}

async fn reconcile_runtime_revision(
    runtime_settings: Arc<dyn RuntimeSettingsRepository>,
    publisher: RuntimeSnapshotPublisher,
    shutdown: TokioCancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval.tick().await;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = interval.tick() => {}
        }
        let settings = match runtime_settings.load_runtime_settings().await {
            Ok(settings) => settings,
            Err(_) => {
                publisher.suspend();
                tracing::error!("Runtime revision 对账无法读取 PostgreSQL，数据面已暂停");
                continue;
            }
        };
        if !runtime_revision_needs_refresh(
            publisher.published_revision(),
            settings.config_revision.get(),
        ) {
            continue;
        }
        match publisher.refresh().await {
            Ok(revision) => tracing::info!(
                config_revision = revision.get(),
                "RuntimeSnapshot 已按 PostgreSQL revision 对账"
            ),
            Err(error) => {
                publisher.suspend();
                tracing::error!(error = %error, "Runtime revision 对账刷新失败，数据面已暂停");
            }
        }
    }
}

#[must_use]
pub fn runtime_revision_needs_refresh(
    published_revision: Option<u64>,
    persisted_revision: u64,
) -> bool {
    published_revision != Some(persisted_revision)
}

struct RedisWorkerLeaderLeasePort {
    repository: gateway_store::redis::RedisCredentialLeaseRepository,
    owner_id: String,
}

impl RedisWorkerLeaderLeasePort {
    fn new(repository: gateway_store::redis::RedisCredentialLeaseRepository) -> Self {
        Self {
            repository,
            owner_id: format!("worker-process-{}", Uuid::now_v7().simple()),
        }
    }
}

struct RedisWorkerLeaderLeaseGuard {
    repository: gateway_store::redis::RedisCredentialLeaseRepository,
    request: gateway_store::redis::CredentialLeaseRequest,
    grant: Option<gateway_store::redis::CredentialLeaseGrant>,
    token: crate::workers::WorkerFencingToken,
}

#[async_trait]
impl crate::workers::WorkerLeaderLeaseGuard for RedisWorkerLeaderLeaseGuard {
    fn fencing_token(&self) -> crate::workers::WorkerFencingToken {
        self.token
    }

    async fn renew(&mut self) -> Result<(), crate::workers::WorkerLeaseError> {
        let current = self
            .grant
            .as_ref()
            .ok_or_else(|| crate::workers::WorkerLeaseError::safe("worker lease is released"))?;
        let renewed = gateway_store::redis::CredentialLeaseRepository::renew_credential_lease(
            &self.repository,
            &self.request,
            current,
        )
        .await
        .map_err(|_| crate::workers::WorkerLeaseError::safe("worker lease renewal failed"))?
        .ok_or_else(|| crate::workers::WorkerLeaseError::safe("worker lease was lost"))?;
        self.grant = Some(renewed);
        Ok(())
    }
}

impl Drop for RedisWorkerLeaderLeaseGuard {
    fn drop(&mut self) {
        let Some(grant) = self.grant.take() else {
            return;
        };
        let repository = self.repository.clone();
        let request = self.request.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            drop(runtime.spawn(async move {
                let _ = gateway_store::redis::CredentialLeaseRepository::release_credential_lease(
                    &repository,
                    &request,
                    &grant,
                )
                .await;
            }));
        }
    }
}

#[async_trait]
impl crate::workers::WorkerLeaderLeasePort for RedisWorkerLeaderLeasePort {
    async fn try_acquire(
        &self,
        request: crate::workers::WorkerLeaseRequest,
    ) -> Result<crate::workers::WorkerLeaseAcquisition, crate::workers::WorkerLeaseError> {
        let lease_request = gateway_store::redis::CredentialLeaseRequest {
            scope: gateway_store::redis::CredentialLeaseScope::ProviderTask,
            resource_id: request.worker().to_string(),
            owner_id: self.owner_id.clone(),
            ttl: request.ttl(),
        };
        let grant = gateway_store::redis::CredentialLeaseRepository::acquire_credential_lease(
            &self.repository,
            &lease_request,
        )
        .await
        .map_err(|_| crate::workers::WorkerLeaseError::safe("worker lease acquisition failed"))?;
        let Some(grant) = grant else {
            return Ok(crate::workers::WorkerLeaseAcquisition::Busy { retry_after: None });
        };
        let token = std::num::NonZeroU64::new(grant.fencing_token.get())
            .map(crate::workers::WorkerFencingToken::new)
            .ok_or_else(|| crate::workers::WorkerLeaseError::safe("worker fence is invalid"))?;
        Ok(crate::workers::WorkerLeaseAcquisition::Acquired(Box::new(
            RedisWorkerLeaderLeaseGuard {
                repository: self.repository.clone(),
                request: lease_request,
                grant: Some(grant),
                token,
            },
        )))
    }
}

struct CodexOAuthRefreshTask {
    service: Arc<provider_openai::credential::CodexCredentialRefreshService>,
    limit: u32,
    permits: Arc<tokio::sync::Semaphore>,
    ops_events: Arc<dyn gateway_store::postgres::OpsEventRepository>,
}

#[async_trait]
impl crate::workers::WorkerTask for CodexOAuthRefreshTask {
    async fn run_cycle(
        &self,
        context: crate::workers::WorkerCycleContext,
    ) -> Result<(), crate::workers::WorkerTaskError> {
        if context.is_cancelled() {
            return Ok(());
        }
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| crate::workers::WorkerTaskError::safe("OAuth refresh stopped"))?;
        let outcomes = self
            .service
            .refresh_due(self.limit)
            .await
            .map_err(|_| crate::workers::WorkerTaskError::safe("Codex OAuth refresh failed"))?;
        for outcome in &outcomes {
            if let Some((account_id, failure_kind)) = codex_refresh_failure(outcome) {
                append_worker_account_ops_event(
                    self.ops_events.as_ref(),
                    "openai",
                    None,
                    account_id,
                    "oauth_refresh",
                    failure_kind,
                )
                .await?;
            }
        }
        Ok(())
    }
}

struct XaiOAuthRefreshTask {
    service: Arc<provider_xai::GrokCredentialRefreshService>,
    limit: u32,
    permits: Arc<tokio::sync::Semaphore>,
    ops_events: Arc<dyn gateway_store::postgres::OpsEventRepository>,
}

#[async_trait]
impl crate::workers::WorkerTask for XaiOAuthRefreshTask {
    async fn run_cycle(
        &self,
        context: crate::workers::WorkerCycleContext,
    ) -> Result<(), crate::workers::WorkerTaskError> {
        if context.is_cancelled() {
            return Ok(());
        }
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| crate::workers::WorkerTaskError::safe("OAuth refresh stopped"))?;
        let outcomes = self
            .service
            .refresh_due(self.limit)
            .await
            .map_err(|_| crate::workers::WorkerTaskError::safe("xAI OAuth refresh failed"))?;
        for outcome in &outcomes {
            if let Some((account_id, failure_kind)) = xai_refresh_failure(outcome) {
                append_worker_account_ops_event(
                    self.ops_events.as_ref(),
                    "xai",
                    None,
                    account_id,
                    "oauth_refresh",
                    failure_kind,
                )
                .await?;
            }
        }
        Ok(())
    }
}

fn codex_refresh_failure(
    outcome: &provider_openai::credential::CodexCredentialRefreshOutcome,
) -> Option<(&str, &'static str)> {
    use provider_openai::credential::CodexCredentialRefreshOutcome as Outcome;
    match outcome {
        Outcome::Invalidated { account_id } => Some((account_id, "refresh_invalidated")),
        Outcome::Banned { account_id } => Some((account_id, "refresh_banned")),
        Outcome::Transient { account_id } => Some((account_id, "refresh_transient")),
        Outcome::Ambiguous { account_id } => Some((account_id, "refresh_ambiguous")),
        Outcome::Failed { account_id } => Some((account_id, "refresh_failed")),
        Outcome::Refreshed { .. } | Outcome::LeaseUnavailable { .. } | Outcome::Stale { .. } => {
            None
        }
    }
}

fn xai_refresh_failure(
    outcome: &provider_xai::GrokCredentialRefreshOutcome,
) -> Option<(&str, &'static str)> {
    use provider_xai::GrokCredentialRefreshOutcome as Outcome;
    match outcome {
        Outcome::Invalidated { account_id } => Some((account_id.as_str(), "refresh_invalidated")),
        Outcome::Ambiguous { account_id } => Some((account_id.as_str(), "refresh_ambiguous")),
        Outcome::Transient { account_id } => Some((account_id.as_str(), "refresh_transient")),
        Outcome::Rejected { account_id } => Some((account_id.as_str(), "refresh_rejected")),
        Outcome::Failed { account_id } => Some((account_id.as_str(), "refresh_failed")),
        Outcome::Refreshed { .. } | Outcome::LeaseUnavailable { .. } | Outcome::Stale { .. } => {
            None
        }
    }
}

struct ProviderQuotaCatalogTask {
    instances: Arc<dyn ConfigCatalogRepository>,
    accounts: Arc<dyn ProviderAccountRepository>,
    ops_events: Arc<dyn gateway_store::postgres::OpsEventRepository>,
    codex_quota: Arc<provider_openai::credential::CodexCredentialQuotaService>,
    codex_catalog: Arc<provider_openai::credential::CodexCredentialCatalogService>,
    xai_quota: Arc<provider_xai::GrokCredentialQuotaService>,
    xai_catalog: Arc<provider_xai::GrokCredentialCatalogService>,
    snapshot_publisher: RuntimeSnapshotPublisher,
}

#[async_trait]
impl crate::workers::WorkerTask for ProviderQuotaCatalogTask {
    async fn run_cycle(
        &self,
        context: crate::workers::WorkerCycleContext,
    ) -> Result<(), crate::workers::WorkerTaskError> {
        let instances = self
            .instances
            .list_provider_instances(false)
            .await
            .map_err(|_| crate::workers::WorkerTaskError::safe("Provider catalog read failed"))?;
        for instance in instances {
            if context.is_cancelled() {
                return Ok(());
            }
            let accounts = self
                .accounts
                .list_provider_accounts(Some(&instance.id), false)
                .await
                .map_err(|_| {
                    crate::workers::WorkerTaskError::safe("Provider account catalog read failed")
                })?;
            if accounts.is_empty() {
                continue;
            }
            match instance.provider_kind.as_str() {
                "openai" => {
                    let core_instance = core_provider_instance(&instance).map_err(|_| {
                        crate::workers::WorkerTaskError::safe("Codex instance is invalid")
                    })?;
                    for account in accounts {
                        if context.is_cancelled() {
                            return Ok(());
                        }
                        let account_id =
                            match gateway_core::engine::credential::ProviderAccountId::new(
                                account.id.clone(),
                            ) {
                                Ok(account_id) => account_id,
                                Err(_) => {
                                    self.record_account_failure(
                                        &instance,
                                        &account.id,
                                        "account_validation",
                                        "invalid_account_id",
                                    )
                                    .await?;
                                    continue;
                                }
                            };
                        if self
                            .codex_quota
                            .refresh_account(&core_instance, &account_id)
                            .await
                            .is_err()
                        {
                            self.record_account_failure(
                                &instance,
                                account_id.as_str(),
                                "quota_refresh",
                                "provider_account_quota_refresh_failed",
                            )
                            .await?;
                        }
                        if self
                            .codex_catalog
                            .synchronize_account(&core_instance, &account_id)
                            .await
                            .is_err()
                        {
                            self.record_account_failure(
                                &instance,
                                account_id.as_str(),
                                "catalog_refresh",
                                "provider_account_catalog_refresh_failed",
                            )
                            .await?;
                        }
                    }
                }
                "xai" => {
                    for account in accounts {
                        if context.is_cancelled() {
                            return Ok(());
                        }
                        let account_id =
                            match gateway_core::engine::credential::ProviderAccountId::new(
                                account.id.clone(),
                            ) {
                                Ok(account_id) => account_id,
                                Err(_) => {
                                    self.record_account_failure(
                                        &instance,
                                        &account.id,
                                        "account_validation",
                                        "invalid_account_id",
                                    )
                                    .await?;
                                    continue;
                                }
                            };
                        if self.xai_quota.refresh_account(&account_id).await.is_err() {
                            self.record_account_failure(
                                &instance,
                                account_id.as_str(),
                                "quota_refresh",
                                "provider_account_quota_refresh_failed",
                            )
                            .await?;
                        }
                        if self
                            .xai_catalog
                            .refresh_account_catalog(
                                &account_id,
                                provider_xai::transport::GROK_CLIENT_VERSION,
                            )
                            .await
                            .is_err()
                        {
                            self.record_account_failure(
                                &instance,
                                account_id.as_str(),
                                "catalog_refresh",
                                "provider_account_catalog_refresh_failed",
                            )
                            .await?;
                        }
                    }
                }
                _ => {}
            }
        }
        self.snapshot_publisher.refresh().await.map_err(|error| {
            tracing::warn!(error = %error, "Provider catalog cycle could not refresh RuntimeSnapshot");
            crate::workers::WorkerTaskError::safe("Runtime snapshot catalog refresh failed")
        })?;
        Ok(())
    }
}

impl ProviderQuotaCatalogTask {
    async fn record_account_failure(
        &self,
        instance: &gateway_store::postgres::ProviderInstanceRecord,
        account_id: &str,
        operation: &'static str,
        failure_kind: &'static str,
    ) -> Result<(), crate::workers::WorkerTaskError> {
        append_worker_account_ops_event(
            self.ops_events.as_ref(),
            &instance.provider_kind,
            Some(&instance.id),
            account_id,
            operation,
            failure_kind,
        )
        .await
    }
}

async fn append_worker_account_ops_event(
    ops_events: &dyn gateway_store::postgres::OpsEventRepository,
    provider_kind: &str,
    provider_instance_id: Option<&str>,
    account_id: &str,
    operation: &'static str,
    failure_kind: &'static str,
) -> Result<(), crate::workers::WorkerTaskError> {
    tracing::warn!(
        provider_kind,
        operation,
        "Provider 单账号后台同步失败，继续处理其他账号"
    );
    ops_events
        .append_ops_event(gateway_store::postgres::OpsEvent {
            id: format!("ops_{}", Uuid::now_v7().simple()),
            model_request_id: None,
            attempt_index: None,
            level: gateway_store::postgres::OpsEventLevel::Warning,
            component: "worker".to_owned(),
            operation: operation.to_owned(),
            provider_instance_id: provider_instance_id.map(str::to_owned),
            provider_kind: Some(provider_kind.to_owned()),
            provider_account_id: Some(account_id.to_owned()),
            provider_account_ref: Some(account_id.to_owned()),
            upstream_model_id: None,
            failure_kind: failure_kind.to_owned(),
            status_code: None,
            provider_error_code: None,
            retry_after_ms: None,
            upstream_request_id: None,
            latency_ms: None,
            message: "Provider account background synchronization failed".to_owned(),
            occurrence_count: 1,
            created_at: Utc::now(),
        })
        .await
        .map_err(|_| crate::workers::WorkerTaskError::safe("worker ops event write failed"))
}

struct StaleModelRequestRecoveryTask {
    repository: Arc<gateway_store::postgres::PgExecutionStore>,
}

#[async_trait]
impl crate::workers::WorkerTask for StaleModelRequestRecoveryTask {
    async fn run_cycle(
        &self,
        _context: crate::workers::WorkerCycleContext,
    ) -> Result<(), crate::workers::WorkerTaskError> {
        gateway_store::postgres::ModelRequestRepository::recover_expired_model_requests(
            self.repository.as_ref(),
            Utc::now(),
        )
        .await
        .map(|_| ())
        .map_err(|_| crate::workers::WorkerTaskError::safe("stale request recovery failed"))
    }
}

struct RetentionTask {
    repository: Arc<gateway_store::postgres::PgRetentionRepository>,
}

#[async_trait]
impl crate::workers::WorkerTask for RetentionTask {
    async fn run_cycle(
        &self,
        _context: crate::workers::WorkerCycleContext,
    ) -> Result<(), crate::workers::WorkerTaskError> {
        let settings = gateway_store::postgres::RetentionRepository::load_retention_settings(
            self.repository.as_ref(),
        )
        .await
        .map_err(|_| crate::workers::WorkerTaskError::safe("retention settings read failed"))?;
        gateway_store::postgres::RetentionRepository::apply_retention(
            self.repository.as_ref(),
            Utc::now(),
            settings,
        )
        .await
        .map(|_| ())
        .map_err(|_| crate::workers::WorkerTaskError::safe("retention cleanup failed"))
    }
}

struct WorkerOwners {
    leases: gateway_store::redis::RedisCredentialLeaseRepository,
    codex_refresh: Arc<provider_openai::credential::CodexCredentialRefreshService>,
    codex_quota: Arc<provider_openai::credential::CodexCredentialQuotaService>,
    codex_catalog: Arc<provider_openai::credential::CodexCredentialCatalogService>,
    xai_refresh: Arc<provider_xai::GrokCredentialRefreshService>,
    xai_quota: Arc<provider_xai::GrokCredentialQuotaService>,
    xai_catalog: Arc<provider_xai::GrokCredentialCatalogService>,
    snapshot_publisher: RuntimeSnapshotPublisher,
    instances: Arc<dyn ConfigCatalogRepository>,
    accounts: Arc<dyn ProviderAccountRepository>,
    ops_events: Arc<dyn gateway_store::postgres::OpsEventRepository>,
    refresh_concurrency: u32,
    execution: Arc<gateway_store::postgres::PgExecutionStore>,
    retention: Arc<gateway_store::postgres::PgRetentionRepository>,
}

fn start_workers(
    owners: WorkerOwners,
) -> Result<crate::workers::WorkerSupervisor, crate::workers::WorkerRegistryError> {
    use crate::workers::{WorkerDisabledReason, WorkerKind, WorkerRegistry, WorkerSchedule};
    let schedule = |interval| {
        WorkerSchedule::new(
            interval,
            Duration::from_secs(1),
            Duration::from_secs(60),
            Duration::from_secs(15 * 60),
        )
    };
    let mut registry = WorkerRegistry::new();
    let refresh_permits = Arc::new(tokio::sync::Semaphore::new(
        usize::try_from(owners.refresh_concurrency).unwrap_or(usize::MAX),
    ));
    registry.register(
        WorkerKind::OAuthRefresh,
        "openai",
        Arc::new(CodexOAuthRefreshTask {
            service: owners.codex_refresh,
            limit: owners.refresh_concurrency,
            permits: Arc::clone(&refresh_permits),
            ops_events: Arc::clone(&owners.ops_events),
        }),
        schedule(Duration::from_secs(30))?,
    )?;
    registry.register(
        WorkerKind::OAuthRefresh,
        "xai",
        Arc::new(XaiOAuthRefreshTask {
            service: owners.xai_refresh,
            limit: owners.refresh_concurrency,
            permits: refresh_permits,
            ops_events: Arc::clone(&owners.ops_events),
        }),
        schedule(Duration::from_secs(30))?,
    )?;
    registry.register(
        WorkerKind::QuotaCatalogHealth,
        "provider-registry",
        Arc::new(ProviderQuotaCatalogTask {
            instances: owners.instances,
            accounts: owners.accounts,
            ops_events: owners.ops_events,
            codex_quota: owners.codex_quota,
            codex_catalog: owners.codex_catalog,
            xai_quota: owners.xai_quota,
            xai_catalog: owners.xai_catalog,
            snapshot_publisher: owners.snapshot_publisher,
        }),
        schedule(Duration::from_secs(5 * 60))?,
    )?;
    registry.disable(WorkerDisabledReason::NoPersistentNativeClaimState)?;
    registry.register(
        WorkerKind::StaleModelRequestRecovery,
        "postgres",
        Arc::new(StaleModelRequestRecoveryTask {
            repository: owners.execution,
        }),
        schedule(Duration::from_secs(30))?,
    )?;
    registry.register(
        WorkerKind::Retention,
        "postgres",
        Arc::new(RetentionTask {
            repository: owners.retention,
        }),
        schedule(Duration::from_secs(60 * 60))?,
    )?;
    registry.disable(WorkerDisabledReason::NoBufferedOpsEvents)?;
    registry.start(Arc::new(RedisWorkerLeaderLeasePort::new(owners.leases)))
}

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};

use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

const LOG_FILE_PREFIX: &str = "codex-proxy-rs";

/// non-blocking 日志 writer 的进程级守卫。
pub struct LogGuard {
    _writers: Vec<WorkerGuard>,
}

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("log IO failed")]
    Io(#[from] io::Error),
    #[error("logging filter is invalid")]
    InvalidFilter,
    #[error("global tracing subscriber is already initialized")]
    AlreadyInitialized,
    #[error("logging size limit is too large")]
    SizeOverflow,
}

/// 按自然日、单文件大小、保留天数和文件总数初始化结构化日志。
pub fn initialize_logging(config: &LoggingConfig) -> Result<LogGuard, LogError> {
    let directive = env::var("RUST_LOG").unwrap_or_else(|_| config.level.clone());
    let filter = EnvFilter::try_new(directive).map_err(|_| LogError::InvalidFilter)?;
    let mut guards = Vec::new();

    let stdout_writer = config.stdout.then(|| {
        let (writer, guard) = NonBlockingBuilder::default()
            .thread_name("gateway-log-stdout")
            .finish(io::stdout());
        guards.push(guard);
        writer
    });
    let file_writer = if config.file.enabled {
        let maximum_bytes = config
            .file
            .max_file_size_mb
            .checked_mul(1024 * 1024)
            .ok_or(LogError::SizeOverflow)?;
        let writer = RotatingLogWriter::open(
            config.file.directory.clone(),
            maximum_bytes,
            config.file.retention_days,
            config.file.max_files,
        )?;
        let (writer, guard) = NonBlockingBuilder::default()
            .thread_name("gateway-log-file")
            .finish(writer);
        guards.push(guard);
        Some(writer)
    } else {
        None
    };

    let stdout_layer = stdout_writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .compact()
            .with_writer(writer)
            .with_target(true)
            .with_ansi(false)
    });
    let file_layer = file_writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(writer)
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_current_span(true)
            .with_span_list(true)
    });
    tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer)
        .with(file_layer)
        .try_init()
        .map_err(|_| LogError::AlreadyInitialized)?;
    Ok(LogGuard { _writers: guards })
}

struct RotatingLogWriter {
    directory: PathBuf,
    maximum_bytes: u64,
    retention_days: usize,
    maximum_files: usize,
    date: chrono::NaiveDate,
    segment: usize,
    bytes_written: u64,
    file: File,
}

impl RotatingLogWriter {
    fn open(
        directory: PathBuf,
        maximum_bytes: u64,
        retention_days: usize,
        maximum_files: usize,
    ) -> io::Result<Self> {
        fs::create_dir_all(&directory)?;
        let date = Utc::now().date_naive();
        cleanup_log_files(&directory, date, retention_days, maximum_files)?;
        let (segment, bytes_written) = writable_log_segment(&directory, date, maximum_bytes)?;
        let file = open_log_segment(&directory, date, segment)?;
        Ok(Self {
            directory,
            maximum_bytes,
            retention_days,
            maximum_files,
            date,
            segment,
            bytes_written,
            file,
        })
    }

    fn rotate_if_required(&mut self, incoming_bytes: usize) -> io::Result<()> {
        let date = Utc::now().date_naive();
        let incoming_bytes = u64::try_from(incoming_bytes).unwrap_or(u64::MAX);
        let day_changed = date != self.date;
        let size_exceeded = !day_changed
            && self.bytes_written > 0
            && self.bytes_written.saturating_add(incoming_bytes) > self.maximum_bytes;
        if !day_changed && !size_exceeded {
            return Ok(());
        }
        self.file.flush()?;
        if day_changed {
            self.date = date;
            self.segment = 0;
        } else {
            self.segment = self.segment.saturating_add(1);
        }
        self.file = open_log_segment(&self.directory, self.date, self.segment)?;
        self.bytes_written = self.file.metadata()?.len();
        cleanup_log_files(
            &self.directory,
            self.date,
            self.retention_days,
            self.maximum_files,
        )
    }
}

impl Write for RotatingLogWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.rotate_if_required(buffer.len())?;
        let written = self.file.write(buffer)?;
        self.bytes_written = self
            .bytes_written
            .saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[derive(Debug)]
struct ManagedLogFile {
    date: chrono::NaiveDate,
    segment: usize,
    path: PathBuf,
}

fn writable_log_segment(
    directory: &Path,
    date: chrono::NaiveDate,
    maximum_bytes: u64,
) -> io::Result<(usize, u64)> {
    let latest = managed_log_files(directory)?
        .into_iter()
        .filter(|entry| entry.date == date)
        .max_by_key(|entry| entry.segment);
    let Some(latest) = latest else {
        return Ok((0, 0));
    };
    let length = latest.path.metadata()?.len();
    if length >= maximum_bytes {
        Ok((latest.segment.saturating_add(1), 0))
    } else {
        Ok((latest.segment, length))
    }
}

fn open_log_segment(directory: &Path, date: chrono::NaiveDate, segment: usize) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(log_file_name(date, segment)))
}

fn log_file_name(date: chrono::NaiveDate, segment: usize) -> String {
    if segment == 0 {
        format!("{LOG_FILE_PREFIX}.{date}.log")
    } else {
        format!("{LOG_FILE_PREFIX}.{date}.{segment}.log")
    }
}

fn managed_log_files(directory: &Path) -> io::Result<Vec<ManagedLogFile>> {
    Ok(fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            parse_log_file_name(entry.file_name().to_string_lossy().as_ref()).map(
                |(date, segment)| ManagedLogFile {
                    date,
                    segment,
                    path: entry.path(),
                },
            )
        })
        .collect())
}

fn parse_log_file_name(name: &str) -> Option<(chrono::NaiveDate, usize)> {
    let body = name
        .strip_prefix(&format!("{LOG_FILE_PREFIX}."))?
        .strip_suffix(".log")?;
    let (date, segment) = body.split_once('.').map_or((body, 0), |(date, segment)| {
        (date, segment.parse().ok().unwrap_or(usize::MAX))
    });
    if segment == usize::MAX {
        return None;
    }
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .map(|date| (date, segment))
}

fn cleanup_log_files(
    directory: &Path,
    today: chrono::NaiveDate,
    retention_days: usize,
    maximum_files: usize,
) -> io::Result<()> {
    let retention_offset = i64::try_from(retention_days.saturating_sub(1)).unwrap_or(i64::MAX);
    let cutoff = today
        .checked_sub_signed(chrono::Duration::days(retention_offset))
        .unwrap_or(chrono::NaiveDate::MIN);
    let mut retained = Vec::new();
    for entry in managed_log_files(directory)? {
        if entry.date < cutoff {
            fs::remove_file(entry.path)?;
        } else {
            retained.push(entry);
        }
    }
    retained.sort_by(|left, right| {
        right
            .date
            .cmp(&left.date)
            .then_with(|| right.segment.cmp(&left.segment))
    });
    for entry in retained.into_iter().skip(maximum_files) {
        fs::remove_file(entry.path)?;
    }
    Ok(())
}
