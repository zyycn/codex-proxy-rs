//! Codex Desktop 官方发布制品观测。

use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

use chrono::{DateTime, Utc};
use roxmltree::{Document, Node};

const APPCAST_TIMEOUT: Duration = Duration::from_secs(30);

/// Codex Desktop 官方 appcast 地址。
pub const CODEX_DESKTOP_APPCAST_URL: &str =
    "https://persistent.oaistatic.com/codex-app-prod/appcast.xml";
/// 官方发布检查周期。
pub const APPCAST_POLL_INTERVAL: Duration = Duration::from_hours(24);

/// appcast 中的最新 Desktop 完整制品信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopRelease {
    pub version: String,
    pub build: String,
    pub published_at: Option<DateTime<Utc>>,
    pub minimum_system_version: Option<String>,
    pub hardware_requirements: Option<String>,
    pub download_url: Option<String>,
    pub download_size: Option<u64>,
    pub signature_present: bool,
}

/// 最近一次 Desktop 发布检查的内存快照。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopReleaseSnapshot {
    pub checked_at: Option<DateTime<Utc>>,
    pub latest: Option<DesktopRelease>,
    pub last_error: Option<String>,
}

/// Dashboard 与后台检查器共享的 Desktop 发布观测状态。
#[derive(Debug, Clone, Default)]
pub struct DesktopReleaseStatus {
    snapshot: Arc<RwLock<DesktopReleaseSnapshot>>,
}

impl DesktopReleaseStatus {
    pub fn snapshot(&self) -> DesktopReleaseSnapshot {
        self.snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn record_success(&self, checked_at: DateTime<Utc>, latest: DesktopRelease) {
        *self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = DesktopReleaseSnapshot {
            checked_at: Some(checked_at),
            latest: Some(latest),
            last_error: None,
        };
    }

    pub fn record_failure(&self, checked_at: DateTime<Utc>, error: String) {
        let mut snapshot = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.checked_at = Some(checked_at);
        snapshot.last_error = Some(error);
    }
}

/// 读取官方 appcast 并更新发布观测状态。
#[derive(Clone)]
pub struct DesktopReleaseChecker {
    client: reqwest::Client,
    appcast_url: String,
    status: DesktopReleaseStatus,
}

impl DesktopReleaseChecker {
    pub fn with_client(
        client: reqwest::Client,
        appcast_url: impl Into<String>,
        status: DesktopReleaseStatus,
    ) -> Self {
        Self {
            client,
            appcast_url: appcast_url.into(),
            status,
        }
    }

    pub async fn check_and_record(&self) -> Result<DesktopRelease, DesktopReleaseError> {
        let checked_at = Utc::now();
        let result = self.fetch_latest().await;
        match result {
            Ok(release) => {
                self.status.record_success(checked_at, release.clone());
                Ok(release)
            }
            Err(error) => {
                self.status.record_failure(checked_at, error.to_string());
                Err(error)
            }
        }
    }

    async fn fetch_latest(&self) -> Result<DesktopRelease, DesktopReleaseError> {
        let response = self
            .client
            .get(&self.appcast_url)
            .timeout(APPCAST_TIMEOUT)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(DesktopReleaseError::AppcastFetch(
                response.status().as_u16(),
            ));
        }
        parse_latest_desktop_release(&response.text().await?)
    }
}

/// 解析 appcast 中按顺序出现的首个完整发布项。
pub fn parse_latest_desktop_release(xml: &str) -> Result<DesktopRelease, DesktopReleaseError> {
    let document = Document::parse(xml)?;
    let item = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "item")
        .ok_or(DesktopReleaseError::MissingItem)?;
    let enclosure = item
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "enclosure");

    let version = child_text(item, "shortVersionString")
        .or_else(|| local_attribute(enclosure, "shortVersionString"))
        .ok_or(DesktopReleaseError::MissingField("shortVersionString"))?;
    let build = child_text(item, "version")
        .or_else(|| local_attribute(enclosure, "version"))
        .ok_or(DesktopReleaseError::MissingField("version"))?;
    let published_at = child_text(item, "pubDate")
        .map(|value| {
            DateTime::parse_from_rfc2822(&value)
                .map(|value| value.to_utc())
                .map_err(|_| DesktopReleaseError::InvalidField("pubDate"))
        })
        .transpose()?;
    let download_size = local_attribute(enclosure, "length")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| DesktopReleaseError::InvalidField("enclosure.length"))
        })
        .transpose()?;

    Ok(DesktopRelease {
        version,
        build,
        published_at,
        minimum_system_version: child_text(item, "minimumSystemVersion"),
        hardware_requirements: child_text(item, "hardwareRequirements"),
        download_url: local_attribute(enclosure, "url"),
        download_size,
        signature_present: local_attribute(enclosure, "edSignature").is_some(),
    })
}

fn child_text(parent: Node<'_, '_>, local_name: &str) -> Option<String> {
    parent
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == local_name)
        .and_then(|node| node.text())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn local_attribute(node: Option<Node<'_, '_>>, local_name: &str) -> Option<String> {
    node?
        .attributes()
        .find(|attribute| attribute.name() == local_name)
        .map(|attribute| attribute.value().trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[derive(Debug, thiserror::Error)]
pub enum DesktopReleaseError {
    #[error("Codex Desktop appcast request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Codex Desktop appcast returned HTTP {0}")]
    AppcastFetch(u16),
    #[error("Codex Desktop appcast XML is invalid: {0}")]
    Xml(#[from] roxmltree::Error),
    #[error("Codex Desktop appcast contains no release item")]
    MissingItem,
    #[error("Codex Desktop appcast release is missing `{0}`")]
    MissingField(&'static str),
    #[error("Codex Desktop appcast field `{0}` is invalid")]
    InvalidField(&'static str),
}
