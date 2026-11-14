//! CLI 稳定发布来自官方 npm 包及其平台依赖，不与 Desktop 的内嵌版本混用

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use futures::{StreamExt as _, future::BoxFuture};
use gateway_core::provider_ports::{ProviderArtifactProfile, ProviderArtifactProfileCachePort};
use gateway_core::routing::ProviderKind;
use reqwest::{Client, redirect::Policy};
use serde::Deserialize;
use tokio::time::Instant;

use super::selection::{ClientKind, ClientPlatform, ClientRelease, object};
use super::{
    ARTIFACT_PROFILE_CACHE_RENEW_INTERVAL, ARTIFACT_PROFILE_CACHE_TTL, CodexWireProfileState,
    baseline,
};

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

/// 验证稳定标签和各平台包确实来自同一次官方发布
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

/// 官方 npm 发布读取边界
pub trait CliReleaseTransport: Send + Sync {
    fn fetch(&self) -> BoxFuture<'_, Result<String, CliReleaseError>>;
}

struct OfficialCliReleaseTransport {
    client: Client,
}

pub struct CliReleaseService {
    transport: Arc<dyn CliReleaseTransport>,
    provider: ProviderKind,
    state: CodexWireProfileState,
    cache: Arc<dyn ProviderArtifactProfileCachePort>,
    cache_writes: Mutex<BTreeMap<String, Instant>>,
}

impl CliReleaseService {
    pub fn new(
        provider: ProviderKind,
        state: CodexWireProfileState,
        cache: Arc<dyn ProviderArtifactProfileCachePort>,
    ) -> Result<Self, CliReleaseError> {
        let client = gateway_egress::http_client(None)
            .map_err(|_| CliReleaseError::Fetch)?
            .https_only(true)
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| CliReleaseError::Fetch)?;
        Ok(Self::with_transport(
            provider,
            state,
            cache,
            Arc::new(OfficialCliReleaseTransport { client }),
        ))
    }

    pub fn with_transport(
        provider: ProviderKind,
        state: CodexWireProfileState,
        cache: Arc<dyn ProviderArtifactProfileCachePort>,
        transport: Arc<dyn CliReleaseTransport>,
    ) -> Self {
        Self {
            transport,
            provider,
            state,
            cache,
            cache_writes: Mutex::default(),
        }
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
        let result = self.transport.fetch().await;
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
        let current = self.state.client_release(ClientKind::Cli, platform, arch);
        if current.as_ref().is_some_and(|current| {
            sequence(&current.codex_version).is_ok_and(|current| current > next)
        }) {
            return Err("官方 CLI 发布版本低于当前已核验版本".to_owned());
        }
        let unchanged = current.filter(|current| current.codex_version == version);
        if let Some(current) = &unchanged {
            let fresh = self
                .cache_writes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(target)
                .is_some_and(|written| written.elapsed() < ARTIFACT_PROFILE_CACHE_RENEW_INTERVAL);
            if fresh {
                return Ok(current.clone());
            }
        }
        // 同版探测和缓存续期保留实际核验时间，只有新版才产生新的核验时间
        let verified_at = unchanged
            .as_ref()
            .and_then(|release| release.verified_at)
            .unwrap_or_else(Utc::now);
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
            Ok(true) => {
                self.cache_writes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(target.to_owned(), Instant::now());
                Ok(ClientRelease {
                    codex_version: version.to_owned(),
                    desktop_version: None,
                    desktop_build: None,
                    verified_at: Some(verified_at),
                })
            }
            Ok(false) => Err("CLI 发布缓存已更新，等待下次同步".to_owned()),
            Err(_) => Err("CLI 发布版本缓存写入失败".to_owned()),
        }
    }
}

impl CliReleaseTransport for OfficialCliReleaseTransport {
    fn fetch(&self) -> BoxFuture<'_, Result<String, CliReleaseError>> {
        Box::pin(async move {
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
        })
    }
}

pub(super) fn seed_releases(state: &CodexWireProfileState) {
    for (platform, arch, _) in TARGETS {
        state.seed_client_release(
            ClientKind::Cli,
            platform,
            arch,
            ClientRelease {
                codex_version: baseline::CLI_RELEASE.version.to_owned(),
                desktop_version: None,
                desktop_build: None,
                verified_at: Some(baseline::CLI_RELEASE.verified_at()),
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
