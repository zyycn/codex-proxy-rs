//! 自更新 Release 查询、缓存和 asset 选择。

use std::{
    env,
    time::{Duration, Instant},
};

use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::update::types::UpdateError;

use super::{
    download::validate_github_api_base,
    service::{build_type_label, deployment_mode_label, SystemUpdateConfig, APP_BINARY_NAME},
};

const UPDATE_CACHE_TTL: Duration = Duration::from_secs(20 * 60);

static RELEASE_CACHE: Mutex<Option<CachedUpdateInfo>> = Mutex::const_new(None);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateInfoData {
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
    pub deployment_mode: String,
    pub deployment_mode_label: String,
    pub build_type: String,
    pub build_type_label: String,
    pub release_url: Option<String>,
    pub notes: Option<String>,
    pub cached: bool,
    pub update_supported: bool,
    pub unsupported_reason: Option<String>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedUpdateInfo {
    key: String,
    info: UpdateInfoData,
    cached_at: Instant,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct GitHubRelease {
    pub tag_name: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub html_url: Option<String>,
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<GitHubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct GitHubAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

pub(super) async fn check_latest_release(
    config: &SystemUpdateConfig,
    force: bool,
) -> UpdateInfoData {
    let Some(repository) = config.update_repository.as_deref() else {
        return UpdateInfoData {
            current_version: config.version.clone(),
            latest_version: config.version.clone(),
            has_update: false,
            deployment_mode: config.deployment_mode.clone(),
            deployment_mode_label: deployment_mode_label(&config.deployment_mode).to_string(),
            build_type: config.build_type.clone(),
            build_type_label: build_type_label(&config.build_type).to_string(),
            release_url: None,
            notes: None,
            cached: false,
            update_supported: false,
            unsupported_reason: Some("检查更新需要配置 CPR_UPDATE_REPOSITORY".to_string()),
            warning: None,
        };
    };
    if let Err(reason) = validate_github_api_base(&config.github_api_base) {
        return UpdateInfoData {
            current_version: config.version.clone(),
            latest_version: config.version.clone(),
            has_update: false,
            deployment_mode: config.deployment_mode.clone(),
            deployment_mode_label: deployment_mode_label(&config.deployment_mode).to_string(),
            build_type: config.build_type.clone(),
            build_type_label: build_type_label(&config.build_type).to_string(),
            release_url: None,
            notes: None,
            cached: false,
            update_supported: false,
            unsupported_reason: Some(reason),
            warning: None,
        };
    }
    let cache_key = config.release_cache_key();

    if !force {
        if let Some(info) = cached_release_info(&cache_key).await {
            return info;
        }
    }

    match fetch_latest_release(&config.github_api_base, repository).await {
        Ok(release) => {
            let info = update_info_from_release(config, release);
            cache_release_info(cache_key, &info).await;
            info
        }
        Err(error) => cached_release_info(&cache_key)
            .await
            .unwrap_or_else(|| UpdateInfoData {
                current_version: config.version.clone(),
                latest_version: config.version.clone(),
                has_update: false,
                deployment_mode: config.deployment_mode.clone(),
                deployment_mode_label: deployment_mode_label(&config.deployment_mode).to_string(),
                build_type: config.build_type.clone(),
                build_type_label: build_type_label(&config.build_type).to_string(),
                release_url: None,
                notes: None,
                cached: false,
                update_supported: false,
                unsupported_reason: config.update_support_error(),
                warning: Some(error),
            }),
    }
}

pub(super) async fn fetch_latest_release(
    api_base: &str,
    repository: &str,
) -> Result<GitHubRelease, String> {
    let url = format!(
        "{}/{repository}/releases/latest",
        api_base.trim_end_matches('/')
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(reqwest::header::USER_AGENT, APP_BINARY_NAME)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GitHub release check failed with {status}"));
    }
    response
        .json::<GitHubRelease>()
        .await
        .map_err(|error| error.to_string())
}

pub(super) fn update_info_from_release(
    config: &SystemUpdateConfig,
    release: GitHubRelease,
) -> UpdateInfoData {
    let latest_version = normalize_version_tag(&release.tag_name);
    let has_update = release_allowed(config, &release)
        && version_is_newer(&config.version, &latest_version).unwrap_or(false);
    let unsupported_reason = config.update_support_error();
    let update_supported = unsupported_reason.is_none();
    let notes = release.body.or(release.name);
    UpdateInfoData {
        current_version: config.version.clone(),
        latest_version,
        has_update,
        deployment_mode: config.deployment_mode.clone(),
        deployment_mode_label: deployment_mode_label(&config.deployment_mode).to_string(),
        build_type: config.build_type.clone(),
        build_type_label: build_type_label(&config.build_type).to_string(),
        release_url: release.html_url,
        notes,
        cached: false,
        update_supported,
        unsupported_reason,
        warning: None,
    }
}

pub(super) fn select_release_archive<'a>(
    release: &'a GitHubRelease,
    version: &str,
) -> Result<&'a GitHubAsset, UpdateError> {
    let os_aliases = platform_os_aliases();
    let arch_aliases = platform_arch_aliases();
    let normalized = normalize_version_tag(version);
    release
        .assets
        .iter()
        .find(|asset| {
            let name = asset.name.as_str();
            name.contains(APP_BINARY_NAME)
                && name.contains(&normalized)
                && asset_matches_current_platform(name, os_aliases, arch_aliases)
                && !name.ends_with(".txt")
        })
        .or_else(|| {
            release.assets.iter().find(|asset| {
                let name = asset.name.as_str();
                asset_matches_current_platform(name, os_aliases, arch_aliases)
                    && !name.ends_with(".txt")
            })
        })
        .ok_or_else(|| {
            UpdateError::conflict(format!(
                "No compatible release archive found for {}/{}",
                env::consts::OS,
                env::consts::ARCH
            ))
        })
}

async fn cached_release_info(cache_key: &str) -> Option<UpdateInfoData> {
    let cache = RELEASE_CACHE.lock().await;
    let cached = cache.as_ref()?;
    if cached.key != cache_key || cached.cached_at.elapsed() > UPDATE_CACHE_TTL {
        return None;
    }
    let mut info = cached.info.clone();
    info.cached = true;
    Some(info)
}

async fn cache_release_info(cache_key: String, info: &UpdateInfoData) {
    let mut cache = RELEASE_CACHE.lock().await;
    *cache = Some(CachedUpdateInfo {
        key: cache_key,
        info: info.clone(),
        cached_at: Instant::now(),
    });
}

fn release_allowed(config: &SystemUpdateConfig, release: &GitHubRelease) -> bool {
    config.update_channel != "stable" || !release.prerelease
}

fn version_is_newer(current: &str, latest: &str) -> Option<bool> {
    let current = Version::parse(&normalize_version_tag(current)).ok()?;
    let latest = Version::parse(&normalize_version_tag(latest)).ok()?;
    Some(latest > current)
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

fn asset_matches_current_platform(name: &str, os_aliases: &[&str], arch_aliases: &[&str]) -> bool {
    os_aliases.iter().any(|os| name.contains(os))
        && arch_aliases.iter().any(|arch| name.contains(arch))
}

pub(super) fn normalize_version_tag(version: &str) -> String {
    version.trim().trim_start_matches('v').to_string()
}
