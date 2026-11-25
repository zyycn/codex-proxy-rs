//! OpenAI Desktop 发行解析、制品缓存与刷新服务

use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::{StreamExt as _, future::BoxFuture};
use gateway_core::provider_ports::{
    ProviderArtifactProfile, ProviderArtifactProfileCachePort, ProviderStoreError,
};
use gateway_core::routing::ProviderKind;
use reqwest::Client;
use reqwest::redirect::Policy;
use roxmltree::{Document, Node};
use serde::Deserialize;
use serde_json::{Map, Value};
use url::Url;

use super::desktop_artifact::{self, CodexDesktopArtifactError, fetch_codex_core_version};

use super::CodexWireProfileState;
use super::selection::{ClientKind, ClientPlatform, ClientRelease};
use gateway_core::account::OpaqueProviderData;

/// Codex Desktop 官方 appcast 地址
pub const CODEX_DESKTOP_APPCAST_URL: &str =
    "https://persistent.oaistatic.com/codex-app-prod/appcast.xml";
/// 官方发布元数据检查周期
pub const APPCAST_POLL_INTERVAL: Duration = Duration::from_secs(60 * 60);

const APPCAST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_APPCAST_BYTES: usize = 1024 * 1024;
pub(super) const ARTIFACT_PROFILE_CACHE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub(super) const ARTIFACT_PROFILE_CACHE_RENEW_INTERVAL: Duration =
    Duration::from_secs(24 * 60 * 60);
const ARTIFACT_PROFILE_SCHEMA_VERSION: u64 = 1;

/// 同一个官方 Desktop ZIP 中核验出的原子版本元组
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexBundledReleaseProfile {
    pub codex_version: String,
    pub desktop_version: String,
    pub desktop_build: String,
    pub verified_at: DateTime<Utc>,
}

impl CodexBundledReleaseProfile {
    fn artifact_sequence(&self) -> Result<u64, CodexDesktopReleaseError> {
        parse_build_sequence(&self.desktop_build)
    }

    fn validate(&self) -> Result<(), CodexDesktopReleaseError> {
        semver::Version::parse(&self.codex_version)
            .map_err(|_| CodexDesktopReleaseError::InvalidCoreVersion)?;
        if !numeric_dotted_version(&self.desktop_version) {
            return Err(CodexDesktopReleaseError::InvalidVersion);
        }
        self.artifact_sequence()?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct CodexArtifactProfileCache {
    provider_kind: ProviderKind,
    store: Arc<dyn ProviderArtifactProfileCachePort>,
}

impl CodexArtifactProfileCache {
    #[must_use]
    pub fn new(
        provider_kind: ProviderKind,
        store: Arc<dyn ProviderArtifactProfileCachePort>,
    ) -> Self {
        Self {
            provider_kind,
            store,
        }
    }

    pub async fn load(
        &self,
    ) -> Result<Option<CodexBundledReleaseProfile>, CodexDesktopReleaseError> {
        self.store
            .read(&self.provider_kind, "desktop-macos-arm64")
            .await
            .map_err(CodexDesktopReleaseError::ArtifactCache)?
            .map(decode_artifact_profile)
            .transpose()
    }

    async fn replace_if_newer(
        &self,
        profile: &CodexBundledReleaseProfile,
    ) -> Result<bool, CodexDesktopReleaseError> {
        profile.validate()?;
        self.store
            .replace_if_newer(
                encode_artifact_profile(self.provider_kind.clone(), profile)?,
                ARTIFACT_PROFILE_CACHE_TTL,
            )
            .await
            .map_err(CodexDesktopReleaseError::ArtifactCache)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedArtifactProfile {
    schema_version: u64,
    codex_version: String,
    desktop_version: String,
    desktop_build: String,
}

fn encode_artifact_profile(
    provider_kind: ProviderKind,
    profile: &CodexBundledReleaseProfile,
) -> Result<ProviderArtifactProfile, CodexDesktopReleaseError> {
    let artifact_sequence = profile.artifact_sequence()?;
    let mut fields = Map::new();
    fields.insert(
        "schema_version".to_owned(),
        Value::from(ARTIFACT_PROFILE_SCHEMA_VERSION),
    );
    fields.insert(
        "codex_version".to_owned(),
        Value::String(profile.codex_version.clone()),
    );
    fields.insert(
        "desktop_version".to_owned(),
        Value::String(profile.desktop_version.clone()),
    );
    fields.insert(
        "desktop_build".to_owned(),
        Value::String(profile.desktop_build.clone()),
    );
    Ok(ProviderArtifactProfile::new(
        provider_kind,
        "desktop-macos-arm64".to_owned(),
        artifact_sequence,
        profile.verified_at.into(),
        OpaqueProviderData::new(fields),
    ))
}

fn decode_artifact_profile(
    profile: ProviderArtifactProfile,
) -> Result<CodexBundledReleaseProfile, CodexDesktopReleaseError> {
    let wire: CachedArtifactProfile = serde_json::from_value(Value::Object(
        profile.profile().expose_to_provider().clone(),
    ))
    .map_err(|_| CodexDesktopReleaseError::InvalidArtifactCache)?;
    if wire.schema_version != ARTIFACT_PROFILE_SCHEMA_VERSION {
        return Err(CodexDesktopReleaseError::InvalidArtifactCache);
    }
    let decoded = CodexBundledReleaseProfile {
        codex_version: wire.codex_version,
        desktop_version: wire.desktop_version,
        desktop_build: wire.desktop_build,
        verified_at: DateTime::<Utc>::from(profile.verified_at()),
    };
    decoded.validate()?;
    if decoded.artifact_sequence()? != profile.artifact_sequence() {
        return Err(CodexDesktopReleaseError::InvalidArtifactCache);
    }
    Ok(decoded)
}

/// 官方 appcast 中按顺序出现的首个完整 Desktop 制品
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

/// 最近一次官方 Desktop 制品检查结果
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexDesktopReleaseSnapshot {
    pub checked_at: Option<DateTime<Utc>>,
    pub latest: Option<CodexDesktopRelease>,
    pub last_error: Option<String>,
}

/// Provider 内共享的 appcast 观察状态
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

/// Desktop appcast 获取边界
/// 生产实现固定访问官方 HTTPS，测试实现只替换此边界
pub trait CodexDesktopReleaseTransport: Send + Sync {
    fn fetch(&self) -> BoxFuture<'_, Result<CodexDesktopRelease, CodexDesktopReleaseError>>;

    fn fetch_bundled_core_version<'a>(
        &'a self,
        release: &'a CodexDesktopRelease,
    ) -> BoxFuture<'a, Result<String, CodexDesktopReleaseError>>;
}

/// 固定访问官方 Desktop appcast 的生产 transport
#[derive(Clone)]
pub struct OfficialCodexDesktopReleaseTransport {
    client: Client,
    endpoint: Url,
}

impl OfficialCodexDesktopReleaseTransport {
    /// 构造使用部署出口且禁用 redirect 的官方 HTTPS transport
    pub fn new() -> Result<Self, CodexDesktopReleaseError> {
        let endpoint = Url::parse(CODEX_DESKTOP_APPCAST_URL)
            .map_err(|_| CodexDesktopReleaseError::InvalidEndpoint)?;
        let client = gateway_egress::http_client(None)
            .map_err(|_| CodexDesktopReleaseError::ClientInitialization)?
            .https_only(true)
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

    fn fetch_bundled_core_version<'a>(
        &'a self,
        release: &'a CodexDesktopRelease,
    ) -> BoxFuture<'a, Result<String, CodexDesktopReleaseError>> {
        Box::pin(async move {
            let artifact_url = release
                .download_url
                .as_deref()
                .ok_or(CodexDesktopReleaseError::MissingArtifactIdentity)?;
            let artifact_size = release
                .download_size
                .ok_or(CodexDesktopReleaseError::MissingArtifactIdentity)?;
            fetch_codex_core_version(&self.client, artifact_url, artifact_size)
                .await
                .map_err(Into::into)
        })
    }
}

/// 拉取 appcast 并原子发布请求画像的 Provider 服务
#[derive(Clone)]
pub struct CodexDesktopReleaseService {
    transport: Arc<dyn CodexDesktopReleaseTransport>,
    status: CodexDesktopReleaseStatus,
    profile: CodexWireProfileState,
    cache: CodexArtifactProfileCache,
    verified: Arc<RwLock<Option<CodexBundledReleaseProfile>>>,
    cache_written_at: Arc<RwLock<Option<tokio::time::Instant>>>,
}

impl CodexDesktopReleaseService {
    /// 从正式 transport seam 构造服务；生产组装只注入官方 transport
    #[must_use]
    pub fn new(
        profile: CodexWireProfileState,
        transport: Arc<dyn CodexDesktopReleaseTransport>,
        cache: CodexArtifactProfileCache,
        verified: Option<CodexBundledReleaseProfile>,
    ) -> Self {
        if let Some(verified) = verified.as_ref() {
            profile.update_bundled_release(verified);
            // 缓存恢复与 CLI/平台渠道一样构成发布观察：只更新画像字段会让恢复版本
            // 在首次检查发现新版时被跳过，滞后档位落回启动种子。这里补写进
            // releases 历史，使「启动种子 → 恢复版本 → 新发现版本」形成完整序列。
            profile.seed_client_release(
                ClientKind::Desktop,
                ClientPlatform::Macos,
                "arm64",
                ClientRelease {
                    codex_version: verified.codex_version.clone(),
                    desktop_version: Some(verified.desktop_version.clone()),
                    desktop_build: Some(verified.desktop_build.clone()),
                    verified_at: Some(verified.verified_at),
                },
            );
        }
        Self {
            transport,
            status: CodexDesktopReleaseStatus::default(),
            profile,
            cache,
            verified: Arc::new(RwLock::new(verified)),
            cache_written_at: Arc::default(),
        }
    }

    #[must_use]
    pub fn status(&self) -> CodexDesktopReleaseStatus {
        self.status.clone()
    }

    /// 执行一次有界检查；失败只更新观察状态，不修改上一份成功画像
    pub async fn refresh(&self) -> Result<CodexDesktopRelease, CodexDesktopReleaseError> {
        let checked_at = Utc::now();
        let result = self.refresh_inner(checked_at).await;
        match result {
            Ok(release) => {
                self.status.record_success(checked_at, release.clone());
                if let Some(current) =
                    self.profile
                        .client_release(ClientKind::Desktop, ClientPlatform::Macos, "arm64")
                {
                    self.profile.record_client_release(
                        ClientKind::Desktop,
                        ClientPlatform::Macos,
                        "arm64",
                        Ok(current),
                    );
                }
                Ok(release)
            }
            Err(error) => {
                self.status.record_failure(checked_at, &error);
                self.profile.record_client_release(
                    ClientKind::Desktop,
                    ClientPlatform::Macos,
                    "arm64",
                    Err(error.to_string()),
                );
                Err(error)
            }
        }
    }

    async fn refresh_inner(
        &self,
        checked_at: DateTime<Utc>,
    ) -> Result<CodexDesktopRelease, CodexDesktopReleaseError> {
        let release = self.transport.fetch().await?;
        validate_release_artifact(&release)?;
        let release_sequence = parse_build_sequence(&release.build)?;
        let verified = self
            .verified
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(verified) = verified {
            let verified_sequence = verified.artifact_sequence()?;
            if release_sequence < verified_sequence {
                return Err(CodexDesktopReleaseError::ReleaseRollback);
            }
            if release_sequence == verified_sequence {
                if release.version != verified.desktop_version
                    || release.build != verified.desktop_build
                {
                    return Err(CodexDesktopReleaseError::InconsistentRelease);
                }
                // 轻量检查只推进观察时间，缓存单独按天续期
                let renewal_due = self
                    .cache_written_at
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_none_or(|written| {
                        written.elapsed() >= ARTIFACT_PROFILE_CACHE_RENEW_INTERVAL
                    });
                if renewal_due {
                    if !self.cache.replace_if_newer(&verified).await? {
                        return Err(CodexDesktopReleaseError::ReleaseRollback);
                    }
                    *self
                        .cache_written_at
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(tokio::time::Instant::now());
                }
                return Ok(release);
            }
        }

        let codex_version = self.transport.fetch_bundled_core_version(&release).await?;
        let bundled = CodexBundledReleaseProfile {
            codex_version,
            desktop_version: release.version.clone(),
            desktop_build: release.build.clone(),
            verified_at: checked_at,
        };
        bundled.validate()?;
        let published = if self.cache.replace_if_newer(&bundled).await? {
            bundled
        } else {
            self.cache
                .load()
                .await?
                .filter(|cached| {
                    cached
                        .artifact_sequence()
                        .is_ok_and(|sequence| sequence >= release_sequence)
                })
                .ok_or(CodexDesktopReleaseError::ReleaseRollback)?
        };
        self.profile.update_bundled_release(&published);
        *self
            .verified
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(published);
        *self
            .cache_written_at
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tokio::time::Instant::now());
        Ok(release)
    }
}

fn validate_release_artifact(
    release: &CodexDesktopRelease,
) -> Result<(), CodexDesktopReleaseError> {
    if !release.signature_present {
        return Err(CodexDesktopReleaseError::MissingArtifactSignature);
    }
    let artifact_url = release
        .download_url
        .as_deref()
        .ok_or(CodexDesktopReleaseError::MissingArtifactIdentity)?;
    let artifact_size = release
        .download_size
        .ok_or(CodexDesktopReleaseError::MissingArtifactIdentity)?;
    desktop_artifact::validate_codex_artifact(artifact_url, artifact_size)?;
    Ok(())
}

fn parse_build_sequence(value: &str) -> Result<u64, CodexDesktopReleaseError> {
    value
        .parse::<u64>()
        .ok()
        .filter(|sequence| *sequence > 0)
        .ok_or(CodexDesktopReleaseError::InvalidBuild)
}

/// 解析 appcast 中按顺序出现的首个完整发布项
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

pub(super) fn numeric_dotted_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let valid = parts
        .by_ref()
        .filter(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        .count();
    valid >= 2 && valid == value.split('.').count()
}

/// Desktop appcast 的稳定、无响应正文错误分类
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
    #[error("Codex Desktop appcast artifact identity is incomplete")]
    MissingArtifactIdentity,
    #[error("Codex Desktop appcast artifact signature is missing")]
    MissingArtifactSignature,
    #[error("Codex Desktop bundled Core version is invalid")]
    InvalidCoreVersion,
    #[error(transparent)]
    Artifact(#[from] CodexDesktopArtifactError),
    #[error("Codex Desktop artifact profile cache operation failed")]
    ArtifactCache(#[source] ProviderStoreError),
    #[error("Codex Desktop artifact profile cache is invalid")]
    InvalidArtifactCache,
    #[error("Codex Desktop appcast attempted to roll back the verified artifact")]
    ReleaseRollback,
    #[error("Codex Desktop appcast release identity conflicts with the verified artifact")]
    InconsistentRelease,
}
