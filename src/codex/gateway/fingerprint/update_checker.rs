use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{info, warn};

use crate::platform::storage::paths;

const APPCAST_URL: &str = "https://persistent.oaistatic.com/codex-app-prod/appcast.xml";
const POLL_INTERVAL: Duration = Duration::from_secs(3 * 24 * 60 * 60); // 3 天
type AppcastFields = (Option<String>, Option<String>, Option<String>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateState {
    pub last_check: DateTime<Utc>,
    pub latest_version: Option<String>,
    pub latest_build: Option<String>,
    pub download_url: Option<String>,
    pub update_available: bool,
    pub current_version: String,
    pub current_build: String,
}

#[derive(Clone)]
pub struct UpdateChecker {
    db: Option<SqlitePool>,
    state: Arc<Mutex<InternalState>>,
}

struct InternalState {
    current_version: String,
    current_build: String,
}

impl UpdateChecker {
    pub fn new(db: Option<SqlitePool>, current_version: String, current_build: String) -> Self {
        Self {
            db,
            state: Arc::new(Mutex::new(InternalState {
                current_version,
                current_build,
            })),
        }
    }

    pub async fn check_for_update(&self) -> Result<UpdateState, UpdateError> {
        let state = self.state.lock().await;
        let current_version = state.current_version.clone();
        let current_build = state.current_build.clone();
        drop(state);

        let xml = fetch_appcast().await?;
        let (version, build, download_url) = parse_appcast(&xml)?;

        let update_available = version.as_ref().is_some_and(|v| v != &current_version)
            || build.as_ref().is_some_and(|b| b != &current_build);

        let update_state = UpdateState {
            last_check: Utc::now(),
            latest_version: version,
            latest_build: build,
            download_url,
            update_available,
            current_version,
            current_build,
        };

        persist_update_state(&update_state)?;

        Ok(update_state)
    }

    pub fn start_background_checker(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = interval(POLL_INTERVAL);

            info!(
                interval_secs = POLL_INTERVAL.as_secs(),
                "UpdateChecker 后台 fingerprint 版本检查器已启动"
            );

            // 立即执行首次检查
            if let Err(e) = self.check_and_apply_update().await {
                warn!(error = %e, "UpdateChecker 首次检查失败");
            }

            loop {
                ticker.tick().await;
                if let Err(e) = self.check_and_apply_update().await {
                    warn!(error = %e, "UpdateChecker 定期检查失败");
                }
            }
        })
    }

    async fn check_and_apply_update(&self) -> Result<(), UpdateError> {
        let update_state = self.check_for_update().await?;

        if update_state.update_available {
            let (Some(version), Some(build)) = (
                update_state.latest_version.as_ref(),
                update_state.latest_build.as_ref(),
            ) else {
                return Ok(());
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
                "UpdateChecker 发现新的 fingerprint 版本"
            );

            self.apply_version_update(version, build).await?;

            let mut state = self.state.lock().await;
            state.current_version = version.clone();
            state.current_build = build.clone();

            info!(
                version = %version,
                build = %build,
                "UpdateChecker 已自动应用 fingerprint 版本"
            );
        }

        Ok(())
    }

    async fn apply_version_update(&self, version: &str, build: &str) -> Result<(), UpdateError> {
        let chromium_version = load_matching_chromium_version(version, build);

        if let Some(db) = &self.db {
            update_fingerprint_in_db(db, version, build, chromium_version.as_deref()).await?;
        }

        Ok(())
    }
}

async fn fetch_appcast() -> Result<String, UpdateError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let response = client.get(APPCAST_URL).send().await?;

    if !response.status().is_success() {
        return Err(UpdateError::AppcastFetch(response.status().as_u16()));
    }

    let xml = response.text().await?;
    Ok(xml)
}

fn parse_appcast(xml: &str) -> Result<AppcastFields, UpdateError> {
    let item_start = xml.find("<item>").ok_or(UpdateError::AppcastParse)?;
    let item_end = xml.find("</item>").ok_or(UpdateError::AppcastParse)?;
    let item = &xml[item_start..item_end];

    let version = extract_sparkle_field(item, "shortVersionString");
    let build = extract_sparkle_field(item, "version");
    let download_url = item.find("url=\"").and_then(|pos| {
        let start = pos + 5;
        item[start..]
            .find('"')
            .map(|end| item[start..start + end].to_string())
    });

    Ok((version, build, download_url))
}

fn extract_sparkle_field(item: &str, field: &str) -> Option<String> {
    // 支持属性语法: sparkle:version="X"
    let attr_pattern = format!("sparkle:{}=\"", field);
    if let Some(pos) = item.find(&attr_pattern) {
        let start = pos + attr_pattern.len();
        if let Some(end) = item[start..].find('"') {
            return Some(item[start..start + end].to_string());
        }
    }

    // 支持元素语法: <sparkle:version>X</sparkle:version>
    let elem_start = format!("<sparkle:{}>", field);
    let elem_end = format!("</sparkle:{}>", field);
    if let Some(start_pos) = item.find(&elem_start) {
        let content_start = start_pos + elem_start.len();
        if let Some(end_pos) = item[content_start..].find(&elem_end) {
            return Some(item[content_start..content_start + end_pos].to_string());
        }
    }

    None
}

fn persist_update_state(state: &UpdateState) -> Result<(), UpdateError> {
    let data_dir = paths::ensure_data_dir()?;
    let state_path = data_dir.join("update-state.json");
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(state_path, json)?;
    Ok(())
}

fn load_matching_chromium_version(version: &str, build: &str) -> Option<String> {
    let extracted_path = paths::data_dir().join("extracted-fingerprint.json");
    let content = std::fs::read_to_string(extracted_path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&content).ok()?;

    let extracted_version = parsed.get("app_version")?.as_str()?;
    let extracted_build = parsed.get("build_number")?.as_str()?;

    if extracted_version == version && extracted_build == build {
        parsed.get("chromium_version")?.as_str().map(String::from)
    } else {
        None
    }
}

async fn update_fingerprint_in_db(
    db: &SqlitePool,
    version: &str,
    build: &str,
    chromium_version: Option<&str>,
) -> Result<(), UpdateError> {
    sqlx::query(
        r#"
        insert into fingerprints (
            id, app_version, build_number, platform, arch,
            chromium_version, user_agent_template, source, created_at
        ) values (?, ?, ?, 'darwin', 'arm64', ?, ?, 'auto_update', ?)
        on conflict(id) do update set
            app_version = excluded.app_version,
            build_number = excluded.build_number,
            chromium_version = excluded.chromium_version
        "#,
    )
    .bind("auto_updated")
    .bind(version)
    .bind(build)
    .bind(chromium_version.unwrap_or("146"))
    .bind("Codex Desktop/{app_version} ({platform}; {arch})")
    .bind(Utc::now().to_rfc3339())
    .execute(db)
    .await?;

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("HTTP 请求失败: {0}")]
    Http(#[from] reqwest::Error),
    #[error("获取 appcast 失败，状态码: {0}")]
    AppcastFetch(u16),
    #[error("解析 appcast 失败")]
    AppcastParse,
    #[error("JSON 序列化失败: {0}")]
    Json(#[from] serde_json::Error),
    #[error("文件操作失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("数据库操作失败: {0}")]
    Database(#[from] sqlx::Error),
}
