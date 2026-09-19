//! GitHub Release 发现、缓存、版本比较与下载信任边界。

use std::env;
use std::time::{Duration, Instant};

use gateway_admin::model::system::SystemUpdateDetail;
use serde::Deserialize;
use tokio::sync::Mutex;

use super::{OperationError, SystemUpdateConfig, conflict, invalid, upstream};

const APP_BINARY_NAME: &str = "codex-proxy-rs";
const CACHE_TTL: Duration = Duration::from_secs(20 * 60);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateChannel {
    Stable,
    Alpha,
    Beta,
    Rc,
    Experimental,
}

impl UpdateChannel {
    pub(crate) fn label(&self) -> &str {
        match self {
            Self::Stable => "stable",
            Self::Alpha => "alpha",
            Self::Beta => "beta",
            Self::Rc => "rc",
            Self::Experimental => "exp",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GitHubRelease {
    pub(crate) tag_name: String,
    pub(crate) name: Option<String>,
    pub(crate) body: Option<String>,
    pub(crate) html_url: Option<String>,
    pub(crate) prerelease: bool,
    #[serde(default)]
    pub(crate) draft: bool,
    #[serde(default)]
    pub(crate) assets: Vec<GitHubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GitHubAsset {
    pub(crate) name: String,
    pub(crate) browser_download_url: String,
    pub(crate) size: u64,
}

struct CachedRelease {
    key: String,
    detail: SystemUpdateDetail,
    cached_at: Instant,
}

#[derive(Default)]
pub(crate) struct ReleaseCache {
    entry: Mutex<Option<CachedRelease>>,
}

impl ReleaseCache {
    pub(crate) async fn detail(
        &self,
        config: &SystemUpdateConfig,
        refresh: bool,
    ) -> Result<SystemUpdateDetail, OperationError> {
        if let Some(reason) = config.update_support_error() {
            return Ok(super::base_update_detail(config, Some(reason), None));
        }
        let repository = config
            .update_repository
            .as_deref()
            .ok_or_else(|| conflict("update repository is not configured"))?;
        validate_repository(repository)?;
        validate_api_base(&config.github_api_base).map_err(conflict)?;
        let key = config.release_cache_key();
        if !refresh && let Some(detail) = self.cached(&key).await {
            return Ok(detail);
        }
        match fetch_latest(&config.github_api_base, repository, &config.version).await {
            Ok(release) => {
                let detail = release.as_ref().map_or_else(
                    || super::base_update_detail(config, config.update_support_error(), None),
                    |release| detail_from_release(config, release),
                );
                *self.entry.lock().await = Some(CachedRelease {
                    key,
                    detail: detail.clone(),
                    cached_at: Instant::now(),
                });
                Ok(detail)
            }
            Err(error) => {
                // 强制检查失败后清掉旧结果，避免后续版本查询再次显示旧的更新标记。
                *self.entry.lock().await = None;
                Err(error)
            }
        }
    }

    async fn cached(&self, key: &str) -> Option<SystemUpdateDetail> {
        let entry = self.entry.lock().await;
        let cached = entry.as_ref()?;
        (cached.key == key && cached.cached_at.elapsed() <= CACHE_TTL).then(|| {
            let mut detail = cached.detail.clone();
            detail.cached = true;
            detail
        })
    }
}

pub(crate) async fn fetch_latest(
    api_base: &str,
    repository: &str,
    current_version: &str,
) -> Result<Option<GitHubRelease>, OperationError> {
    validate_api_base(api_base).map_err(conflict)?;
    validate_repository(repository)?;
    let base = api_base.trim_end_matches('/');
    let current = semver::Version::parse(&normalize_version(current_version))
        .map_err(|_| invalid("current version is invalid"))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| upstream(format!("failed to create release client: {error}")))?;

    // 所有发行线先过滤再排序，避免更高大版本或其他实验遮住仍可安装的更新。
    let mut latest: Option<(semver::Version, GitHubRelease)> = None;
    let mut page = 1;
    loop {
        let releases: Vec<GitHubRelease> = fetch_release_json(
            &client,
            &format!("{base}/{repository}/releases?per_page=100&page={page}"),
        )
        .await?;
        let last_page = releases.len() < 100;
        for release in releases {
            let Some(version) = eligible_release_version(&release, &current) else {
                continue;
            };
            if latest
                .as_ref()
                .is_none_or(|(previous, _)| version.cmp_precedence(previous).is_gt())
            {
                latest = Some((version, release));
            }
        }
        if last_page {
            return Ok(latest.map(|(_, release)| release));
        }
        page += 1;
    }
}

fn eligible_release_version(
    release: &GitHubRelease,
    current: &semver::Version,
) -> Option<semver::Version> {
    let version = semver::Version::parse(&normalize_version(&release.tag_name)).ok()?;
    (!release.draft
        && release.prerelease != version.pre.is_empty()
        && update_target_allowed(current, &version))
    .then_some(version)
}

async fn fetch_release_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T, OperationError> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, APP_BINARY_NAME)
        .send()
        .await
        .map_err(|error| upstream(format!("GitHub release check failed: {error}")))?;
    if !response.status().is_success() {
        return Err(upstream(format!(
            "GitHub release check failed with {}",
            response.status()
        )));
    }
    response
        .json::<T>()
        .await
        .map_err(|error| upstream(format!("invalid GitHub release response: {error}")))
}

pub(crate) fn detail_from_release(
    config: &SystemUpdateConfig,
    release: &GitHubRelease,
) -> SystemUpdateDetail {
    let unsupported_reason = config.update_support_error();
    if unsupported_reason.is_some()
        || validate_update_target(&config.version, &release.tag_name).is_err()
    {
        return super::base_update_detail(config, unsupported_reason, None);
    }
    SystemUpdateDetail {
        current_version: config.version.clone(),
        latest_version: normalize_version(&release.tag_name),
        has_update: true,
        deployment_mode: config.deployment_mode.clone(),
        build_type: config.build_type.clone(),
        release_url: release.html_url.clone(),
        notes: release.body.clone().or_else(|| release.name.clone()),
        cached: false,
        update_supported: true,
        unsupported_reason: None,
        warning: None,
    }
}

pub(crate) fn confirmed_target(target: Option<String>) -> Result<String, OperationError> {
    let target = target.ok_or_else(|| conflict("target version must be confirmed"))?;
    let target = normalize_version(&target);
    if target.is_empty() || semver::Version::parse(&target).is_err() {
        return Err(invalid("target version is invalid"));
    }
    Ok(target)
}

pub(crate) fn select_archive<'a>(
    release: &'a GitHubRelease,
    version: &str,
) -> Result<&'a GitHubAsset, OperationError> {
    let os = platform_os_aliases();
    let arch = platform_arch_aliases();
    let normalized = normalize_version(version);
    release
        .assets
        .iter()
        .find(|asset| {
            asset.name.contains(APP_BINARY_NAME)
                && asset.name.contains(&normalized)
                && asset_matches_platform(&asset.name, os, arch)
                && !asset.name.ends_with(".txt")
        })
        .or_else(|| {
            release.assets.iter().find(|asset| {
                asset_matches_platform(&asset.name, os, arch) && !asset.name.ends_with(".txt")
            })
        })
        .ok_or_else(|| {
            conflict(format!(
                "no compatible release archive for {}/{}",
                env::consts::OS,
                env::consts::ARCH
            ))
        })
}

/// 校验 GitHub Release 资产 URL 及其重定向目标是否位于受信任边界内。
pub fn validate_download_url(raw: &str, api_base: &str) -> Result<(), OperationError> {
    let url = reqwest::Url::parse(raw)
        .map_err(|error| invalid(format!("invalid download URL: {error}")))?;
    if local_download_allowed(&url, api_base) {
        return Ok(());
    }
    if url.scheme() != "https" {
        return Err(invalid("only HTTPS release downloads are allowed"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| invalid("download URL is missing host"))?;
    if github_download_host_allowed(host) {
        Ok(())
    } else {
        Err(invalid("release download host is not trusted"))
    }
}

pub(crate) fn download_client(
    api_base: &str,
    timeout: Duration,
) -> Result<reqwest::Client, OperationError> {
    let api_base = api_base.to_owned();
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if download_url_allowed(attempt.url(), &api_base) {
                attempt.follow()
            } else {
                attempt.error(std::io::Error::other(
                    "release redirect target is not trusted",
                ))
            }
        }))
        .build()
        .map_err(|error| upstream(format!("failed to create download client: {error}")))
}

pub(crate) fn validate_repository(repository: &str) -> Result<(), OperationError> {
    let mut segments = repository.split('/');
    let owner = segments.next().unwrap_or_default();
    let name = segments.next().unwrap_or_default();
    if owner.is_empty()
        || name.is_empty()
        || segments.next().is_some()
        || !owner.chars().all(repository_character)
        || !name.chars().all(repository_character)
    {
        return Err(conflict("update repository must use owner/repository"));
    }
    Ok(())
}

pub(crate) fn validate_api_base(raw: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(raw).map_err(|error| format!("invalid API base: {error}"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("API base must not include credentials, query, or fragment".to_owned());
    }
    if url.path().trim_end_matches('/') != "/repos" {
        return Err("API base path must be /repos".to_owned());
    }
    if url_host_is_loopback(&url) {
        return matches!(url.scheme(), "http" | "https")
            .then_some(())
            .ok_or_else(|| "loopback API base must use HTTP or HTTPS".to_owned());
    }
    if url.scheme() != "https" || url.host_str() != Some("api.github.com") {
        return Err("API base must be https://api.github.com/repos".to_owned());
    }
    Ok(())
}

fn normalize_version(version: &str) -> String {
    version.trim().trim_start_matches('v').to_owned()
}

fn channel_for_version(version: &semver::Version) -> Option<UpdateChannel> {
    if version.pre.is_empty() {
        return Some(UpdateChannel::Stable);
    }
    let (name, sequence) = version.pre.as_str().split_once('.')?;
    if sequence.is_empty() || sequence == "0" || !sequence.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    match name {
        "alpha" => Some(UpdateChannel::Alpha),
        "beta" => Some(UpdateChannel::Beta),
        "rc" => Some(UpdateChannel::Rc),
        "exp" => Some(UpdateChannel::Experimental),
        _ => None,
    }
}

pub(crate) fn version_channel(version: &str) -> Option<UpdateChannel> {
    let version = semver::Version::parse(&normalize_version(version)).ok()?;
    channel_for_version(&version)
}

/// 检查更新与执行更新共用同一规则；构建元数据不构成更新。
fn update_target_allowed(current: &semver::Version, target: &semver::Version) -> bool {
    use UpdateChannel::{Alpha, Beta, Experimental, Rc, Stable};

    if current.major != target.major || !target.cmp_precedence(current).is_gt() {
        return false;
    }
    let (Some(current_channel), Some(target_channel)) =
        (channel_for_version(current), channel_for_version(target))
    else {
        return false;
    };
    if current_channel == Stable {
        return target_channel == Stable;
    }
    // 预发行只承接本轮目标版本；exp 的相同基线也只代表同一轮实验。
    if (current.minor, current.patch) != (target.minor, target.patch) {
        return false;
    }
    matches!(
        (current_channel, target_channel),
        (Alpha, Alpha | Beta | Rc | Stable)
            | (Beta, Beta | Rc | Stable)
            | (Rc, Rc | Stable)
            | (Experimental, Experimental)
    )
}

pub(crate) fn validate_update_target(current: &str, target: &str) -> Result<(), OperationError> {
    let current = semver::Version::parse(&normalize_version(current))
        .map_err(|_| conflict("当前版本格式无效"))?;
    let target = semver::Version::parse(&normalize_version(target))
        .map_err(|_| invalid("target version is invalid"))?;
    if !update_target_allowed(&current, &target) {
        return Err(conflict(
            "当前版本不允许更新到此目标：不支持跨通道、跨发行线、跨大版本或降级",
        ));
    }
    Ok(())
}

fn repository_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

fn platform_os_aliases() -> &'static [&'static str] {
    match env::consts::OS {
        "macos" => &["macos", "darwin"],
        "linux" => &["linux"],
        _ => &[env::consts::OS],
    }
}

fn platform_arch_aliases() -> &'static [&'static str] {
    match env::consts::ARCH {
        "x86_64" => &["x86_64", "amd64"],
        "aarch64" => &["aarch64", "arm64"],
        _ => &[env::consts::ARCH],
    }
}

fn asset_matches_platform(name: &str, os: &[&str], arch: &[&str]) -> bool {
    os.iter().any(|alias| name.contains(alias)) && arch.iter().any(|alias| name.contains(alias))
}

fn github_download_host_allowed(host: &str) -> bool {
    host == "github.com"
        || host.ends_with(".github.com")
        || host == "objects.githubusercontent.com"
        || host.ends_with(".objects.githubusercontent.com")
        || host == "release-assets.githubusercontent.com"
}

fn local_download_allowed(url: &reqwest::Url, api_base: &str) -> bool {
    if !matches!(url.scheme(), "http" | "https") || !url_host_is_loopback(url) {
        return false;
    }
    let Ok(base) = reqwest::Url::parse(api_base) else {
        return false;
    };
    url_host_is_loopback(&base)
        && url.scheme() == base.scheme()
        && url.host_str() == base.host_str()
        && url.port_or_known_default() == base.port_or_known_default()
}

fn download_url_allowed(url: &reqwest::Url, api_base: &str) -> bool {
    local_download_allowed(url, api_base)
        || (url.scheme() == "https" && url.host_str().is_some_and(github_download_host_allowed))
}

fn url_host_is_loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host == "localhost"
            || host == "127.0.0.1"
            || host == "::1"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}
