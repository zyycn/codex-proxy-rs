//! 系统升级与回滚前的插件兼容性检查及确认版本校验

use std::{collections::BTreeMap, sync::Arc};

use crate::model::system::{SystemIncompatiblePlugin, SystemRestartPlan};
use crate::ports::system::SystemRestartPreflight;
use async_trait::async_trait;
use sha2::{Digest as _, Sha256};

use crate::{
    model::{AdminErrorKind, Revision},
    ports::{
        plugins::{PluginPackageInspector, PluginStore},
        store::AdminStoreErrorKind,
        system::{
            SystemOperationError, SystemOperationErrorKind, SystemUpdateCandidate,
            SystemUpdatePreflight,
        },
    },
};

use super::plugins::official::{update_compatibility, validate_update_release};

/// 只读取启用实例及其固定包体，检查目标宿主合同；不准备或执行插件
pub(crate) struct PluginSystemUpdatePreflight {
    store: Arc<dyn PluginStore>,
    inspector: Arc<dyn PluginPackageInspector>,
}

impl PluginSystemUpdatePreflight {
    pub(crate) fn new(
        store: Arc<dyn PluginStore>,
        inspector: Arc<dyn PluginPackageInspector>,
    ) -> Self {
        Self { store, inspector }
    }
}

#[async_trait]
impl SystemUpdatePreflight for PluginSystemUpdatePreflight {
    async fn validate(
        &self,
        candidate: SystemUpdateCandidate,
    ) -> Result<Revision, SystemOperationError> {
        let target_version = candidate.target_version.trim_start_matches('v');
        semver::Version::parse(target_version).map_err(|_| invalid("目标网关版本不合法"))?;
        // 下载阶段只校验发行身份；插件兼容性在用户点击重启时检查并确认
        validate_update_release(candidate.release_manifest.as_ref(), target_version)
            .map_err(|_| invalid("目标发行清单不合法或版本不匹配"))?;
        Ok(self
            .store
            .load_instances()
            .await
            .map_err(map_store_error)?
            .config_revision)
    }

    async fn validate_rollback(
        &self,
        candidate: SystemUpdateCandidate,
    ) -> Result<Revision, SystemOperationError> {
        self.validate(candidate).await
    }

    async fn confirm_revision(&self, expected: Revision) -> Result<(), SystemOperationError> {
        let current = self
            .store
            .load_instances()
            .await
            .map_err(map_store_error)?
            .config_revision;
        if current != expected {
            return Err(conflict("插件配置已变化，请重新执行系统更新"));
        }
        Ok(())
    }
}

fn map_store_error(error: crate::ports::store::AdminStoreError) -> SystemOperationError {
    match error.kind() {
        AdminStoreErrorKind::StaleRevision
        | AdminStoreErrorKind::DuplicateName
        | AdminStoreErrorKind::Conflict => conflict("插件配置已变化，请重新执行系统更新"),
        AdminStoreErrorKind::Invalid | AdminStoreErrorKind::NotFound => {
            conflict("启用插件的制品不完整，无法执行系统更新")
        }
        AdminStoreErrorKind::Unavailable => internal("插件配置暂不可读取"),
    }
}

fn map_inspection_error(error: crate::model::AdminError) -> SystemOperationError {
    match error.kind() {
        AdminErrorKind::Invalid | AdminErrorKind::Conflict | AdminErrorKind::NotFound => {
            conflict("启用插件的制品无法通过兼容性检查")
        }
        _ => internal("插件兼容性检查暂不可用"),
    }
}

fn invalid(message: impl Into<String>) -> SystemOperationError {
    SystemOperationError::new(SystemOperationErrorKind::Invalid, message)
}

fn conflict(message: impl Into<String>) -> SystemOperationError {
    SystemOperationError::new(SystemOperationErrorKind::Conflict, message)
}

fn internal(message: impl Into<String>) -> SystemOperationError {
    SystemOperationError::new(SystemOperationErrorKind::Internal, message)
}

impl PluginSystemUpdatePreflight {
    pub(crate) async fn plan(
        &self,
        candidate: Option<SystemUpdateCandidate>,
    ) -> Result<SystemRestartPlan, SystemOperationError> {
        let snapshot = self.store.load_instances().await.map_err(map_store_error)?;
        let mut plan = SystemRestartPlan {
            target_version: candidate.as_ref().map(|item| item.target_version.clone()),
            release_manifest_sha256: candidate
                .as_ref()
                .map(|item| hex::encode(Sha256::digest(&item.release_manifest))),
            config_revision: snapshot.config_revision.get(),
            incompatible_plugins: Vec::new(),
        };
        let target = candidate
            .as_ref()
            .map(|item| semver::Version::parse(item.target_version.trim_start_matches('v')))
            .transpose()
            .map_err(|_| invalid("目标网关版本不合法"))?;
        if let Some(candidate) = &candidate {
            validate_update_release(
                &candidate.release_manifest,
                candidate.target_version.trim_start_matches('v'),
            )
            .map_err(|_| invalid("目标发行清单不合法或版本不匹配"))?;
        }
        // 未知目标合同不能证明兼容，列出风险供确认，不改写插件启用配置
        let host = candidate.as_ref().and_then(|item| {
            update_compatibility(
                &item.release_manifest,
                item.target_version.trim_start_matches('v'),
            )
            .ok()
        });
        let mut inspected: BTreeMap<String, Option<String>> = BTreeMap::new();
        for instance in snapshot.instances.iter().filter(|item| item.enabled) {
            let reason = if let Some(reason) = inspected.get(&instance.artifact_sha256) {
                reason.clone()
            } else {
                let artifact = self
                    .store
                    .load_artifact(&instance.artifact_sha256)
                    .await
                    .map_err(map_store_error)?;
                let reason = if let Some(target) = &target {
                    match (
                        &host,
                        self.inspector
                            .compatibility(artifact.archive, instance.artifact_sha256.clone())
                            .await,
                    ) {
                        (_, Err(error)) if error.kind() == AdminErrorKind::Invalid => {
                            Some("插件包或协议与目标版本不兼容".to_owned())
                        }
                        (_, Err(error)) => return Err(map_inspection_error(error)),
                        (None, _) => Some("无法确认插件与目标版本兼容".to_owned()),
                        (Some(host), Ok(requirements)) => {
                            if !semver::VersionReq::parse(&requirements.host_version)
                                .is_ok_and(|versions| versions.matches(target))
                            {
                                Some(format!("要求网关版本 {}", requirements.host_version))
                            } else if !host
                                .manifest_schema_versions
                                .contains(&requirements.manifest_schema_version)
                                || !host
                                    .protocol_versions
                                    .contains(&requirements.protocol_version)
                                || requirements
                                    .capabilities
                                    .iter()
                                    .any(|(capability, version)| {
                                        !host.supports_capability(capability, *version)
                                    })
                            {
                                Some("插件协议或能力与目标版本不兼容".to_owned())
                            } else {
                                None
                            }
                        }
                    }
                } else {
                    match self
                        .inspector
                        .inspect(
                            artifact.archive.clone(),
                            Some(instance.artifact_sha256.clone()),
                        )
                        .await
                    {
                        Ok(_) => self
                            .inspector
                            .compatibility_warning(
                                artifact.archive,
                                instance.artifact_sha256.clone(),
                            )
                            .await
                            .map_err(map_inspection_error)?,
                        Err(error) if error.kind() == AdminErrorKind::Invalid => {
                            Some("插件与当前宿主不兼容".to_owned())
                        }
                        Err(error) => return Err(map_inspection_error(error)),
                    }
                };
                inspected.insert(instance.artifact_sha256.clone(), reason.clone());
                reason
            };
            if let Some(reason) = reason {
                plan.incompatible_plugins.push(SystemIncompatiblePlugin {
                    instance_id: instance.id.clone(),
                    name: instance.name.clone(),
                    reason,
                });
            }
        }
        plan.incompatible_plugins
            .sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
        Ok(plan)
    }
}

pub(crate) struct ConfirmedPluginRestart {
    pub preflight: Arc<PluginSystemUpdatePreflight>,
    pub confirmation: Option<SystemRestartPlan>,
}

#[async_trait]
impl SystemRestartPreflight for ConfirmedPluginRestart {
    async fn prepare(
        &self,
        candidate: Option<SystemUpdateCandidate>,
    ) -> Result<(), SystemOperationError> {
        let plan = self.preflight.plan(candidate).await?;
        if self
            .confirmation
            .as_ref()
            .is_some_and(|confirmed| confirmed != &plan)
        {
            return Err(conflict("插件配置或目标版本已变化，请重新检查并确认重启"));
        }
        if !plan.incompatible_plugins.is_empty() && self.confirmation.is_none() {
            return Err(conflict("请先确认插件兼容性风险，再重启服务"));
        }
        let revision =
            Revision::new(plan.config_revision).map_err(|_| internal("插件配置版本不合法"))?;
        self.preflight.confirm_revision(revision).await
    }
}
