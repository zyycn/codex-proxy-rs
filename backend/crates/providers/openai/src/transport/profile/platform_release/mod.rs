//! Windows / Linux Desktop 按平台和架构独立核验、缓存配套版本。

use super::selection::{
    ClientKind, ClientPlatform, ClientProfileSelection, ClientRelease, VersionMode, object,
};
use super::{ARTIFACT_PROFILE_CACHE_TTL, CodexWireProfileState};
use chrono::Utc;
use futures::future::BoxFuture;
use gateway_core::provider_ports::{ProviderArtifactProfile, ProviderArtifactProfileCachePort};
use gateway_core::routing::ProviderKind;
use range::{ArtifactIdentity, RemoteFile, invalid};
use reqwest::{Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub mod artifact;
pub mod range;
pub mod xz;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DesktopTarget {
    pub platform: ClientPlatform,
    pub arch: &'static str,
}

pub const TARGETS: [DesktopTarget; 4] = [
    DesktopTarget {
        platform: ClientPlatform::Windows,
        arch: "x86_64",
    },
    DesktopTarget {
        platform: ClientPlatform::Windows,
        arch: "arm64",
    },
    DesktopTarget {
        platform: ClientPlatform::Linux,
        arch: "x86_64",
    },
    DesktopTarget {
        platform: ClientPlatform::Linux,
        arch: "arm64",
    },
];

impl DesktopTarget {
    fn key(self) -> String {
        format!(
            "desktop-{}-{}",
            if self.platform == ClientPlatform::Windows {
                "windows"
            } else {
                "linux"
            },
            self.arch
        )
    }
    fn url(self) -> io::Result<String> {
        let path = match (self.platform, self.arch) {
            (ClientPlatform::Windows, "x86_64") => "ChatGPT-x64.msix",
            (ClientPlatform::Windows, "arm64") => "ChatGPT-arm64.msix",
            (ClientPlatform::Linux, "x86_64") => "linux/deb/latest/chatgpt_amd64.deb",
            (ClientPlatform::Linux, "arm64") => "linux/deb/latest/chatgpt_arm64.deb",
            _ => return Err(invalid()),
        };
        Ok(format!(
            "https://persistent.oaistatic.com/codex-app-prod/{path}"
        ))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedArtifact {
    pub release: ClientRelease,
    identity: ArtifactIdentity,
}

pub trait DesktopArtifactTransport: Send + Sync {
    fn fetch(
        &self,
        target: DesktopTarget,
        previous: Option<VerifiedArtifact>,
    ) -> BoxFuture<'_, io::Result<VerifiedArtifact>>;
}

pub struct OfficialDesktopArtifactTransport {
    client: Client,
}
impl OfficialDesktopArtifactTransport {
    pub fn new() -> io::Result<Self> {
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| invalid())?;
        Ok(Self { client })
    }
}
impl DesktopArtifactTransport for OfficialDesktopArtifactTransport {
    fn fetch(
        &self,
        target: DesktopTarget,
        previous: Option<VerifiedArtifact>,
    ) -> BoxFuture<'_, io::Result<VerifiedArtifact>> {
        Box::pin(async move {
            let cancel = CancellationToken::new();
            let _cancel_on_drop = cancel.clone().drop_guard();
            let file = RemoteFile::open(self.client.clone(), target.url()?, cancel).await?;
            if let Some(previous) = previous.filter(|previous| previous.identity == file.identity) {
                return Ok(previous);
            }
            let identity = file.identity.clone();
            let release = tokio::task::spawn_blocking(move || match target.platform {
                ClientPlatform::Windows => artifact::read_windows(file, target.arch),
                ClientPlatform::Linux => artifact::read_linux(file, target.arch),
                ClientPlatform::Macos => Err(invalid()),
            })
            .await
            .map_err(|_| invalid())??;
            Ok(VerifiedArtifact { release, identity })
        })
    }
}

pub struct PlatformDesktopReleaseService {
    provider: ProviderKind,
    state: CodexWireProfileState,
    cache: Arc<dyn ProviderArtifactProfileCachePort>,
    transport: Arc<dyn DesktopArtifactTransport>,
    verified: Mutex<BTreeMap<DesktopTarget, VerifiedArtifact>>,
}
impl PlatformDesktopReleaseService {
    pub fn new(
        provider: ProviderKind,
        state: CodexWireProfileState,
        cache: Arc<dyn ProviderArtifactProfileCachePort>,
        transport: Arc<dyn DesktopArtifactTransport>,
    ) -> Self {
        Self {
            provider,
            state,
            cache,
            transport,
            verified: Mutex::new(baseline_artifacts()),
        }
    }
    pub async fn restore(&self) {
        for target in TARGETS {
            match self.cache.read(&self.provider, &target.key()).await {
                Ok(Some(cached)) => {
                    let parsed = serde_json::from_value::<VerifiedArtifact>(
                        serde_json::Value::Object(cached.profile().expose_to_provider().clone()),
                    );
                    if let Ok(mut artifact) = parsed
                        && sequence(target, &artifact.release)
                            .is_ok_and(|sequence| sequence == cached.artifact_sequence())
                        && self.accepts(target, &artifact.release)
                    {
                        artifact.release.verified_at = Some(cached.verified_at().into());
                        self.state.seed_client_release(
                            ClientKind::Desktop,
                            target.platform,
                            target.arch,
                            artifact.release.clone(),
                        );
                        self.verified
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .insert(target, artifact);
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    tracing::warn!(target = %target.key(), "OpenAI Desktop release cache could not be loaded")
                }
            }
        }
    }
    pub async fn refresh(&self) {
        for target in TARGETS {
            let result = self.refresh_target(target).await;
            if let Err(error) = &result {
                tracing::warn!(target = %target.key(), error, "OpenAI Desktop release check failed");
            }
            self.state.record_client_release(
                ClientKind::Desktop,
                target.platform,
                target.arch,
                result,
            );
        }
    }
    async fn refresh_target(&self, target: DesktopTarget) -> Result<ClientRelease, String> {
        let previous = self
            .verified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&target)
            .cloned();
        let mut artifact = self
            .transport
            .fetch(target, previous)
            .await
            .map_err(|error| error.to_string())?;
        let sequence = sequence(target, &artifact.release).map_err(|error| error.to_string())?;
        if !self.accepts(target, &artifact.release) {
            return Err("官方 Desktop 发布低于当前版本或同构建版本不一致".to_owned());
        }
        let verified_at = Utc::now();
        // 核验时间属于缓存信封，避免同构建重复检查被判定为不同制品。
        artifact.release.verified_at = None;
        let payload =
            object(&serde_json::to_value(&artifact).map_err(|_| "Desktop 发布序列化失败")?)
                .map_err(|error| error.to_string())?;
        let profile = ProviderArtifactProfile::new(
            self.provider.clone(),
            target.key(),
            sequence,
            verified_at.into(),
            payload,
        );
        match self
            .cache
            .replace_if_newer(profile, ARTIFACT_PROFILE_CACHE_TTL)
            .await
        {
            Ok(true) => {}
            Ok(false) => return Err("Desktop 发布缓存已更新，等待下次同步".to_owned()),
            Err(_) => return Err("Desktop 发布缓存写入失败".to_owned()),
        }
        artifact.release.verified_at = Some(verified_at);
        self.verified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(target, artifact.clone());
        Ok(artifact.release)
    }
    fn accepts(&self, target: DesktopTarget, next: &ClientRelease) -> bool {
        let Ok(next_sequence) = sequence(target, next) else {
            return false;
        };
        self.state
            .client_release(ClientKind::Desktop, target.platform, target.arch)
            .is_none_or(|current| {
                sequence(target, &current).is_ok_and(|current_sequence| {
                    next_sequence > current_sequence
                        || (next_sequence == current_sequence
                            && current.codex_version == next.codex_version
                            && current.desktop_version == next.desktop_version)
                })
            })
    }
}
fn sequence(target: DesktopTarget, release: &ClientRelease) -> io::Result<u64> {
    ClientProfileSelection {
        client: ClientKind::Desktop,
        platform: target.platform,
        version_mode: VersionMode::Fixed,
        arch: Some(target.arch.to_owned()),
        codex_version: Some(release.codex_version.clone()),
        desktop_version: release.desktop_version.clone(),
        desktop_build: release.desktop_build.clone(),
        ..ClientProfileSelection::default()
    }
    .validate()
    .map_err(|_| invalid())?;
    release
        .desktop_build
        .as_deref()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .ok_or_else(invalid)
}

fn baseline_artifacts() -> BTreeMap<DesktopTarget, VerifiedArtifact> {
    // 2026-09-18 分别读取四个官方安装包，版本元组与 ETag 均来自同一个包。
    // 离线可直接选择自动模式；ETag 变化后必须重新核验，不能延用另一平台的版本。
    let identities = [
        (774_919_598, "\"0x8DF141FFDF86290\""),
        (771_727_321, "\"0x8DF14202C49201F\""),
        (417_337_930, "\"0x8DF150AE1091545\""),
        (395_993_890, "\"0x8DF150ADC810C82\""),
    ];
    TARGETS
        .into_iter()
        .zip(identities)
        .map(|(target, (size, etag))| {
            let (core, version, build) = if target.platform == ClientPlatform::Windows {
                ("0.154.0-alpha.6.2", "26.908.70816", "9275")
            } else {
                ("0.155.0-alpha.9", "26.915.31029", "9771")
            };
            (
                target,
                VerifiedArtifact {
                    identity: ArtifactIdentity {
                        size,
                        etag: etag.to_owned(),
                    },
                    release: ClientRelease {
                        codex_version: core.to_owned(),
                        desktop_version: Some(version.to_owned()),
                        desktop_build: Some(build.to_owned()),
                        verified_at: chrono::DateTime::parse_from_rfc3339("2026-09-18T00:00:00Z")
                            .ok()
                            .map(|time| time.with_timezone(&Utc)),
                    },
                },
            )
        })
        .collect()
}
pub(super) fn seed_releases(state: &CodexWireProfileState) {
    for (target, artifact) in baseline_artifacts() {
        state.seed_client_release(
            ClientKind::Desktop,
            target.platform,
            target.arch,
            artifact.release,
        );
    }
}
