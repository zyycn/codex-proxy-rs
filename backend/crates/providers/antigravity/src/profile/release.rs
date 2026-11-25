//! 按客户端、平台与架构刷新官方发布资料，失败时保留上次成功结果

pub(crate) use super::artifact::Release;
use super::selection::{Architecture, ClientKind, Platform, ReleaseTarget, valid_version};
use super::verification::{VerificationCache, VerifiedArtifact};
use crate::ConfigError;
use chrono::{DateTime, Utc};
use futures::{StreamExt, future::BoxFuture, stream};
use gateway_core::task::{ScheduledTask, WorkerCycleContext, WorkerTaskError};
use gateway_core::{provider_ports::ProviderArtifactProfileCachePort, routing::ProviderKind};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

const DOWNLOAD_PAGE: &str = "https://antigravity.google/download";
const DESKTOP_PREFIX: &str = "https://storage.googleapis.com/antigravity-public/antigravity-hub/";
const CLI_MANIFESTS: &str =
    "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests";
const MAX_RELEASE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Default)]
pub(crate) struct ReleaseSnapshot {
    pub(crate) release: Option<Release>,
    pub(crate) checked: Option<DateTime<Utc>>,
    pub(crate) failed: bool,
    verified: Option<VerifiedArtifact>,
}
impl ReleaseSnapshot {
    pub(crate) fn verified_at(&self, version: &str) -> Option<DateTime<Utc>> {
        self.verified
            .as_ref()
            .filter(|verified| {
                verified.release.version == version
                    && self.release.as_ref() == Some(&verified.release)
            })
            .map(|v| v.verified_at)
    }
    pub(crate) fn version(&self, client: ClientKind) -> &str {
        self.release
            .as_ref()
            .map_or(client.baseline().version, |release| {
                release.version.as_str()
            })
    }
}
pub(crate) struct ReleaseService {
    client: reqwest::Client,
    state: RwLock<BTreeMap<ReleaseTarget, ReleaseSnapshot>>,
    requested: Mutex<BTreeSet<ReleaseTarget>>,
    verification: VerificationCache,
}
impl ReleaseService {
    pub(crate) fn new(
        cache: Arc<dyn ProviderArtifactProfileCachePort>,
    ) -> Result<Self, ConfigError> {
        let client = gateway_egress::http_client(None)
            .map_err(|_| ConfigError)?
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| ConfigError)?;
        Ok(Self {
            client,
            state: RwLock::default(),
            requested: Mutex::new(BTreeSet::from([super::Selection::default().target()])),
            verification: VerificationCache::new(
                ProviderKind::new("antigravity").map_err(|_| ConfigError)?,
                cache,
            ),
        })
    }
    pub(crate) fn snapshot(&self, target: ReleaseTarget) -> ReleaseSnapshot {
        self.requested
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(target);
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&target)
            .cloned()
            .unwrap_or_default()
    }
    pub(crate) fn publish(&self, target: ReleaseTarget, result: Option<Release>) {
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = state.entry(target).or_default();
        entry.checked = Some(Utc::now());
        entry.failed = result.is_none();
        if let Some(release) = result {
            entry.release = Some(release);
        }
    }
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, WorkerTaskError> {
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| {
                WorkerTaskError::safe("读取 Antigravity 发布资料失败").with_source(e.without_url())
            })?
            .error_for_status()
            .map_err(|e| {
                WorkerTaskError::safe("Antigravity 发布资料返回错误状态")
                    .with_source(e.without_url())
            })?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            WorkerTaskError::safe("读取 Antigravity 发布资料失败").with_source(e.without_url())
        })? {
            if body.len().saturating_add(chunk.len()) > MAX_RELEASE_BYTES {
                return Err(WorkerTaskError::safe("Antigravity 发布资料超出大小限制"));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
    pub(crate) async fn update(&self) -> Result<(), WorkerTaskError> {
        let due = {
            let state = self
                .state
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.is_empty()
                || state.values().any(|s| {
                    s.failed
                        || s.checked
                            .is_none_or(|at| (Utc::now() - at).num_seconds() >= 86400)
                })
        };
        let mut first_error = if due {
            self.refresh().await.err()
        } else {
            None
        };
        let targets: Vec<_> = self
            .requested
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .collect();
        let mut verification = stream::iter(targets)
            .map(|target| async move {
                let snapshot = self.snapshot(target);
                let Some(release) = snapshot.release.as_ref() else {
                    return Ok(());
                };
                let verified = self
                    .verification
                    .verify(&self.client, target, release, snapshot.verified.as_ref())
                    .await?;
                let mut state = self
                    .state
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(current) = state.get_mut(&target)
                    && current.release.as_ref() == Some(&verified.release)
                {
                    current.verified = Some(verified);
                }
                Ok::<_, WorkerTaskError>(())
            })
            .buffer_unordered(2);
        while let Some(result) = verification.next().await {
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
    async fn refresh(&self) -> Result<(), WorkerTaskError> {
        let page = self.fetch(DOWNLOAD_PAGE).await;
        let mut first_error = None;
        for platform in [Platform::Macos, Platform::Linux, Platform::Windows] {
            for arch in [Architecture::Arm64, Architecture::Amd64] {
                let target = ReleaseTarget {
                    client: ClientKind::Desktop,
                    platform,
                    arch,
                };
                let release = page
                    .as_ref()
                    .ok()
                    .and_then(|page| std::str::from_utf8(page).ok())
                    .and_then(|page| parse_desktop(page, target));
                if release.is_none() {
                    first_error = Some(WorkerTaskError::safe(
                        "无法解析 Antigravity Desktop 发布资料",
                    ));
                }
                self.publish(target, release);
            }
        }
        if let Err(error) = page {
            first_error = Some(error);
        }
        let targets = [Platform::Macos, Platform::Linux, Platform::Windows]
            .into_iter()
            .flat_map(|platform| {
                [Architecture::Arm64, Architecture::Amd64].map(move |arch| ReleaseTarget {
                    client: ClientKind::Cli,
                    platform,
                    arch,
                })
            });
        let mut updates = stream::iter(targets)
            .map(|target| async move {
                let url = format!(
                    "{CLI_MANIFESTS}/{}_{}.json",
                    target.platform.os_type(),
                    target.arch.label()
                );
                let result = self
                    .fetch(&url)
                    .await
                    .and_then(|body| parse_cli(&body, target));
                self.publish(target, result.as_ref().ok().cloned());
                result
            })
            .buffer_unordered(3);
        while let Some(result) = updates.next().await {
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}
pub(crate) fn parse_desktop(page: &str, target: ReleaseTarget) -> Option<Release> {
    let (platform, filename) = match target.platform {
        Platform::Macos => ("darwin", "Antigravity.dmg"),
        Platform::Linux => ("linux", "Antigravity.tar.gz"),
        Platform::Windows => (
            "windows",
            match target.arch {
                Architecture::Arm64 => "Antigravity-arm64.exe",
                Architecture::Amd64 => "Antigravity-x64.exe",
            },
        ),
    };
    let arch = match target.arch {
        Architecture::Arm64 => "arm",
        Architecture::Amd64 => "x64",
    };
    let suffix = format!("/{platform}-{arch}/{filename}");
    let mut found = None;
    for quoted in page.split("href=").skip(1) {
        let Some(quote) = quoted.chars().next() else {
            continue;
        };
        if !matches!(quote, '"' | '\'') {
            continue;
        }
        let Some((href, _)) = quoted[1..].split_once(quote) else {
            continue;
        };
        let Some((version, build)) = href
            .strip_prefix(DESKTOP_PREFIX)
            .and_then(|s| s.strip_suffix(&suffix))
            .and_then(|s| s.split_once('-'))
        else {
            continue;
        };
        if !valid_version(version)
            || build.is_empty()
            || build.len() > 20
            || !build.bytes().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        let release = Release {
            version: version.into(),
            build: Some(build.into()),
            url: href.into(),
            sha512: None,
        };
        if found.as_ref().is_some_and(|current| current != &release) {
            return None;
        }
        found = Some(release);
    }
    found
}
pub(crate) fn parse_cli(body: &[u8], target: ReleaseTarget) -> Result<Release, WorkerTaskError> {
    #[derive(Deserialize)]
    struct Manifest {
        version: String,
        url: String,
        sha512: String,
    }
    let manifest: Manifest = serde_json::from_slice(body)
        .map_err(|e| WorkerTaskError::safe("无法解析 Antigravity CLI 发布资料").with_source(e))?;
    let prefix = format!(
        "https://storage.googleapis.com/antigravity-public/antigravity-cli/{}-",
        manifest.version
    );
    let build = manifest
        .url
        .strip_prefix(&prefix)
        .and_then(|s| s.split_once('/'))
        .map(|(build, _)| build.to_owned());
    let directory_arch = match target.arch {
        Architecture::Arm64 => "arm",
        Architecture::Amd64 => "x64",
    };
    let file_arch = match target.arch {
        Architecture::Arm64 => "arm64",
        Architecture::Amd64 => "x64",
    };
    let file_platform = match target.platform {
        Platform::Macos => "mac",
        Platform::Linux => "linux",
        Platform::Windows => "windows",
    };
    let extension = if target.platform == Platform::Windows {
        "exe"
    } else {
        "tar.gz"
    };
    let suffix = format!(
        "/{}-{directory_arch}/cli_{file_platform}_{file_arch}.{extension}",
        target.platform.os_type()
    );
    if !valid_version(&manifest.version)
        || target.client != ClientKind::Cli
        || build
            .as_ref()
            .is_some_and(|build| manifest.url != format!("{prefix}{build}{suffix}"))
        || manifest.sha512.len() != 128
        || !manifest.sha512.bytes().all(|c| c.is_ascii_hexdigit())
        || build
            .as_ref()
            .is_none_or(|v| v.is_empty() || v.len() > 20 || !v.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err(WorkerTaskError::safe("Antigravity CLI 发布资料无效"));
    }
    Ok(Release {
        version: manifest.version,
        build,
        url: manifest.url,
        sha512: Some(manifest.sha512.to_ascii_lowercase()),
    })
}
pub(crate) struct ReleaseTask(pub(crate) Arc<ReleaseService>);
impl ScheduledTask for ReleaseTask {
    fn run_cycle(&self, ctx: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            tokio::select! {_=ctx.cancellation().cancelled()=>Ok(()),result=self.0.update()=>result}
        })
    }
}
