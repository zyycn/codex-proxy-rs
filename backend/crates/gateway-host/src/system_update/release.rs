//! GitHub Release 发现、缓存、版本比较与下载信任边界。

use std::env;
use std::time::{Duration, Instant};

use gateway_admin::model::system::SystemUpdateDetail;
use serde::Deserialize;
use tokio::sync::Mutex;

use super::{OperationError, SystemUpdateConfig, conflict, invalid, upstream};

const APP_BINARY_NAME: &str = "codex-proxy-rs";
const CACHE_TTL: Duration = Duration::from_secs(20 * 60);

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GitHubRelease {
    pub(crate) tag_name: String,
    pub(crate) name: Option<String>,
    pub(crate) body: Option<String>,
    pub(crate) html_url: Option<String>,
    pub(crate) prerelease: bool,
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
        match fetch_latest(&config.github_api_base, repository).await {
            Ok(release) => {
                let detail = detail_from_release(config, &release);
                *self.entry.lock().await = Some(CachedRelease {
                    key,
                    detail: detail.clone(),
                    cached_at: Instant::now(),
                });
                Ok(detail)
            }
            Err(error) => self.cached(&key).await.ok_or(error),
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
) -> Result<GitHubRelease, OperationError> {
    validate_api_base(api_base).map_err(conflict)?;
    validate_repository(repository)?;
    let url = format!(
        "{}/{repository}/releases/latest",
        api_base.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| upstream(format!("failed to create release client: {error}")))?;
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
        .json::<GitHubRelease>()
        .await
        .map_err(|error| upstream(format!("invalid GitHub release response: {error}")))
}

pub(crate) fn detail_from_release(
    config: &SystemUpdateConfig,
    release: &GitHubRelease,
) -> SystemUpdateDetail {
    let latest = normalize_version(&release.tag_name);
    let available = (config.update_channel != "stable" || !release.prerelease)
        && version_is_newer(&config.version, &latest).unwrap_or(false);
    SystemUpdateDetail {
        current_version: config.version.clone(),
        latest_version: latest,
        has_update: available,
        deployment_mode: config.deployment_mode.clone(),
        build_type: config.build_type.clone(),
        release_url: release.html_url.clone(),
        notes: release.body.clone().or_else(|| release.name.clone()),
        cached: false,
        update_supported: config.update_support_error().is_none(),
        unsupported_reason: config.update_support_error(),
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

pub(crate) fn validate_download_url(raw: &str, api_base: &str) -> Result<(), OperationError> {
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

fn version_is_newer(current: &str, latest: &str) -> Option<bool> {
    let current = semver::Version::parse(&normalize_version(current)).ok()?;
    let latest = semver::Version::parse(&normalize_version(latest)).ok()?;
    Some(latest > current)
}

fn repository_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

fn platform_os_aliases() -> &'static [&'static str] {
    match env::consts::OS {
        "macos" => &["macos", "darwin"],
        "windows" => &["windows", "win32"],
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
