//! 配置设置领域逻辑与运行时数据库服务。

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock as StdRwLock},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use thiserror::Error;

use crate::config::types::AppConfig;
use crate::upstream::accounts::pool::{
    AccountPoolOptions, RotationStrategy, RuntimeAccountPoolService,
};

const ROTATION_STRATEGIES: [&str; 3] = ["least_used", "round_robin", "sticky"];
const RUNTIME_SETTINGS_ID: i64 = 1;

/// 管理端可变设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminSettings {
    /// 默认模型 ID。
    pub default_model: String,
    /// 默认 reasoning effort。
    pub default_reasoning_effort: Option<String>,
    /// 默认服务层级。
    pub service_tier: Option<String>,
    /// 模型别名映射。
    pub model_aliases: BTreeMap<String, String>,
    /// 模型到账号 ID 的显式路由。
    pub model_account_routes: BTreeMap<String, Vec<String>>,
    /// 是否启用访问令牌刷新。
    pub refresh_enabled: bool,
    /// 访问令牌过期前多少秒开始刷新。
    pub refresh_margin_seconds: u64,
    /// 访问令牌刷新并发数。
    pub refresh_concurrency: u32,
    /// 单账号最大并发请求数。
    pub max_concurrent_per_account: usize,
    /// 同账号请求间隔毫秒数。
    pub request_interval_ms: u64,
    /// 账号轮换策略。
    pub rotation_strategy: String,
    /// 计划类型优先级。
    pub tier_priority: Vec<String>,
    /// 配额刷新间隔分钟数。
    pub quota_refresh_interval_minutes: u64,
    /// 配额预警阈值。
    pub quota_warning_thresholds: AdminQuotaWarningThresholds,
    /// 配额耗尽账号是否跳过调度。
    pub quota_skip_exhausted: bool,
    /// 是否启用事件日志。
    pub logs_enabled: bool,
    /// 事件日志容量。
    pub logs_capacity: u32,
    /// 事件日志是否捕获请求/响应体。
    pub logs_capture_body: bool,
    /// 用量历史保留天数。
    pub usage_history_retention_days: Option<u64>,
}

impl Default for AdminSettings {
    fn default() -> Self {
        Self {
            default_model: "gpt-4o".to_string(),
            default_reasoning_effort: None,
            service_tier: None,
            model_aliases: BTreeMap::new(),
            model_account_routes: BTreeMap::new(),
            refresh_enabled: true,
            refresh_margin_seconds: 300,
            refresh_concurrency: 2,
            max_concurrent_per_account: 3,
            request_interval_ms: 50,
            rotation_strategy: "least_used".to_string(),
            tier_priority: Vec::new(),
            quota_refresh_interval_minutes: 5,
            quota_warning_thresholds: AdminQuotaWarningThresholds::default(),
            quota_skip_exhausted: true,
            logs_enabled: false,
            logs_capacity: 2_000,
            logs_capture_body: false,
            usage_history_retention_days: None,
        }
    }
}

/// 配额预警阈值集合。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminQuotaWarningThresholds {
    /// primary 配额预警百分比。
    pub primary: Vec<u8>,
    /// secondary 配额预警百分比。
    pub secondary: Vec<u8>,
}

impl Default for AdminQuotaWarningThresholds {
    fn default() -> Self {
        Self {
            primary: vec![80, 90],
            secondary: vec![80, 90],
        }
    }
}

/// 管理端设置补丁。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSettingsPatch {
    /// 默认模型 ID。
    pub default_model: Option<String>,
    /// 模型别名映射。
    pub model_aliases: Option<BTreeMap<String, String>>,
    /// 模型到账号 ID 的显式路由。
    pub model_account_routes: Option<BTreeMap<String, Vec<String>>>,
    /// 访问令牌过期前多少秒开始刷新。
    pub refresh_margin_seconds: Option<u64>,
    /// 访问令牌刷新并发数。
    pub refresh_concurrency: Option<u32>,
    /// 单账号最大并发请求数。
    pub max_concurrent_per_account: Option<usize>,
    /// 同账号请求间隔毫秒数。
    pub request_interval_ms: Option<u64>,
    /// 账号轮换策略。
    pub rotation_strategy: Option<String>,
}

/// 设置领域服务。
#[derive(Debug, Clone, Default)]
pub struct SettingsService;

impl SettingsService {
    /// 将管理端设置补丁应用到当前设置。
    pub fn apply_patch(
        current: &mut AdminSettings,
        patch: AdminSettingsPatch,
    ) -> Result<(), SettingsServiceError> {
        if let Some(default_model) = patch.default_model {
            current.default_model = non_empty_string("defaultModel", default_model)?;
        }
        if let Some(model_aliases) = patch.model_aliases {
            current.model_aliases = validate_model_aliases(model_aliases)?;
        }
        if let Some(model_account_routes) = patch.model_account_routes {
            current.model_account_routes = validate_model_account_routes(model_account_routes)?;
        }
        if let Some(refresh_margin_seconds) = patch.refresh_margin_seconds {
            current.refresh_margin_seconds =
                positive_u64("refreshMarginSeconds", refresh_margin_seconds)?;
        }
        if let Some(refresh_concurrency) = patch.refresh_concurrency {
            current.refresh_concurrency = positive_u32("refreshConcurrency", refresh_concurrency)?;
        }
        if let Some(max_concurrent_per_account) = patch.max_concurrent_per_account {
            current.max_concurrent_per_account =
                positive_usize("maxConcurrentPerAccount", max_concurrent_per_account)?;
        }
        if let Some(request_interval_ms) = patch.request_interval_ms {
            current.request_interval_ms = request_interval_ms;
        }
        if let Some(rotation_strategy) = patch.rotation_strategy {
            current.rotation_strategy = validate_rotation_strategy(rotation_strategy)?;
        }
        Ok(())
    }
}

/// 设置领域错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SettingsServiceError {
    /// 字段值无效。
    #[error("invalid setting `{field}`: {message}")]
    InvalidField {
        /// 字段名。
        field: String,
        /// 错误说明。
        message: String,
    },
}

impl SettingsServiceError {
    /// 返回无效字段名。
    pub fn field(&self) -> &str {
        match self {
            Self::InvalidField { field, .. } => field,
        }
    }
    /// 返回字段错误说明。
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidField { message, .. } => message,
        }
    }
}

fn validate_model_aliases(
    aliases: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, SettingsServiceError> {
    let mut normalized = BTreeMap::new();
    for (alias, target) in aliases {
        let alias = non_empty_string("modelAliases", alias)?;
        let target = non_empty_string("modelAliases", target)?;
        if alias == target {
            return Err(invalid_field(
                "modelAliases",
                "alias and target must differ",
            ));
        }
        normalized.insert(alias, target);
    }
    Ok(normalized)
}

fn validate_model_account_routes(
    routes: BTreeMap<String, Vec<String>>,
) -> Result<BTreeMap<String, Vec<String>>, SettingsServiceError> {
    let mut normalized = BTreeMap::new();
    for (model, account_ids) in routes {
        let model = non_empty_string("modelAccountRoutes", model)?;
        let mut normalized_account_ids = Vec::new();
        for account_id in account_ids {
            let account_id = non_empty_string("modelAccountRoutes", account_id)?;
            if normalized_account_ids.contains(&account_id) {
                return Err(invalid_field(
                    "modelAccountRoutes",
                    format!("duplicate account id `{account_id}`"),
                ));
            }
            normalized_account_ids.push(account_id);
        }
        if normalized_account_ids.is_empty() {
            return Err(invalid_field(
                "modelAccountRoutes",
                "account list must not be empty",
            ));
        }
        normalized.insert(model, normalized_account_ids);
    }
    Ok(normalized)
}

fn validate_rotation_strategy(strategy: String) -> Result<String, SettingsServiceError> {
    let strategy = non_empty_string("rotationStrategy", strategy)?;
    if ROTATION_STRATEGIES.contains(&strategy.as_str()) {
        Ok(strategy)
    } else {
        Err(invalid_field(
            "rotationStrategy",
            "must be one of least_used, round_robin, sticky",
        ))
    }
}

fn non_empty_string(field: &str, value: String) -> Result<String, SettingsServiceError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        Err(invalid_field(field, "must not be empty"))
    } else {
        Ok(value)
    }
}

fn positive_u64(field: &str, value: u64) -> Result<u64, SettingsServiceError> {
    if value == 0 {
        Err(invalid_field(field, "must be greater than 0"))
    } else {
        Ok(value)
    }
}

fn positive_u32(field: &str, value: u32) -> Result<u32, SettingsServiceError> {
    if value == 0 {
        Err(invalid_field(field, "must be greater than 0"))
    } else {
        Ok(value)
    }
}

fn positive_usize(field: &str, value: usize) -> Result<usize, SettingsServiceError> {
    if value == 0 {
        Err(invalid_field(field, "must be greater than 0"))
    } else {
        Ok(value)
    }
}

fn invalid_field(field: &str, message: impl Into<String>) -> SettingsServiceError {
    SettingsServiceError::InvalidField {
        field: field.to_string(),
        message: message.into(),
    }
}

/// 运行时设置服务。
#[derive(Clone)]
pub struct RuntimeSettingsService {
    current: Arc<StdRwLock<Arc<AppConfig>>>,
    pool: SqlitePool,
    account_pool: Option<Arc<RuntimeAccountPoolService>>,
}

impl RuntimeSettingsService {
    /// 构造运行时设置服务。
    pub fn new(config: AppConfig, pool: SqlitePool) -> Self {
        Self {
            current: Arc::new(StdRwLock::new(Arc::new(config))),
            pool,
            account_pool: None,
        }
    }

    /// 构造带账号池同步的运行时设置服务。
    pub fn with_account_pool(
        config: AppConfig,
        pool: SqlitePool,
        account_pool: Arc<RuntimeAccountPoolService>,
    ) -> Self {
        Self {
            current: Arc::new(StdRwLock::new(Arc::new(config))),
            pool,
            account_pool: Some(account_pool),
        }
    }

    /// 返回当前运行时配置快照。
    pub fn current(&self) -> Arc<AppConfig> {
        self.current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// 初始化缺省运行设置，并返回数据库中的运行配置快照。
    pub async fn load_or_initialize_config(
        mut config: AppConfig,
        pool: &SqlitePool,
    ) -> Result<AppConfig, RuntimeSettingsError> {
        let settings = admin_settings_from_config(&config);
        insert_runtime_settings_if_missing(pool, &settings).await?;
        let settings = load_runtime_settings(pool).await?;
        apply_admin_settings_to_config(&mut config, settings);
        Ok(config)
    }

    /// 应用设置补丁、写入数据库并更新运行时配置快照。
    pub async fn update(
        &self,
        patch: AdminSettingsPatch,
    ) -> Result<Arc<AppConfig>, RuntimeSettingsError> {
        let mut next = (*self.current()).clone();
        let mut settings = admin_settings_from_config(&next);
        SettingsService::apply_patch(&mut settings, patch)?;
        save_runtime_settings(&self.pool, &settings).await?;
        apply_admin_settings_to_config(&mut next, settings);
        if let Some(account_pool) = &self.account_pool {
            account_pool
                .apply_options(
                    account_pool_options_from_config(&next),
                    next.auth.request_interval_ms,
                )
                .await;
        }
        let next = Arc::new(next);
        *self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next.clone();
        Ok(next)
    }
}

/// 运行时设置错误。
#[derive(Debug, Error)]
pub enum RuntimeSettingsError {
    /// 设置字段校验失败。
    #[error(transparent)]
    InvalidField(#[from] SettingsServiceError),
    /// 数据库操作失败。
    #[error("runtime settings database error: {0}")]
    Database(#[from] sqlx::Error),
    /// JSON 编解码失败。
    #[error("runtime settings json error: {0}")]
    Json(#[from] serde_json::Error),
    /// 数据库存储值非法。
    #[error("invalid stored setting `{field}`: {message}")]
    StoredField {
        /// 字段名。
        field: String,
        /// 错误说明。
        message: String,
    },
}

fn admin_settings_from_config(config: &AppConfig) -> AdminSettings {
    let defaults = AdminSettings::default();
    AdminSettings {
        default_model: config.model.default_model.clone(),
        default_reasoning_effort: config.model.default_reasoning_effort.clone(),
        service_tier: config.model.service_tier.clone(),
        model_aliases: config.model.aliases.clone(),
        model_account_routes: config.model.account_routes.clone(),
        refresh_enabled: defaults.refresh_enabled,
        refresh_margin_seconds: config.auth.refresh_margin_seconds,
        refresh_concurrency: config.auth.refresh_concurrency,
        max_concurrent_per_account: config.auth.max_concurrent_per_account,
        request_interval_ms: config.auth.request_interval_ms,
        rotation_strategy: config.auth.rotation_strategy.clone(),
        tier_priority: config.auth.tier_priority.clone(),
        quota_refresh_interval_minutes: config.quota.refresh_interval_minutes,
        quota_warning_thresholds: AdminQuotaWarningThresholds {
            primary: config.quota.warning_thresholds.primary.clone(),
            secondary: config.quota.warning_thresholds.secondary.clone(),
        },
        quota_skip_exhausted: defaults.quota_skip_exhausted,
        logs_enabled: defaults.logs_enabled,
        logs_capacity: defaults.logs_capacity,
        logs_capture_body: config.logging.capture_body,
        usage_history_retention_days: config.usage_stats.history_retention_days,
    }
}

fn apply_admin_settings_to_config(config: &mut AppConfig, settings: AdminSettings) {
    config.model.default_model = settings.default_model;
    config.model.aliases = settings.model_aliases;
    config.model.account_routes = settings.model_account_routes;
    config.auth.refresh_margin_seconds = settings.refresh_margin_seconds;
    config.auth.refresh_concurrency = settings.refresh_concurrency;
    config.auth.max_concurrent_per_account = settings.max_concurrent_per_account;
    config.auth.request_interval_ms = settings.request_interval_ms;
    config.auth.rotation_strategy = settings.rotation_strategy;
}

async fn insert_runtime_settings_if_missing(
    pool: &SqlitePool,
    settings: &AdminSettings,
) -> Result<(), RuntimeSettingsError> {
    let model_aliases_json = serde_json::to_string(&settings.model_aliases)?;
    let mut tx = pool.begin().await?;
    let result = sqlx::query(
        r"
insert or ignore into runtime_settings (
  id,
  default_model,
  model_aliases_json,
  refresh_margin_seconds,
  refresh_concurrency,
  max_concurrent_per_account,
  request_interval_ms,
  rotation_strategy,
  updated_at
) values (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(RUNTIME_SETTINGS_ID)
    .bind(&settings.default_model)
    .bind(model_aliases_json)
    .bind(
        i64::try_from(settings.refresh_margin_seconds)
            .map_err(|_| stored_field_error("refreshMarginSeconds", "out of range"))?,
    )
    .bind(i64::from(settings.refresh_concurrency))
    .bind(
        i64::try_from(settings.max_concurrent_per_account)
            .map_err(|_| stored_field_error("maxConcurrentPerAccount", "out of range"))?,
    )
    .bind(
        i64::try_from(settings.request_interval_ms)
            .map_err(|_| stored_field_error("requestIntervalMs", "out of range"))?,
    )
    .bind(&settings.rotation_strategy)
    .bind(Utc::now().to_rfc3339())
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() > 0 {
        replace_model_account_routes(&mut tx, &settings.model_account_routes).await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn save_runtime_settings(
    pool: &SqlitePool,
    settings: &AdminSettings,
) -> Result<(), RuntimeSettingsError> {
    let model_aliases_json = serde_json::to_string(&settings.model_aliases)?;
    let mut tx = pool.begin().await?;
    sqlx::query(
        r"
insert into runtime_settings (
  id,
  default_model,
  model_aliases_json,
  refresh_margin_seconds,
  refresh_concurrency,
  max_concurrent_per_account,
  request_interval_ms,
  rotation_strategy,
  updated_at
) values (?, ?, ?, ?, ?, ?, ?, ?, ?)
on conflict(id) do update set
  default_model = excluded.default_model,
  model_aliases_json = excluded.model_aliases_json,
  refresh_margin_seconds = excluded.refresh_margin_seconds,
  refresh_concurrency = excluded.refresh_concurrency,
  max_concurrent_per_account = excluded.max_concurrent_per_account,
  request_interval_ms = excluded.request_interval_ms,
  rotation_strategy = excluded.rotation_strategy,
  updated_at = excluded.updated_at",
    )
    .bind(RUNTIME_SETTINGS_ID)
    .bind(&settings.default_model)
    .bind(model_aliases_json)
    .bind(
        i64::try_from(settings.refresh_margin_seconds)
            .map_err(|_| stored_field_error("refreshMarginSeconds", "out of range"))?,
    )
    .bind(i64::from(settings.refresh_concurrency))
    .bind(
        i64::try_from(settings.max_concurrent_per_account)
            .map_err(|_| stored_field_error("maxConcurrentPerAccount", "out of range"))?,
    )
    .bind(
        i64::try_from(settings.request_interval_ms)
            .map_err(|_| stored_field_error("requestIntervalMs", "out of range"))?,
    )
    .bind(&settings.rotation_strategy)
    .bind(Utc::now().to_rfc3339())
    .execute(&mut *tx)
    .await?;
    replace_model_account_routes(&mut tx, &settings.model_account_routes).await?;
    tx.commit().await?;
    Ok(())
}

async fn load_runtime_settings(pool: &SqlitePool) -> Result<AdminSettings, RuntimeSettingsError> {
    let row = sqlx::query(
        r"
select
  default_model,
  model_aliases_json,
  refresh_margin_seconds,
  refresh_concurrency,
  max_concurrent_per_account,
  request_interval_ms,
  rotation_strategy
from runtime_settings
where id = ?",
    )
    .bind(RUNTIME_SETTINGS_ID)
    .fetch_one(pool)
    .await?;
    let mut settings = runtime_settings_from_row(&row)?;
    settings.model_account_routes = load_model_account_routes(pool).await?;
    Ok(settings)
}

fn runtime_settings_from_row(row: &SqliteRow) -> Result<AdminSettings, RuntimeSettingsError> {
    let model_aliases_json: String = row.get("model_aliases_json");
    Ok(AdminSettings {
        default_model: row.get("default_model"),
        model_aliases: serde_json::from_str(&model_aliases_json)?,
        model_account_routes: BTreeMap::new(),
        refresh_margin_seconds: positive_i64_to_u64(
            "refreshMarginSeconds",
            row.get("refresh_margin_seconds"),
        )?,
        refresh_concurrency: positive_i64_to_u32(
            "refreshConcurrency",
            row.get("refresh_concurrency"),
        )?,
        max_concurrent_per_account: positive_i64_to_usize(
            "maxConcurrentPerAccount",
            row.get("max_concurrent_per_account"),
        )?,
        request_interval_ms: non_negative_i64_to_u64(
            "requestIntervalMs",
            row.get("request_interval_ms"),
        )?,
        rotation_strategy: validate_rotation_strategy(row.get("rotation_strategy"))?,
        ..AdminSettings::default()
    })
}

async fn replace_model_account_routes(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    routes: &BTreeMap<String, Vec<String>>,
) -> Result<(), RuntimeSettingsError> {
    sqlx::query("delete from model_account_routes")
        .execute(&mut **tx)
        .await?;
    let now = Utc::now().to_rfc3339();
    for (model, account_ids) in routes {
        for (priority, account_id) in account_ids.iter().enumerate() {
            sqlx::query(
                r"
insert into model_account_routes (
  model,
  account_id,
  priority,
  enabled,
  created_at,
  updated_at
) values (?, ?, ?, 1, ?, ?)",
            )
            .bind(model)
            .bind(account_id)
            .bind(
                i64::try_from(priority)
                    .map_err(|_| stored_field_error("modelAccountRoutes", "out of range"))?,
            )
            .bind(&now)
            .bind(&now)
            .execute(&mut **tx)
            .await?;
        }
    }
    Ok(())
}

async fn load_model_account_routes(
    pool: &SqlitePool,
) -> Result<BTreeMap<String, Vec<String>>, RuntimeSettingsError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        r"
select model, account_id
from model_account_routes
where enabled = 1
order by model asc, priority asc, account_id asc",
    )
    .fetch_all(pool)
    .await?;
    let mut routes = BTreeMap::<String, Vec<String>>::new();
    for (model, account_id) in rows {
        routes.entry(model).or_default().push(account_id);
    }
    Ok(routes)
}

/// 从当前运行配置生成账号池调度参数。
pub fn account_pool_options_from_config(config: &AppConfig) -> AccountPoolOptions {
    AccountPoolOptions {
        max_concurrent_per_account: config.auth.max_concurrent_per_account,
        rotation_strategy: rotation_strategy_from_config(&config.auth.rotation_strategy),
        skip_quota_limited: config.quota.skip_exhausted,
        tier_priority: config.auth.tier_priority.clone(),
        model_account_routes: config.model.account_routes.clone(),
        ..AccountPoolOptions::default()
    }
}

fn rotation_strategy_from_config(strategy: &str) -> RotationStrategy {
    match strategy {
        "round_robin" => RotationStrategy::RoundRobin,
        "sticky" => RotationStrategy::Sticky,
        _ => RotationStrategy::LeastUsed,
    }
}

fn positive_i64_to_u64(field: &'static str, value: i64) -> Result<u64, RuntimeSettingsError> {
    if value <= 0 {
        return Err(stored_field_error(field, "must be greater than 0"));
    }
    u64::try_from(value).map_err(|_| stored_field_error(field, "out of range"))
}

fn positive_i64_to_u32(field: &'static str, value: i64) -> Result<u32, RuntimeSettingsError> {
    if value <= 0 {
        return Err(stored_field_error(field, "must be greater than 0"));
    }
    u32::try_from(value).map_err(|_| stored_field_error(field, "out of range"))
}

fn positive_i64_to_usize(field: &'static str, value: i64) -> Result<usize, RuntimeSettingsError> {
    if value <= 0 {
        return Err(stored_field_error(field, "must be greater than 0"));
    }
    usize::try_from(value).map_err(|_| stored_field_error(field, "out of range"))
}

fn non_negative_i64_to_u64(field: &'static str, value: i64) -> Result<u64, RuntimeSettingsError> {
    if value < 0 {
        return Err(stored_field_error(
            field,
            "must be greater than or equal to 0",
        ));
    }
    u64::try_from(value).map_err(|_| stored_field_error(field, "out of range"))
}

fn stored_field_error(
    field: impl Into<String>,
    message: impl Into<String>,
) -> RuntimeSettingsError {
    RuntimeSettingsError::StoredField {
        field: field.into(),
        message: message.into(),
    }
}
