//! 校验官方发行清单与平台制品，并编排插件发行导入

use std::{collections::BTreeSet, path::Path};

use serde::Deserialize;

use crate::{
    model::{
        AdminError, AdminErrorKind, MutationActor, MutationContext,
        plugins::{
            InspectedPluginArtifact, PluginHostCompatibility, PluginSource,
            official::{OfficialPluginImport, OfficialPluginReleaseIdentity},
        },
    },
    ports::plugin_release::{
        OfficialPluginReleaseFiles, OfficialPluginReleaseReadError,
        OfficialPluginReleaseReadErrorKind,
    },
};

use super::PluginsService;

const MANIFEST_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_MANIFEST_BYTES: usize = 256 * 1024;
const MAXIMUM_PLUGINS: usize = 64;
const MAXIMUM_ARTIFACTS_PER_PLUGIN: usize = 8;
const MAXIMUM_VERSION_BYTES: usize = 128;
const MAXIMUM_PUBLISHER_BYTES: usize = 256;
const MAXIMUM_STABILITY_BYTES: usize = 32;
const MAXIMUM_ARCHIVE_NAME_BYTES: usize = 255;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedReleaseManifest {
    schema_version: u32,
    sealed: bool,
    gateway_version: String,
    gateway_git_sha: String,
    plugin_host: serde_json::Value,
    plugins: Vec<OfficialPlugin>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OfficialPlugin {
    id: String,
    version: String,
    publisher: String,
    stability: String,
    artifacts: Vec<OfficialArtifact>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OfficialArtifact {
    release_os: String,
    release_architecture: String,
    target: String,
    os: String,
    architecture: String,
    archive: String,
    sha256: String,
}

struct ValidatedOfficialArtifact {
    inspected: InspectedPluginArtifact,
}

#[derive(Clone, Copy)]
struct Platform {
    release_os: &'static str,
    release_architecture: &'static str,
    target: &'static str,
    runtime_os: &'static str,
    runtime_architecture: &'static str,
}

impl PluginsService {
    /// 从与宿主发行物同信任边界的封口清单导入官方包
    ///
    /// 所有清单项和包体会先完成静态校验，再进入与上传/远程安装相同的持久化用例
    pub async fn import_official_release(
        &self,
        files: &dyn OfficialPluginReleaseFiles,
        identity: &OfficialPluginReleaseIdentity,
        context: &MutationContext,
    ) -> Result<OfficialPluginImport, AdminError> {
        if !matches!(context.actor, MutationActor::System) {
            return Err(AdminError::new(
                AdminErrorKind::Forbidden,
                "官方插件只能由宿主发行导入",
            ));
        }
        let Some(bytes) = files.manifest().await.map_err(map_read_error)? else {
            return Ok(OfficialPluginImport {
                artifacts: 0,
                config_revision: None,
            });
        };
        let ValidatedManifest {
            gateway_version,
            plugins,
            plugins_len,
        } = validate_manifest(&bytes, identity)?;
        let platform = current_platform()
            .ok_or_else(|| AdminError::invalid("当前平台不支持官方插件发行物"))?;
        let mut validated = Vec::with_capacity(plugins.len());

        // 先完成整份清单的静态检查；持久化故障后的已提交前缀由下次启动幂等补齐
        for plugin in plugins {
            let artifact = select_artifact(&plugin, platform)?;
            let archive = files
                .artifact(&artifact.archive)
                .await
                .map_err(map_read_error)?;
            let inspected = self
                .inspector
                .inspect(archive, Some(artifact.sha256.clone()))
                .await?;
            validate_inspected(&plugin, &artifact, &inspected)?;
            validated.push(ValidatedOfficialArtifact { inspected });
        }

        let mut revision = None;
        for artifact in validated {
            let mutation = self
                .persist(
                    artifact.inspected,
                    PluginSource::Builtin {
                        release: gateway_version.clone(),
                    },
                    context,
                )
                .await?;
            revision = Some(mutation.config_revision);
        }
        Ok(OfficialPluginImport {
            artifacts: plugins_len,
            config_revision: revision,
        })
    }
}

struct ValidatedManifest {
    gateway_version: String,
    plugins: Vec<OfficialPlugin>,
    plugins_len: usize,
}

fn validate_manifest(
    bytes: &[u8],
    identity: &OfficialPluginReleaseIdentity,
) -> Result<ValidatedManifest, AdminError> {
    let manifest = parse_manifest(bytes)?;
    parse_host_compatibility(manifest.plugin_host)?;
    if manifest.gateway_version != identity.gateway_version
        || manifest.gateway_git_sha != identity.gateway_git_sha
    {
        return Err(invalid_manifest());
    }
    let plugins_len = manifest.plugins.len();
    Ok(ValidatedManifest {
        gateway_version: manifest.gateway_version,
        plugins: manifest.plugins,
        plugins_len,
    })
}

pub(crate) fn update_compatibility(
    bytes: &[u8],
    target_version: &str,
) -> Result<PluginHostCompatibility, AdminError> {
    let manifest = parse_manifest(bytes)?;
    if manifest.gateway_version != target_version {
        return Err(invalid_manifest());
    }
    parse_host_compatibility(manifest.plugin_host)
}

pub(crate) fn validate_update_release(
    bytes: &[u8],
    target_version: &str,
) -> Result<(), AdminError> {
    let manifest = parse_manifest(bytes)?;
    if manifest.gateway_version != target_version {
        return Err(invalid_manifest());
    }
    Ok(())
}

fn parse_host_compatibility(
    mut value: serde_json::Value,
) -> Result<PluginHostCompatibility, AdminError> {
    // 旧更新器要求 v1 与空权限字段，运行兼容性仍由清单、协议和能力版本表达
    if value.get("schema_version") == Some(&serde_json::json!(1))
        && value.get("permissions") == Some(&serde_json::json!([]))
    {
        let fields = value.as_object_mut().ok_or_else(invalid_manifest)?;
        fields.remove("permissions");
        fields.insert("schema_version".into(), serde_json::json!(2));
    }
    let compatibility: PluginHostCompatibility =
        serde_json::from_value(value).map_err(|_| invalid_manifest())?;
    compatibility
        .is_valid()
        .then_some(compatibility)
        .ok_or_else(invalid_manifest)
}

fn parse_manifest(bytes: &[u8]) -> Result<SealedReleaseManifest, AdminError> {
    if bytes.len() > MAXIMUM_MANIFEST_BYTES {
        return Err(invalid_manifest());
    }
    let manifest: SealedReleaseManifest =
        serde_json::from_slice(bytes).map_err(|_| invalid_manifest())?;
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION
        || !manifest.sealed
        || !valid_text(&manifest.gateway_version, MAXIMUM_VERSION_BYTES)
        || !valid_git_sha(&manifest.gateway_git_sha)
        || manifest.plugins.len() > MAXIMUM_PLUGINS
    {
        return Err(invalid_manifest());
    }
    let mut plugin_ids = BTreeSet::new();
    let mut archives = BTreeSet::new();
    for plugin in &manifest.plugins {
        if !crate::model::plugins::valid_plugin_id(&plugin.id)
            || !valid_text(&plugin.version, MAXIMUM_VERSION_BYTES)
            || !valid_text(&plugin.publisher, MAXIMUM_PUBLISHER_BYTES)
            || plugin.id.split_once('.').map(|(publisher, _)| publisher)
                != Some(plugin.publisher.as_str())
            || !valid_text(&plugin.stability, MAXIMUM_STABILITY_BYTES)
            || !plugin_ids.insert(plugin.id.as_str())
            || plugin.artifacts.is_empty()
            || plugin.artifacts.len() > MAXIMUM_ARTIFACTS_PER_PLUGIN
        {
            return Err(invalid_manifest());
        }
        let mut platforms = BTreeSet::new();
        for artifact in &plugin.artifacts {
            if !valid_artifact(plugin, artifact)
                || !platforms.insert((
                    artifact.release_os.as_str(),
                    artifact.release_architecture.as_str(),
                ))
                || !archives.insert(artifact.archive.as_str())
            {
                return Err(invalid_manifest());
            }
        }
    }
    Ok(manifest)
}

fn valid_artifact(plugin: &OfficialPlugin, artifact: &OfficialArtifact) -> bool {
    supported_platform(artifact).is_some()
        && valid_archive_name(&artifact.archive)
        && valid_sha256(&artifact.sha256)
        && artifact.archive
            == format!(
                "{}-{}-{}-{}.tar.gz",
                plugin.id, plugin.version, artifact.os, artifact.architecture
            )
}

fn supported_platform(artifact: &OfficialArtifact) -> Option<Platform> {
    SUPPORTED_PLATFORMS.iter().copied().find(|platform| {
        artifact.release_os == platform.release_os
            && artifact.release_architecture == platform.release_architecture
            && artifact.target == platform.target
            && artifact.os == platform.runtime_os
            && artifact.architecture == platform.runtime_architecture
    })
}

fn select_artifact(
    plugin: &OfficialPlugin,
    current: Platform,
) -> Result<OfficialArtifact, AdminError> {
    let mut matching = plugin.artifacts.iter().filter(|artifact| {
        artifact.release_os == current.release_os
            && artifact.release_architecture == current.release_architecture
            && artifact.target == current.target
            && artifact.os == current.runtime_os
            && artifact.architecture == current.runtime_architecture
    });
    let selected = matching.next().cloned().ok_or_else(invalid_manifest)?;
    if matching.next().is_some() {
        return Err(invalid_manifest());
    }
    Ok(selected)
}

fn validate_inspected(
    plugin: &OfficialPlugin,
    artifact: &OfficialArtifact,
    inspected: &InspectedPluginArtifact,
) -> Result<(), AdminError> {
    if inspected.metadata.plugin_id != plugin.id
        || inspected.metadata.publisher != plugin.publisher
        || inspected.metadata.version != plugin.version
        || inspected.metadata.sha256 != artifact.sha256
        || !inspected
            .metadata
            .platforms
            .iter()
            .any(|value| value == &format!("{}-{}", artifact.os, artifact.architecture))
    {
        return Err(AdminError::invalid("官方插件包身份与发行清单不一致"));
    }
    Ok(())
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_git_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_archive_name(value: &str) -> bool {
    value.len() <= MAXIMUM_ARCHIVE_NAME_BYTES
        && value.ends_with(".tar.gz")
        && Path::new(value).file_name().and_then(|name| name.to_str()) == Some(value)
        && Path::new(value).components().count() == 1
        && !value.chars().any(char::is_control)
}

fn map_read_error(error: OfficialPluginReleaseReadError) -> AdminError {
    match error.kind() {
        OfficialPluginReleaseReadErrorKind::Invalid
        | OfficialPluginReleaseReadErrorKind::NotFound => invalid_manifest(),
        OfficialPluginReleaseReadErrorKind::Unavailable => {
            AdminError::unavailable("官方插件发行物暂不可读取")
        }
    }
}

fn invalid_manifest() -> AdminError {
    AdminError::invalid("官方插件发行清单不合法或不属于当前宿主发行物")
}

fn current_platform() -> Option<Platform> {
    SUPPORTED_PLATFORMS.iter().copied().find(|platform| {
        platform.runtime_os == std::env::consts::OS
            && platform.runtime_architecture == std::env::consts::ARCH
            && target_environment_matches(platform.target)
    })
}

fn target_environment_matches(target: &str) -> bool {
    match target {
        "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu" => cfg!(target_env = "gnu"),
        "aarch64-apple-darwin" => cfg!(target_os = "macos"),
        _ => false,
    }
}

const SUPPORTED_PLATFORMS: &[Platform] = &[
    Platform {
        release_os: "linux",
        release_architecture: "amd64",
        target: "x86_64-unknown-linux-gnu",
        runtime_os: "linux",
        runtime_architecture: "x86_64",
    },
    Platform {
        release_os: "linux",
        release_architecture: "arm64",
        target: "aarch64-unknown-linux-gnu",
        runtime_os: "linux",
        runtime_architecture: "aarch64",
    },
    Platform {
        release_os: "darwin",
        release_architecture: "arm64",
        target: "aarch64-apple-darwin",
        runtime_os: "macos",
        runtime_architecture: "aarch64",
    },
];
