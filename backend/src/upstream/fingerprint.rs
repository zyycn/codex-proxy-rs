//! 指纹类型、更新与持久化。

use std::{path::PathBuf, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

/// 运行时当前指纹槽位。
pub const CURRENT_FINGERPRINT_ID: &str = "current";

const AUTO_UPDATE_SOURCE: &str = "auto_update";
const CONFIG_SEED_SOURCE: &str = "config_seed";
const APPCAST_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_hours(72);

type AppcastFields = (Option<String>, Option<String>, Option<String>);

// ---------------------------------------------------------------------------
// Fingerprint 数据类型
// ---------------------------------------------------------------------------

/// 上游请求指纹。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fingerprint {
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
    pub default_headers: IndexMap<String, String>,
    /// 请求头顺序。
    pub header_order: Vec<String>,
    /// DB 最后更新时间。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl Fingerprint {
    /// 从配置构造指纹。
    pub fn from_config(config: &crate::config::schema::FingerprintConfig) -> Self {
        Self {
            originator: config.originator.clone(),
            app_version: config.app_version.clone(),
            build_number: config.build_number.clone(),
            platform: config.platform.clone(),
            arch: config.arch.clone(),
            chromium_version: config.chromium_version.clone(),
            user_agent_template: config.user_agent_template.clone(),
            default_headers: config
                .default_headers
                .iter()
                .map(|h| (h.name.clone(), h.value.clone()))
                .collect(),
            header_order: config.header_order.clone(),
            updated_at: None,
        }
    }

    /// 根据模板展开最终 User-Agent。
    pub fn user_agent(&self) -> String {
        self.user_agent_template
            .replace("{version}", &self.app_version)
            .replace("{platform}", &self.platform)
            .replace("{arch}", &self.arch)
    }

    /// 生成 `sec-ch-ua` 头值。
    pub fn sec_ch_ua(&self) -> String {
        format!(
            "\"Chromium\";v=\"{}\", \"Not:A-Brand\";v=\"24\"",
            self.chromium_version
        )
    }
}

// ---------------------------------------------------------------------------
// 指纹历史记录与仓储
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredHeader {
    name: String,
    value: String,
}

/// 指纹自动更新状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateState {
    /// 最近检查时间。
    pub last_check: DateTime<Utc>,
    /// 最新版本。
    pub latest_version: Option<String>,
    /// 最新构建号。
    pub latest_build: Option<String>,
    /// 下载地址。
    pub download_url: Option<String>,
    /// 是否有可用更新。
    pub update_available: bool,
    /// 当前版本。
    pub current_version: String,
    /// 当前构建号。
    pub current_build: String,
}

/// Appcast 检查错误。
#[derive(Debug, Error)]
pub enum UpdateError {
    /// HTTP 请求失败。
    #[error("HTTP 请求失败: {0}")]
    Http(#[from] reqwest::Error),
    /// 获取 appcast 失败。
    #[error("获取 appcast 失败，状态码: {0}")]
    AppcastFetch(u16),
    /// 解析 appcast 失败。
    #[error("解析 appcast 失败")]
    AppcastParse,
    /// JSON 序列化失败。
    #[error("JSON 序列化失败: {0}")]
    Json(#[from] serde_json::Error),
    /// 文件操作失败。
    #[error("文件操作失败: {0}")]
    Io(#[from] std::io::Error),
    /// 数据库存储失败。
    #[error("数据库操作失败: {0}")]
    Database(#[from] sqlx::Error),
}

/// SQLite 指纹仓储。
#[derive(Clone)]
pub struct FingerprintRepository {
    pool: SqlitePool,
}

impl FingerprintRepository {
    /// 使用给定连接池构造仓储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 写入当前指纹默认值；如果当前槽位已存在，直接读取数据库值。
    pub async fn ensure_current_seed(
        &self,
        default_fingerprint: &Fingerprint,
    ) -> Result<Fingerprint, sqlx::Error> {
        if let Some(fingerprint) = self.load_current().await? {
            return Ok(fingerprint);
        }

        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r"
            insert into fingerprints (
              id,
              originator,
              app_version,
              build_number,
              platform,
              arch,
              chromium_version,
              user_agent_template,
              default_headers_json,
              header_order_json,
              source,
              created_at,
              updated_at
            ) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(CURRENT_FINGERPRINT_ID)
        .bind(&default_fingerprint.originator)
        .bind(&default_fingerprint.app_version)
        .bind(&default_fingerprint.build_number)
        .bind(&default_fingerprint.platform)
        .bind(&default_fingerprint.arch)
        .bind(&default_fingerprint.chromium_version)
        .bind(&default_fingerprint.user_agent_template)
        .bind(encode_default_headers(
            &default_fingerprint.default_headers,
        )?)
        .bind(encode_header_order(&default_fingerprint.header_order)?)
        .bind(CONFIG_SEED_SOURCE)
        .bind(&now)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(default_fingerprint.clone())
    }

    /// 更新当前指纹中的自动更新版本字段。
    pub async fn update_current_version(
        &self,
        app_version: &str,
        build_number: &str,
        chromium_version: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let resolved_chromium_version = match chromium_version {
            Some(version) => version.to_string(),
            None => {
                sqlx::query_scalar::<_, String>(
                    "select chromium_version from fingerprints where id = ?",
                )
                .bind(CURRENT_FINGERPRINT_ID)
                .fetch_one(&self.pool)
                .await?
            }
        };

        let result = sqlx::query(
            "update fingerprints set app_version = ?, build_number = ?, chromium_version = ?, source = ?, updated_at = ? where id = ?",
        )
        .bind(app_version)
        .bind(build_number)
        .bind(resolved_chromium_version)
        .bind(AUTO_UPDATE_SOURCE)
        .bind(Utc::now().to_rfc3339())
        .bind(CURRENT_FINGERPRINT_ID)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(sqlx::Error::RowNotFound);
        }
        Ok(())
    }

    /// 读取当前运行时指纹。
    pub async fn load_current(&self) -> Result<Option<Fingerprint>, sqlx::Error> {
        let row = sqlx::query(
            r"
            select
              originator,
              app_version,
              build_number,
              platform,
              arch,
              chromium_version,
              user_agent_template,
              default_headers_json,
              header_order_json,
              updated_at
            from fingerprints
            where id = ?
            ",
        )
        .bind(CURRENT_FINGERPRINT_ID)
        .fetch_optional(&self.pool)
        .await?;

        row.map(|row| fingerprint_from_row(&row)).transpose()
    }

    /// 记录自动更新历史。
    pub async fn insert_update_history(
        &self,
        app_version: &str,
        build_number: &str,
        chromium_version: Option<&str>,
        manifest_json: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "insert into fingerprint_update_history (id, current_fingerprint_id, app_version, build_number, chromium_version, source, manifest_json, created_at) values (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(CURRENT_FINGERPRINT_ID)
        .bind(app_version)
        .bind(build_number)
        .bind(chromium_version)
        .bind(AUTO_UPDATE_SOURCE)
        .bind(manifest_json)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn fingerprint_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Fingerprint, sqlx::Error> {
    let default_headers_json = row.get::<String, _>("default_headers_json");
    let header_order_json = row.get::<String, _>("header_order_json");
    Ok(Fingerprint {
        originator: row.get("originator"),
        app_version: row.get("app_version"),
        build_number: row.get("build_number"),
        platform: row.get("platform"),
        arch: row.get("arch"),
        chromium_version: row.get("chromium_version"),
        user_agent_template: row.get("user_agent_template"),
        default_headers: decode_default_headers(&default_headers_json)?,
        header_order: decode_header_order(&header_order_json)?,
        updated_at: row.get("updated_at"),
    })
}

fn encode_default_headers(headers: &IndexMap<String, String>) -> Result<String, sqlx::Error> {
    let headers = headers
        .iter()
        .map(|(name, value)| StoredHeader {
            name: name.clone(),
            value: value.clone(),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&headers).map_err(json_error)
}

fn decode_default_headers(value: &str) -> Result<IndexMap<String, String>, sqlx::Error> {
    let headers = serde_json::from_str::<Vec<StoredHeader>>(value).map_err(json_error)?;
    Ok(headers
        .into_iter()
        .map(|header| (header.name, header.value))
        .collect())
}

fn encode_header_order(header_order: &[String]) -> Result<String, sqlx::Error> {
    serde_json::to_string(header_order).map_err(json_error)
}

fn decode_header_order(value: &str) -> Result<Vec<String>, sqlx::Error> {
    serde_json::from_str(value).map_err(json_error)
}

fn json_error(error: serde_json::Error) -> sqlx::Error {
    sqlx::Error::Decode(Box::new(error))
}

// ---------------------------------------------------------------------------
// Appcast 指纹更新检查器
// ---------------------------------------------------------------------------

/// Appcast 指纹更新检查器。
#[derive(Clone)]
pub struct UpdateChecker {
    repository: Option<FingerprintRepository>,
    client: reqwest::Client,
    appcast_url: String,
    extracted_fingerprint_path: PathBuf,
    state: Arc<Mutex<InternalState>>,
}

struct InternalState {
    current_version: String,
    current_build: String,
}

impl UpdateChecker {
    /// 使用自定义 client 和路径构造检查器。
    pub fn with_client(
        repository: Option<FingerprintRepository>,
        client: reqwest::Client,
        appcast_url: impl Into<String>,
        extracted_fingerprint_path: PathBuf,
        current_version: impl Into<String>,
        current_build: impl Into<String>,
    ) -> Self {
        Self {
            repository,
            client,
            appcast_url: appcast_url.into(),
            extracted_fingerprint_path,
            state: Arc::new(Mutex::new(InternalState {
                current_version: current_version.into(),
                current_build: current_build.into(),
            })),
        }
    }

    /// 检查 appcast 是否存在更新。
    pub async fn check_for_update(&self) -> Result<UpdateState, UpdateError> {
        let state = self.state.lock().await;
        let current_version = state.current_version.clone();
        let current_build = state.current_build.clone();
        drop(state);

        let xml = self.fetch_appcast().await?;
        let (version, build, download_url) = parse_appcast(&xml)?;

        let update_available = version
            .as_ref()
            .is_some_and(|value| value != &current_version)
            || build.as_ref().is_some_and(|value| value != &current_build);

        Ok(UpdateState {
            last_check: Utc::now(),
            latest_version: version,
            latest_build: build,
            download_url,
            update_available,
            current_version,
            current_build,
        })
    }

    /// 检查并在需要时应用更新。
    pub async fn check_and_apply_update(&self) -> Result<bool, UpdateError> {
        let update_state = self.check_for_update().await?;

        if !update_state.update_available {
            return Ok(false);
        }

        let (Some(version), Some(build)) = (
            update_state.latest_version.as_deref(),
            update_state.latest_build.as_deref(),
        ) else {
            return Ok(false);
        };

        let state = self.state.lock().await;
        let current_version = state.current_version.clone();
        let current_build = state.current_build.clone();
        drop(state);

        info!(
            version = %version,
            build = %build,
            current_version = %current_version,
            current_build = %current_build,
            "发现新的 fingerprint 版本"
        );

        self.apply_version_update(version, build).await?;

        let mut state = self.state.lock().await;
        state.current_version = version.to_string();
        state.current_build = build.to_string();

        Ok(true)
    }

    /// 启动后台轮询任务。
    pub fn start_background_checker(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(POLL_INTERVAL);

            info!(
                interval_secs = POLL_INTERVAL.as_secs(),
                "fingerprint 后台版本检查器已启动"
            );

            if let Err(error) = self.check_and_apply_update().await {
                warn!(error = %error, "fingerprint 首次版本检查失败");
            }

            loop {
                ticker.tick().await;
                if let Err(error) = self.check_and_apply_update().await {
                    warn!(error = %error, "fingerprint 定期版本检查失败");
                }
            }
        })
    }

    async fn fetch_appcast(&self) -> Result<String, UpdateError> {
        let response = self
            .client
            .get(&self.appcast_url)
            .timeout(APPCAST_TIMEOUT)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(UpdateError::AppcastFetch(response.status().as_u16()));
        }

        response.text().await.map_err(UpdateError::from)
    }

    async fn apply_version_update(&self, version: &str, build: &str) -> Result<(), UpdateError> {
        let chromium_version =
            load_matching_chromium_version(&self.extracted_fingerprint_path, version, build);

        if let Some(repository) = &self.repository {
            repository
                .update_current_version(version, build, chromium_version.as_deref())
                .await?;
            repository
                .insert_update_history(version, build, chromium_version.as_deref(), None)
                .await?;
        }

        Ok(())
    }
}

fn parse_appcast(xml: &str) -> Result<AppcastFields, UpdateError> {
    let item_start = xml.find("<item>").ok_or(UpdateError::AppcastParse)?;
    let item_end = xml.find("</item>").ok_or(UpdateError::AppcastParse)?;
    let item = &xml[item_start..item_end];

    let version = extract_sparkle_field(item, "shortVersionString");
    let build = extract_sparkle_field(item, "version");
    let download_url = item.find("url=\"").and_then(|position| {
        let start = position + 5;
        item[start..]
            .find('"')
            .map(|end| item[start..start + end].to_string())
    });

    Ok((version, build, download_url))
}

fn extract_sparkle_field(item: &str, field: &str) -> Option<String> {
    let attr_pattern = format!("sparkle:{}=\"", field);
    if let Some(position) = item.find(&attr_pattern) {
        let start = position + attr_pattern.len();
        if let Some(end) = item[start..].find('"') {
            return Some(item[start..start + end].to_string());
        }
    }

    let elem_start = format!("<sparkle:{}>", field);
    let elem_end = format!("</sparkle:{}>", field);
    if let Some(start_position) = item.find(&elem_start) {
        let content_start = start_position + elem_start.len();
        if let Some(end_position) = item[content_start..].find(&elem_end) {
            return Some(item[content_start..content_start + end_position].to_string());
        }
    }

    None
}

fn load_matching_chromium_version(
    extracted_fingerprint_path: &PathBuf,
    version: &str,
    build: &str,
) -> Option<String> {
    let content = std::fs::read_to_string(extracted_fingerprint_path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&content).ok()?;

    let extracted_version = parsed.get("app_version")?.as_str()?;
    let extracted_build = parsed.get("build_number")?.as_str()?;

    if extracted_version == version && extracted_build == build {
        parsed
            .get("chromium_version")?
            .as_str()
            .map(ToString::to_string)
    } else {
        None
    }
}
