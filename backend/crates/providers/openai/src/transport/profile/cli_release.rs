//! CLI 稳定发布来自官方 npm 包及其平台依赖，不与 Desktop 的内嵌版本混用。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt as _;
use gateway_core::provider_ports::{ProviderArtifactProfile, ProviderArtifactProfileCachePort};
use gateway_core::routing::ProviderKind;
use reqwest::{Client, redirect::Policy};
use serde::Deserialize;

use super::selection::{ClientKind, ClientPlatform, ClientRelease, object};
use super::{ARTIFACT_PROFILE_CACHE_TTL, CodexWireProfileState};

const ENDPOINT: &str = "https://registry.npmjs.org/@openai%2Fcodex/latest";
const MAX_BYTES: usize = 256 * 1024;
const TARGETS: [(ClientPlatform, &str, &str); 6] = [
    (ClientPlatform::Macos, "arm64", "darwin-arm64"),
    (ClientPlatform::Macos, "x86_64", "darwin-x64"),
    (ClientPlatform::Linux, "arm64", "linux-arm64"),
    (ClientPlatform::Linux, "x86_64", "linux-x64"),
    (ClientPlatform::Windows, "arm64", "win32-arm64"),
    (ClientPlatform::Windows, "x86_64", "win32-x64"),
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NpmRelease {
    name: String,
    version: String,
    optional_dependencies: BTreeMap<String, String>,
}

/// 验证稳定标签和各平台包确实来自同一次官方发布。
pub fn parse_cli_release(bytes: &[u8]) -> Result<String, CliReleaseError> {
    if bytes.len() > MAX_BYTES {
        return Err(CliReleaseError::Invalid);
    }
    let release: NpmRelease =
        serde_json::from_slice(bytes).map_err(|_| CliReleaseError::Invalid)?;
    sequence(&release.version)?;
    if release.name != "@openai/codex"
        || TARGETS.iter().any(|(_, _, target)| {
            release
                .optional_dependencies
                .get(&format!("@openai/codex-{target}"))
                != Some(&format!("npm:@openai/codex@{}-{target}", release.version))
        })
    {
        return Err(CliReleaseError::Invalid);
    }
    Ok(release.version)
}

fn sequence(version: &str) -> Result<u64, CliReleaseError> {
    let version = semver::Version::parse(version).map_err(|_| CliReleaseError::Invalid)?;
    if !version.pre.is_empty()
        || !version.build.is_empty()
        || version.major > 999
        || version.minor > 999
        || version.patch > 999
    {
        return Err(CliReleaseError::Invalid);
    }
    Ok(1 + version.major * 1_000_000 + version.minor * 1_000 + version.patch)
}

pub struct CliReleaseService {
    client: Client,
    provider: ProviderKind,
    state: CodexWireProfileState,
    cache: Arc<dyn ProviderArtifactProfileCachePort>,
}

impl CliReleaseService {
    pub fn new(
        provider: ProviderKind,
        state: CodexWireProfileState,
        cache: Arc<dyn ProviderArtifactProfileCachePort>,
    ) -> Result<Self, CliReleaseError> {
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| CliReleaseError::Fetch)?;
        Ok(Self {
            client,
            provider,
            state,
            cache,
        })
    }

    pub async fn restore(&self) {
        for (platform, arch, target) in TARGETS {
            let key = format!("cli-{target}");
            match self.cache.read(&self.provider, &key).await {
                Ok(Some(cached)) => {
                    if let Some(version) = cached
                        .profile()
                        .expose_to_provider()
                        .get("version")
                        .and_then(serde_json::Value::as_str)
                        && sequence(version)
                            .is_ok_and(|sequence| sequence == cached.artifact_sequence())
                        && self
                            .state
                            .client_release(ClientKind::Cli, platform, arch)
                            .is_none_or(|current| {
                                sequence(version).ok() >= sequence(&current.codex_version).ok()
                            })
                    {
                        self.state.seed_client_release(
                            ClientKind::Cli,
                            platform,
                            arch,
                            ClientRelease {
                                codex_version: version.to_owned(),
                                desktop_version: None,
                                desktop_build: None,
                                verified_at: Some(cached.verified_at().into()),
                            },
                        );
                    }
                }
                Ok(None) => {}
                Err(_) => tracing::warn!(target, "OpenAI CLI release cache could not be loaded"),
            }
        }
    }

    pub async fn refresh(&self) -> Result<(), CliReleaseError> {
        let result = self.fetch().await;
        for (platform, arch, target) in TARGETS {
            let outcome = match &result {
                Ok(version) => self.publish(platform, arch, target, version).await,
                Err(error) => Err(error.to_string()),
            };
            self.state
                .record_client_release(ClientKind::Cli, platform, arch, outcome);
        }
        result.map(|_| ())
    }

    async fn publish(
        &self,
        platform: ClientPlatform,
        arch: &str,
        target: &str,
        version: &str,
    ) -> Result<ClientRelease, String> {
        let next = sequence(version).map_err(|error| error.to_string())?;
        if self
            .state
            .client_release(ClientKind::Cli, platform, arch)
            .is_some_and(|current| {
                sequence(&current.codex_version).is_ok_and(|current| current > next)
            })
        {
            return Err("官方 CLI 发布版本低于当前已核验版本".to_owned());
        }
        let verified_at = Utc::now();
        let payload =
            object(&serde_json::json!({"version": version})).map_err(|error| error.to_string())?;
        let profile = ProviderArtifactProfile::new(
            self.provider.clone(),
            format!("cli-{target}"),
            next,
            verified_at.into(),
            payload,
        );
        match self
            .cache
            .replace_if_newer(profile, ARTIFACT_PROFILE_CACHE_TTL)
            .await
        {
            Ok(true) => Ok(ClientRelease {
                codex_version: version.to_owned(),
                desktop_version: None,
                desktop_build: None,
                verified_at: Some(verified_at),
            }),
            Ok(false) => Err("CLI 发布缓存已更新，等待下次同步".to_owned()),
            Err(_) => Err("CLI 发布版本缓存写入失败".to_owned()),
        }
    }

    async fn fetch(&self) -> Result<String, CliReleaseError> {
        let response = self
            .client
            .get(ENDPOINT)
            .send()
            .await
            .map_err(|_| CliReleaseError::Fetch)?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > MAX_BYTES as u64)
        {
            return Err(CliReleaseError::Fetch);
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| CliReleaseError::Fetch)?;
            if bytes.len().saturating_add(chunk.len()) > MAX_BYTES {
                return Err(CliReleaseError::Invalid);
            }
            bytes.extend_from_slice(&chunk);
        }
        parse_cli_release(&bytes)
    }
}

pub(super) fn seed_releases(state: &CodexWireProfileState) {
    // 2026-09-18 核对官方 npm latest 及六个平台依赖；这里只提供离线启动资料。
    for (platform, arch, _) in TARGETS {
        state.seed_client_release(
            ClientKind::Cli,
            platform,
            arch,
            ClientRelease {
                codex_version: "0.155.0".to_owned(),
                desktop_version: None,
                desktop_build: None,
                verified_at: chrono::DateTime::parse_from_rfc3339("2026-09-18T00:00:00Z")
                    .ok()
                    .map(|time| time.with_timezone(&Utc)),
            },
        );
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CliReleaseError {
    #[error("官方 CLI 发布检查失败")]
    Fetch,
    #[error("官方 CLI 稳定版本或平台依赖不完整")]
    Invalid,
}
