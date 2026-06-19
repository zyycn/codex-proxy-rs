//! 指纹更新与持久化适配器。

use std::{path::PathBuf, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

use codex_proxy_core::gateway::fingerprint::Fingerprint;

/// 指纹历史记录的来源标识。
pub const CODEX_DESKTOP_UPDATE_SOURCE: &str = "codex_desktop_update_source";

const AUTO_UPDATED_FINGERPRINT_ID: &str = "auto_updated";
const AUTO_UPDATE_SOURCE: &str = "auto_update";
const AUTO_UPDATE_PLATFORM: &str = "darwin";
const AUTO_UPDATE_ARCH: &str = "arm64";
const AUTO_UPDATE_USER_AGENT_TEMPLATE: &str = "Codex Desktop/{version} ({platform}; {arch})";
const DEFAULT_AUTO_UPDATE_CHROMIUM_VERSION: &str = "146";
const APPCAST_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_secs(3 * 24 * 60 * 60);

type AppcastFields = (Option<String>, Option<String>, Option<String>);

/// 更新清单解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintUpdate {
    /// 应用版本。
    pub app_version: String,
    /// 构建号。
    pub build_number: String,
}

/// 指纹更新错误。
#[derive(Debug, Error)]
pub enum FingerprintError {
    /// 更新清单 JSON 无效。
    #[error("invalid update manifest: {0}")]
    InvalidManifest(#[from] serde_json::Error),
    /// HTTP 请求失败。
    #[error("failed to fetch update manifest: {0}")]
    Http(#[from] reqwest::Error),
    /// 数据库存储失败。
    #[error("failed to persist fingerprint update: {0}")]
    Database(#[from] sqlx::Error),
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

#[derive(Deserialize)]
struct Manifest {
    version: String,
    build_number: String,
}

/// 从更新清单 JSON 中提取版本信息。
pub fn parse_update_manifest(input: &str) -> Result<FingerprintUpdate, FingerprintError> {
    // 自动更新只同步桌面端指纹字段，不把远端配置当作运行时业务配置执行。
    let manifest: Manifest = serde_json::from_str(input)?;
    Ok(FingerprintUpdate {
        app_version: manifest.version,
        build_number: manifest.build_number,
    })
}

/// 指纹历史记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFingerprint {
    /// 应用版本。
    pub app_version: String,
    /// 构建号。
    pub build_number: String,
    /// 来源。
    pub source: String,
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

    /// 插入一条指纹更新历史。
    pub async fn insert_update(&self, update: &FingerprintUpdate) -> Result<(), sqlx::Error> {
        let mut fingerprint = Fingerprint::default_codex_desktop();
        fingerprint.app_version.clone_from(&update.app_version);
        fingerprint.build_number.clone_from(&update.build_number);

        sqlx::query(
            "insert into fingerprints (id, app_version, build_number, platform, arch, chromium_version, user_agent_template, source, created_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(fingerprint.app_version)
        .bind(fingerprint.build_number)
        .bind(fingerprint.platform)
        .bind(fingerprint.arch)
        .bind(fingerprint.chromium_version)
        .bind(fingerprint.user_agent_template)
        .bind(CODEX_DESKTOP_UPDATE_SOURCE)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 更新自动应用的最新版本快照。
    pub async fn upsert_auto_update(
        &self,
        app_version: &str,
        build_number: &str,
        chromium_version: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "insert into fingerprints (id, app_version, build_number, platform, arch, chromium_version, user_agent_template, source, created_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             on conflict(id) do update set app_version = excluded.app_version, build_number = excluded.build_number, platform = excluded.platform, arch = excluded.arch, chromium_version = excluded.chromium_version, user_agent_template = excluded.user_agent_template, source = excluded.source, created_at = excluded.created_at",
        )
        .bind(AUTO_UPDATED_FINGERPRINT_ID)
        .bind(app_version)
        .bind(build_number)
        .bind(AUTO_UPDATE_PLATFORM)
        .bind(AUTO_UPDATE_ARCH)
        .bind(chromium_version.unwrap_or(DEFAULT_AUTO_UPDATE_CHROMIUM_VERSION))
        .bind(AUTO_UPDATE_USER_AGENT_TEMPLATE)
        .bind(AUTO_UPDATE_SOURCE)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 读取最新一条指纹历史记录。
    pub async fn latest(&self) -> Result<Option<StoredFingerprint>, sqlx::Error> {
        let row = sqlx::query(
            "select app_version, build_number, source from fingerprints order by created_at desc, id desc limit 1",
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| StoredFingerprint {
            app_version: row.get("app_version"),
            build_number: row.get("build_number"),
            source: row.get("source"),
        }))
    }

    /// 读取自动更新应用后的最新指纹。
    pub async fn load_latest_auto_updated(&self) -> Result<Option<Fingerprint>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            select app_version, build_number, platform, arch, chromium_version, user_agent_template
            from fingerprints
            where source = ?
            order by created_at desc
            limit 1
            "#,
        )
        .bind(AUTO_UPDATE_SOURCE)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        Ok(Some(Fingerprint {
            originator: "Codex Desktop".to_string(),
            app_version: row.get("app_version"),
            build_number: row.get("build_number"),
            platform: row.get("platform"),
            arch: row.get("arch"),
            chromium_version: row.get("chromium_version"),
            user_agent_template: row.get("user_agent_template"),
            default_headers: Fingerprint::default_headers(),
            header_order: Fingerprint::default_header_order(),
        }))
    }
}

/// 通过 HTTP 拉取更新清单并写入历史记录。
#[derive(Clone)]
pub struct FingerprintUpdater {
    client: reqwest::Client,
    repository: FingerprintRepository,
    update_url: String,
}

impl FingerprintUpdater {
    /// 构造更新器。
    pub fn new(
        client: reqwest::Client,
        repository: FingerprintRepository,
        update_url: impl Into<String>,
    ) -> Self {
        Self {
            client,
            repository,
            update_url: update_url.into(),
        }
    }

    /// 执行一次更新清单拉取并持久化历史记录。
    pub async fn poll_once(&self) -> Result<FingerprintUpdate, FingerprintError> {
        let manifest = self
            .client
            .get(&self.update_url)
            .timeout(APPCAST_TIMEOUT)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let update = parse_update_manifest(&manifest)?;
        self.repository.insert_update(&update).await?;
        Ok(update)
    }
}

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
                .upsert_auto_update(version, build, chromium_version.as_deref())
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
