//! 将插件包校验适配为制品元数据、图标与兼容性查询端口

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use gateway_admin::{
    model::{
        AdminError,
        plugins::{
            InspectedPluginArtifact, PluginArtifactIcon, PluginArtifactIconResource,
            PluginArtifactIconVariants, PluginArtifactMetadata, PluginContribution,
            PluginIconTheme,
        },
    },
    ports::plugins::PluginPackageInspector,
};

use super::{PackageError, PackageLimits, ValidatedPackage};

pub struct PackageInspector {
    limits: PackageLimits,
    host_version: semver::Version,
    slots: Arc<tokio::sync::Semaphore>,
}

impl PackageInspector {
    #[must_use]
    pub fn new(limits: PackageLimits, host_version: semver::Version) -> Self {
        Self {
            limits,
            host_version,
            slots: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }
}

#[async_trait]
impl PluginPackageInspector for PackageInspector {
    async fn inspect(
        &self,
        archive: Arc<[u8]>,
        expected_sha256: Option<String>,
    ) -> Result<InspectedPluginArtifact, AdminError> {
        let permit = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| AdminError::unavailable("插件包校验繁忙，请稍后重试"))?;
        let limits = self.limits;
        let host_version = self.host_version.clone();
        tokio::task::spawn_blocking(move || {
            // blocking 任务取消后仍可能运行，容量引用由任务持有直到真正结束
            let _permit = permit;
            let package = ValidatedPackage::read(archive, expected_sha256.as_deref(), limits)
                .map_err(inspection_error)?;
            let manifest = package.manifest();
            if !super::compatibility::supports(manifest)? {
                return Err(AdminError::invalid("插件能力与当前宿主不兼容"));
            }
            let target = manifest
                .package_for(&host_version, std::env::consts::OS, std::env::consts::ARCH)
                .map_err(|_| AdminError::invalid("插件不支持当前网关版本或平台"))?;
            let metadata = PluginArtifactMetadata {
                plugin_id: manifest
                    .plugin_id()
                    .map_err(|_| AdminError::invalid("插件身份不合法"))?,
                version: manifest.version.to_string(),
                name: manifest.name.clone(),
                display_name: manifest.display_name.clone(),
                publisher: manifest.publisher.clone(),
                author: manifest.author.clone(),
                description: manifest.description.clone(),
                license: manifest.license.clone(),
                sha256: package.digest().into(),
                platforms: vec![format!(
                    "{}-{}",
                    target.target.os, target.target.architecture
                )],
                icon: manifest.icon.as_ref().map(|icon| match icon {
                    gateway_plugin_sdk::PluginIcon::Path(path) => {
                        PluginArtifactIcon::Path(path.clone())
                    }
                    gateway_plugin_sdk::PluginIcon::Themed(variants) => {
                        PluginArtifactIcon::Themed(PluginArtifactIconVariants {
                            light: variants.light.clone(),
                            dark: variants.dark.clone(),
                        })
                    }
                }),
                contributes: manifest
                    .contributes
                    .iter()
                    .map(|(capability, declaration)| {
                        Ok((
                            identifier(*capability)?,
                            PluginContribution {
                                id: declaration.id.clone(),
                                version: declaration.version,
                                stages: declaration
                                    .stages
                                    .iter()
                                    .map(|stage| identifier(*stage))
                                    .collect::<Result<_, _>>()?,
                                input_formats: declaration.input_formats.clone(),
                                output_formats: declaration.output_formats.clone(),
                            },
                        ))
                    })
                    .collect::<Result<_, _>>()?,
                configuration_schema: manifest.configuration_schema.clone(),
                secret_fields: manifest.secret_fields.iter().cloned().collect(),
                state_namespaces: crate::callback::private_state::configuration(manifest)?
                    .namespaces,
            };
            Ok(InspectedPluginArtifact {
                metadata,
                archive: package.archive(),
            })
        })
        .await
        .map_err(|_| AdminError::internal("插件包校验任务失败"))?
    }

    async fn icon(
        &self,
        archive: Arc<[u8]>,
        expected_sha256: String,
        theme: PluginIconTheme,
    ) -> Result<Option<PluginArtifactIconResource>, AdminError> {
        let permit = Arc::clone(&self.slots).acquire_owned();
        let permit = tokio::time::timeout(Duration::from_secs(5), permit)
            .await
            .map_err(|_| AdminError::unavailable("插件图标读取繁忙，请稍后重试"))?
            .map_err(|_| AdminError::unavailable("插件包校验繁忙，请稍后重试"))?;
        let limits = self.limits;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let package = ValidatedPackage::read(archive, Some(&expected_sha256), limits)
                .map_err(icon_error)?;
            let Some(path) = super::icon::path(package.manifest(), theme) else {
                return Ok(None);
            };
            let content_type = package
                .manifest()
                .resources
                .get(path)
                .cloned()
                .ok_or_else(|| AdminError::invalid("插件图标资源声明无效"))?;
            let body = package
                .resource(path)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| AdminError::invalid("插件图标资源缺失"))?;
            Ok(Some(PluginArtifactIconResource { content_type, body }))
        })
        .await
        .map_err(|_| AdminError::internal("插件图标读取任务失败"))?
    }

    async fn compatibility(
        &self,
        archive: Arc<[u8]>,
        expected_sha256: String,
    ) -> Result<gateway_admin::model::plugins::PluginCompatibilityRequirements, AdminError> {
        let permit = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| AdminError::unavailable("插件包校验繁忙，请稍后重试"))?;
        let limits = self.limits;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let package = ValidatedPackage::read(archive, Some(&expected_sha256), limits)
                .map_err(inspection_error)?;
            super::compatibility::requirements(package.manifest())
        })
        .await
        .map_err(|_| AdminError::internal("插件兼容性检查任务失败"))?
    }
}

fn inspection_error(error: PackageError) -> AdminError {
    match error {
        PackageError::Manifest(gateway_plugin_sdk::ManifestError::Incompatible) => {
            AdminError::invalid("插件清单或协议版本不兼容，请使用当前 SDK 重新构建")
        }
        _ => AdminError::invalid("插件包格式、兼容性或摘要校验失败"),
    }
}

fn icon_error(error: PackageError) -> AdminError {
    tracing::warn!(error = ?error, "installed plugin icon validation failed");
    AdminError::unavailable("插件图标读取失败")
}

fn identifier(value: impl serde::Serialize) -> Result<String, AdminError> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| AdminError::internal("插件描述转换失败"))
}
