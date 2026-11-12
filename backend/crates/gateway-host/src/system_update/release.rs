//! Release 详情、版本比较与下载信任边界

use std::env;
use std::time::Duration;

use gateway_admin::model::system::{
    SystemUpdateChannel as UpdateChannel, SystemUpdateDetail, SystemUpdatePolicy,
};
use serde::Deserialize;

use super::{OperationError, SystemUpdateConfig, conflict, invalid};

pub(super) const MAX_DOWNLOAD_SIZE: u64 = 500 * 1024 * 1024;
pub(super) const MAX_CHECKSUM_SIZE: u64 = 1024 * 1024;

const APP_BINARY_NAME: &str = "codex-proxy-rs";

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GitHubRelease {
    pub(crate) tag_name: String,
    pub(crate) name: Option<String>,
    pub(crate) body: Option<String>,
    pub(crate) html_url: Option<String>,
    pub(crate) prerelease: bool,
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

impl GitHubRelease {
    pub(super) fn install_assets(
        &self,
        version: &str,
        api_base: &str,
    ) -> Result<(&GitHubAsset, &GitHubAsset), OperationError> {
        let os = match env::consts::OS {
            "macos" => "darwin",
            os => os,
        };
        let arch = match env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            arch => arch,
        };
        let name = format!("{APP_BINARY_NAME}_{version}_{os}_{arch}.tar.gz");
        let archive = self
            .assets
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| conflict(format!("当前发行版本没有适用于 {os}/{arch} 的更新包")))?;
        if !(1..=MAX_DOWNLOAD_SIZE).contains(&archive.size) {
            return Err(invalid("更新包大小无效或超出限制"));
        }
        let checksum = self
            .assets
            .iter()
            .find(|a| a.name == "checksums.txt")
            .ok_or_else(|| super::upstream("发行版本缺少 checksums.txt 校验文件，无法安全更新"))?;
        if !(1..=MAX_CHECKSUM_SIZE).contains(&checksum.size) {
            return Err(invalid("校验文件大小无效或超出限制"));
        }
        validate_download_url(&archive.browser_download_url, api_base)?;
        validate_download_url(&checksum.browser_download_url, api_base)?;
        Ok((archive, checksum))
    }
}

pub(crate) fn detail_from_release(
    config: &SystemUpdateConfig,
    release: &GitHubRelease,
    channel: UpdateChannel,
) -> SystemUpdateDetail {
    let unsupported_reason = config.update_support_error();
    let latest_version = normalize_version(&release.tag_name);
    let has_update = validate_update_target(&config.version, &latest_version, channel).is_ok();
    if unsupported_reason.is_some()
        || (!has_update && latest_version != normalize_version(&config.version))
    {
        return base_update_detail(config, channel, unsupported_reason, None);
    }
    SystemUpdateDetail {
        policy: update_policy(config, channel),
        current_version: config.version.clone(),
        latest_version,
        has_update,
        deployment_mode: config.deployment_mode.clone(),
        build_type: config.build_type.clone(),
        release_url: release.html_url.clone(),
        notes: release.body.clone().or_else(|| release.name.clone()),
        cached: false,
        update_supported: true,
        unsupported_reason: None,
        warning: None,
        retry_at: None,
    }
}

pub(crate) fn confirmed_target(target: Option<String>) -> Result<String, OperationError> {
    let target = target.ok_or_else(|| conflict("请先确认目标版本"))?;
    let target = normalize_version(&target);
    if target.is_empty() || semver::Version::parse(&target).is_err() {
        return Err(invalid("目标版本号格式无效"));
    }
    Ok(target)
}

/// 校验 GitHub Release 资产 URL 及其重定向目标是否位于受信任边界内
pub fn validate_download_url(raw: &str, api_base: &str) -> Result<(), OperationError> {
    let url = reqwest::Url::parse(raw)
        .map_err(|error| invalid("更新文件下载地址格式无效").with_source(error))?;
    if local_download_allowed(&url, api_base) {
        return Ok(());
    }
    if url.scheme() != "https" {
        return Err(invalid("更新文件必须通过 HTTPS 下载"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| invalid("更新文件下载地址缺少主机名"))?;
    if github_download_host_allowed(host) {
        Ok(())
    } else {
        Err(invalid("更新文件下载地址不在受信任的发行站点范围内"))
    }
}

pub(crate) fn download_client(
    api_base: &str,
    timeout: Duration,
) -> Result<reqwest::Client, OperationError> {
    let api_base = api_base.to_owned();
    gateway_egress::http_client(None)
        .map_err(|_| invalid("部署代理配置不合法"))?
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() < 5 && download_url_allowed(attempt.url(), &api_base) {
                attempt.follow()
            } else {
                attempt.error(std::io::Error::other(
                    "release redirect target is not trusted",
                ))
            }
        }))
        .build()
        .map_err(|error| super::error::http("无法初始化下载连接", error))
}

pub(crate) fn validate_repository(repository: &str) -> Result<(), OperationError> {
    let mut segments = repository.split('/');
    let owner = segments.next().unwrap_or_default();
    let name = segments.next().unwrap_or_default();
    if owner.is_empty()
        || matches!(owner, "." | "..")
        || matches!(name, "." | "..")
        || name.is_empty()
        || segments.next().is_some()
        || !owner.chars().all(repository_character)
        || !name.chars().all(repository_character)
    {
        return Err(conflict("更新仓库格式应为 owner/repository"));
    }
    Ok(())
}

pub(crate) fn validate_api_base(raw: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "发行查询地址格式无效".to_owned())?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("发行查询地址不能包含认证信息、查询参数或片段".to_owned());
    }
    if url.path().trim_end_matches('/') != "/repos" {
        return Err("发行查询地址的路径必须为 /repos".to_owned());
    }
    if url_host_is_loopback(&url) {
        return matches!(url.scheme(), "http" | "https")
            .then_some(())
            .ok_or_else(|| "本地发行查询地址必须使用 HTTP 或 HTTPS".to_owned());
    }
    if url.scheme() != "https" || url.host_str() != Some("api.github.com") || url.port().is_some() {
        return Err("发行查询地址必须为 https://api.github.com/repos".to_owned());
    }
    Ok(())
}

fn normalize_version(version: &str) -> String {
    version.trim().trim_start_matches('v').to_owned()
}

pub(super) fn channel_for_version(version: &semver::Version) -> Option<UpdateChannel> {
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

/// 检查更新与执行更新共用同一规则；构建元数据不构成更新
pub(super) fn update_target_allowed(
    current: &semver::Version,
    target: &semver::Version,
    selected: UpdateChannel,
) -> bool {
    use UpdateChannel::Experimental;
    if current.major != target.major || !target.cmp_precedence(current).is_gt() {
        return false;
    }
    let (Some(current_channel), Some(target_channel)) =
        (channel_for_version(current), channel_for_version(target))
    else {
        return false;
    };
    if current_channel == Experimental || selected == Experimental || target_channel == Experimental
    {
        return current_channel == Experimental
            && selected == Experimental
            && target_channel == Experimental
            && (current.minor, current.patch) == (target.minor, target.patch);
    }
    channel_accepts(selected, target_channel)
}

pub(super) fn channel_accepts(selected: UpdateChannel, target_channel: UpdateChannel) -> bool {
    use UpdateChannel::{Alpha, Beta, Rc, Stable};
    matches!(
        (selected, target_channel),
        (Stable, Stable)
            | (Rc, Rc | Stable)
            | (Beta, Beta | Rc | Stable)
            | (Alpha, Alpha | Beta | Rc | Stable)
    )
}

pub(crate) fn validate_update_target(
    current: &str,
    target: &str,
    channel: UpdateChannel,
) -> Result<(), OperationError> {
    let current = semver::Version::parse(&normalize_version(current))
        .map_err(|_| conflict("当前版本格式无效"))?;
    let target = semver::Version::parse(&normalize_version(target))
        .map_err(|_| invalid("目标版本号格式无效"))?;
    if !update_target_allowed(&current, &target, channel) {
        return Err(conflict(
            "所选通道不允许此目标，或目标涉及跨实验线、跨大版本、降级",
        ));
    }
    Ok(())
}

fn repository_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
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

impl SystemUpdateConfig {
    pub(super) fn update_support_error(&self) -> Option<String> {
        if !matches!(self.build_type.as_str(), "release" | "experimental") {
            return Some("在线更新需要官方发布构建".to_owned());
        }
        let Some(channel) = version_channel(&self.version) else {
            return Some("当前版本不符合发行命名规范，无法确定更新通道".to_owned());
        };
        if self.build_type == "experimental" && channel != UpdateChannel::Experimental {
            return Some("实验构建必须使用 exp.N 版本，无法在线更新".to_owned());
        }
        let Some(repository) = self.update_repository.as_deref() else {
            return Some("检查更新需要配置 CPR_UPDATE_REPOSITORY".to_owned());
        };
        if let Err(error) = validate_repository(repository) {
            return Some(error.message().to_owned());
        }
        if let Err(error) = validate_api_base(&self.github_api_base) {
            return Some(error);
        }
        if let Err(error) = self.web_dist_dir() {
            return Some(error.message().to_owned());
        }
        None
    }
}

pub(super) fn update_policy(
    config: &SystemUpdateConfig,
    channel: UpdateChannel,
) -> SystemUpdatePolicy {
    use UpdateChannel::{Alpha, Beta, Experimental, Rc, Stable};
    SystemUpdatePolicy {
        channel,
        available_channels: if version_channel(&config.version) == Some(Experimental) {
            vec![Experimental]
        } else {
            vec![Stable, Rc, Beta, Alpha]
        },
    }
}

pub(super) fn base_update_detail(
    config: &SystemUpdateConfig,
    channel: UpdateChannel,
    unsupported_reason: Option<String>,
    warning: Option<&OperationError>,
) -> SystemUpdateDetail {
    SystemUpdateDetail {
        policy: update_policy(config, channel),
        current_version: config.version.clone(),
        latest_version: config.version.clone(),
        has_update: false,
        deployment_mode: config.deployment_mode.clone(),
        build_type: config.build_type.clone(),
        release_url: None,
        notes: None,
        cached: false,
        update_supported: unsupported_reason.is_none() && warning.is_none(),
        unsupported_reason,
        warning: warning.map(|error| error.message().to_owned()),
        retry_at: warning.and_then(OperationError::retry_at),
    }
}
