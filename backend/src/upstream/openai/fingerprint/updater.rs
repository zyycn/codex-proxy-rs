//! OpenAI 客户端指纹更新检查器。

use std::{path::PathBuf, sync::Arc, time::Duration};

use chrono::Utc;
use tokio::sync::Mutex;
use tracing::info;

use super::{
    store::PgFingerprintStore,
    types::{Fingerprint, UpdateError, UpdateState},
};

const APPCAST_TIMEOUT: Duration = Duration::from_secs(30);
pub const APPCAST_POLL_INTERVAL: Duration = Duration::from_hours(72);
type AppcastFields = (Option<String>, Option<String>, Option<String>);

/// Appcast 指纹更新检查器。
#[derive(Clone)]
pub struct UpdateChecker {
    store: PgFingerprintStore,
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
        store: PgFingerprintStore,
        client: reqwest::Client,
        appcast_url: impl Into<String>,
        extracted_fingerprint_path: PathBuf,
        current_version: impl Into<String>,
        current_build: impl Into<String>,
    ) -> Self {
        Self {
            store,
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
    pub async fn check_and_apply_update(&self) -> Result<Option<Fingerprint>, UpdateError> {
        let update_state = self.check_for_update().await?;

        if !update_state.update_available {
            return Ok(None);
        }

        let (Some(version), Some(build)) = (
            update_state.latest_version.as_deref(),
            update_state.latest_build.as_deref(),
        ) else {
            return Ok(None);
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

        let updated_fingerprint = self.apply_version_update(version, build).await?;

        let mut state = self.state.lock().await;
        state.current_version = version.to_string();
        state.current_build = build.to_string();

        Ok(Some(updated_fingerprint))
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

    async fn apply_version_update(
        &self,
        version: &str,
        build: &str,
    ) -> Result<Fingerprint, UpdateError> {
        let chromium_version =
            load_matching_chromium_version(&self.extracted_fingerprint_path, version, build);

        self.store
            .update_current_version(version, build, chromium_version.as_deref())
            .await?;
        self.store
            .insert_update_history(version, build, chromium_version.as_deref(), None)
            .await?;
        let updated = self
            .store
            .load_current()
            .await?
            .ok_or(sqlx::Error::RowNotFound)?;
        Ok(updated)
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
