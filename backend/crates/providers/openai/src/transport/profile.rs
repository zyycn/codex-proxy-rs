//! Codex Desktop 上游请求画像。

use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::{StreamExt as _, future::BoxFuture};
use reqwest::Client;
use reqwest::redirect::Policy;
use roxmltree::{Document, Node};
use url::Url;

/// Codex Desktop 官方 appcast 地址。
pub const CODEX_DESKTOP_APPCAST_URL: &str =
    "https://persistent.oaistatic.com/codex-app-prod/appcast.xml";
/// 官方制品画像检查周期。
pub const APPCAST_POLL_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

const APPCAST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_APPCAST_BYTES: usize = 1024 * 1024;

/// Codex Desktop 上游请求身份。
///
/// 启动配置提供经源码审计的 Core、运行环境和初始 Desktop 版本。官方 appcast
/// 检查成功后，会仅同步 Desktop 版本和构建号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWireProfile {
    /// `originator` 请求头及 User-Agent 产品名。
    pub originator: String,
    /// bundled Codex Core 版本；同时用于 `/codex/models?client_version=`。
    pub codex_version: String,
    /// Desktop 应用版本，用于 app-server `clientInfo.version` 对应的 UA 后缀。
    pub desktop_version: String,
    /// Desktop 制品构建号，仅用于发布对齐诊断。
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

impl CodexWireProfile {
    /// 按 Codex Core 的官方格式生成最终 User-Agent。
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
}

/// 跨 Codex 上游请求共享的运行时请求画像。
#[derive(Debug, Clone)]
pub struct CodexWireProfileState {
    profile: Arc<RwLock<CodexWireProfile>>,
}

impl CodexWireProfileState {
    /// 从启动画像创建运行时状态。
    pub fn new(profile: CodexWireProfile) -> Self {
        Self {
            profile: Arc::new(RwLock::new(profile)),
        }
    }

    /// 返回当前画像的独立快照，避免持锁执行网络请求。
    pub fn snapshot(&self) -> CodexWireProfile {
        self.profile
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// 使用官方 appcast 已验证的 Desktop 版本和构建号更新运行时画像。
    pub fn update_desktop_release(&self, desktop_version: &str, desktop_build: &str) {
        let mut profile = self
            .profile
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if profile.desktop_version == desktop_version && profile.desktop_build == desktop_build {
            return;
        }
        profile.desktop_version = desktop_version.to_owned();
        profile.desktop_build = desktop_build.to_owned();
    }
}

/// 官方 appcast 中按顺序出现的首个完整 Desktop 制品。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexDesktopRelease {
    pub version: String,
    pub build: String,
    pub published_at: Option<DateTime<Utc>>,
    pub minimum_system_version: Option<String>,
    pub hardware_requirements: Option<String>,
    pub download_url: Option<String>,
    pub download_size: Option<u64>,
    pub signature_present: bool,
}

/// 最近一次官方 Desktop 制品检查结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexDesktopReleaseSnapshot {
    pub checked_at: Option<DateTime<Utc>>,
    pub latest: Option<CodexDesktopRelease>,
    pub last_error: Option<String>,
}

/// Provider 内共享的 appcast 观察状态。
#[derive(Debug, Clone, Default)]
pub struct CodexDesktopReleaseStatus {
    snapshot: Arc<RwLock<CodexDesktopReleaseSnapshot>>,
}

impl CodexDesktopReleaseStatus {
    #[must_use]
    pub fn snapshot(&self) -> CodexDesktopReleaseSnapshot {
        self.snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn record_success(&self, checked_at: DateTime<Utc>, latest: CodexDesktopRelease) {
        *self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = CodexDesktopReleaseSnapshot {
            checked_at: Some(checked_at),
            latest: Some(latest),
            last_error: None,
        };
    }

    fn record_failure(&self, checked_at: DateTime<Utc>, error: &CodexDesktopReleaseError) {
        let mut snapshot = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.checked_at = Some(checked_at);
        snapshot.last_error = Some(error.to_string());
    }
}

/// Desktop appcast 获取边界。生产实现固定访问官方 HTTPS，测试实现只替换此边界。
pub trait CodexDesktopReleaseTransport: Send + Sync {
    fn fetch(&self) -> BoxFuture<'_, Result<CodexDesktopRelease, CodexDesktopReleaseError>>;
}

/// 固定访问官方 Desktop appcast 的生产 transport。
#[derive(Clone)]
pub struct OfficialCodexDesktopReleaseTransport {
    client: Client,
    endpoint: Url,
}

impl OfficialCodexDesktopReleaseTransport {
    /// 构造禁用环境代理和 redirect 的官方 HTTPS transport。
    pub fn new() -> Result<Self, CodexDesktopReleaseError> {
        let endpoint = Url::parse(CODEX_DESKTOP_APPCAST_URL)
            .map_err(|_| CodexDesktopReleaseError::InvalidEndpoint)?;
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(APPCAST_TIMEOUT)
            .build()
            .map_err(|_| CodexDesktopReleaseError::ClientInitialization)?;
        Ok(Self { client, endpoint })
    }
}

impl CodexDesktopReleaseTransport for OfficialCodexDesktopReleaseTransport {
    fn fetch(&self) -> BoxFuture<'_, Result<CodexDesktopRelease, CodexDesktopReleaseError>> {
        Box::pin(async move {
            let response = self.client.get(self.endpoint.clone()).send().await?;
            if !response.status().is_success() {
                return Err(CodexDesktopReleaseError::HttpStatus(
                    response.status().as_u16(),
                ));
            }
            if response
                .content_length()
                .is_some_and(|length| length > MAX_APPCAST_BYTES as u64)
            {
                return Err(CodexDesktopReleaseError::ResponseTooLarge);
            }
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                if bytes
                    .len()
                    .checked_add(chunk.len())
                    .is_none_or(|length| length > MAX_APPCAST_BYTES)
                {
                    return Err(CodexDesktopReleaseError::ResponseTooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            let xml = std::str::from_utf8(&bytes)
                .map_err(|_| CodexDesktopReleaseError::InvalidDocument)?;
            parse_desktop_release(xml)
        })
    }
}

/// 拉取 appcast 并原子发布请求画像的 Provider 服务。
#[derive(Clone)]
pub struct CodexDesktopReleaseService {
    transport: Arc<dyn CodexDesktopReleaseTransport>,
    status: CodexDesktopReleaseStatus,
    profile: CodexWireProfileState,
}

impl CodexDesktopReleaseService {
    /// 从正式 transport seam 构造服务；生产组装只注入官方 transport。
    #[must_use]
    pub fn new(
        profile: CodexWireProfileState,
        transport: Arc<dyn CodexDesktopReleaseTransport>,
    ) -> Self {
        Self {
            transport,
            status: CodexDesktopReleaseStatus::default(),
            profile,
        }
    }

    #[must_use]
    pub fn status(&self) -> CodexDesktopReleaseStatus {
        self.status.clone()
    }

    /// 执行一次有界检查；失败只更新观察状态，不修改上一份成功画像。
    pub async fn refresh(&self) -> Result<CodexDesktopRelease, CodexDesktopReleaseError> {
        let checked_at = Utc::now();
        let result = self.transport.fetch().await;
        match result {
            Ok(release) => {
                self.profile
                    .update_desktop_release(&release.version, &release.build);
                self.status.record_success(checked_at, release.clone());
                Ok(release)
            }
            Err(error) => {
                self.status.record_failure(checked_at, &error);
                Err(error)
            }
        }
    }
}

/// 解析 appcast 中按顺序出现的首个完整发布项。
pub fn parse_desktop_release(xml: &str) -> Result<CodexDesktopRelease, CodexDesktopReleaseError> {
    let document = Document::parse(xml).map_err(|_| CodexDesktopReleaseError::InvalidDocument)?;
    for item in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
    {
        if let Some(release) = parse_release_item(item) {
            return release;
        }
    }
    Err(CodexDesktopReleaseError::MissingItem)
}

fn parse_release_item(
    item: Node<'_, '_>,
) -> Option<Result<CodexDesktopRelease, CodexDesktopReleaseError>> {
    let enclosure = item
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == "enclosure");
    let version = child_text(item, "shortVersionString")
        .or_else(|| local_attribute(enclosure, "shortVersionString"))?;
    if !numeric_dotted_version(&version) {
        return Some(Err(CodexDesktopReleaseError::InvalidVersion));
    }
    let build = child_text(item, "version").or_else(|| local_attribute(enclosure, "version"))?;
    if build.is_empty() || !build.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(Err(CodexDesktopReleaseError::InvalidBuild));
    }
    let published_at = child_text(item, "pubDate")
        .map(|value| {
            DateTime::parse_from_rfc2822(&value)
                .map(|value| value.to_utc())
                .map_err(|_| CodexDesktopReleaseError::InvalidPublishedAt)
        })
        .transpose();
    let published_at = match published_at {
        Ok(value) => value,
        Err(error) => return Some(Err(error)),
    };
    let download_size = local_attribute(enclosure, "length")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| CodexDesktopReleaseError::InvalidDownloadSize)
        })
        .transpose();
    let download_size = match download_size {
        Ok(value) => value,
        Err(error) => return Some(Err(error)),
    };
    Some(Ok(CodexDesktopRelease {
        version,
        build,
        published_at,
        minimum_system_version: child_text(item, "minimumSystemVersion"),
        hardware_requirements: child_text(item, "hardwareRequirements"),
        download_url: local_attribute(enclosure, "url"),
        download_size,
        signature_present: local_attribute(enclosure, "edSignature").is_some(),
    }))
}

fn child_text(parent: Node<'_, '_>, local_name: &str) -> Option<String> {
    parent
        .children()
        .find(|node| node.is_element() && node.tag_name().name() == local_name)
        .and_then(|node| node.text())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn local_attribute(node: Option<Node<'_, '_>>, local_name: &str) -> Option<String> {
    node?
        .attributes()
        .find(|attribute| attribute.name() == local_name)
        .map(|attribute| attribute.value().trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn numeric_dotted_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let valid = parts
        .by_ref()
        .filter(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .count();
    valid >= 2 && valid == value.split('.').count()
}

/// Desktop appcast 的稳定、无响应正文错误分类。
#[derive(Debug, thiserror::Error)]
pub enum CodexDesktopReleaseError {
    #[error("Codex Desktop appcast client initialization failed")]
    ClientInitialization,
    #[error("Codex Desktop appcast endpoint is invalid")]
    InvalidEndpoint,
    #[error("Codex Desktop appcast request failed")]
    Http(#[from] reqwest::Error),
    #[error("Codex Desktop appcast returned HTTP {0}")]
    HttpStatus(u16),
    #[error("Codex Desktop appcast response exceeded the size limit")]
    ResponseTooLarge,
    #[error("Codex Desktop appcast document is invalid")]
    InvalidDocument,
    #[error("Codex Desktop appcast contains no complete release item")]
    MissingItem,
    #[error("Codex Desktop appcast version is invalid")]
    InvalidVersion,
    #[error("Codex Desktop appcast build is invalid")]
    InvalidBuild,
    #[error("Codex Desktop appcast publish time is invalid")]
    InvalidPublishedAt,
    #[error("Codex Desktop appcast download size is invalid")]
    InvalidDownloadSize,
}
