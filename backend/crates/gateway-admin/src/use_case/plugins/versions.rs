use super::{
    PluginsService,
    artifacts::{configuration_defaults, default_bindings},
};
use crate::{
    model::{
        AdminError, MutationContext,
        plugins::{
            PluginArtifactMetadata,
            instances::{
                ConfigurePluginInstance, PluginInstance, PluginInstanceMutation,
                PluginRollbackPlan, PluginRollbackTarget, PluginVersionConfiguration,
                PluginVersionPlan, RollbackPluginInstance,
            },
        },
    },
    use_case::map_store_error,
};

impl PluginsService {
    pub async fn rollback_instance(
        &self,
        id: &str,
        input: RollbackPluginInstance,
        context: &MutationContext,
    ) -> Result<PluginInstanceMutation, AdminError> {
        let snapshot = self
            .store
            .load_instances()
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        let existing = snapshot
            .instances
            .iter()
            .find(|instance| instance.id == id)
            .cloned()
            .ok_or_else(|| AdminError::not_found("插件实例不存在"))?;
        if existing.revision.get() != input.expected_revision {
            return Err(AdminError::conflict("插件实例已变更，请重新确认回滚目标"));
        }
        let artifacts = self.list().await?;
        let current = artifacts
            .iter()
            .find(|artifact| artifact.metadata.sha256 == existing.artifact_sha256)
            .ok_or_else(|| AdminError::not_found("当前插件制品不存在"))?;
        let target = artifacts
            .iter()
            .find(|artifact| artifact.metadata.sha256 == input.artifact_sha256)
            .ok_or_else(|| AdminError::not_found("回滚目标制品尚未安装"))?;
        if rollback_version(&current.metadata, &target.metadata)?.is_none() {
            return Err(AdminError::invalid("回滚目标必须是同一插件的较早版本"));
        }
        let saved = self
            .store
            .load_version_configuration(id, &input.artifact_sha256)
            .await
            .map_err(|error| map_store_error(error, "plugin"))?
            .ok_or_else(|| {
                AdminError::invalid("该版本没有可恢复的配置，请通过版本切换检查当前设置")
            })?;
        let configuration = ConfigurePluginInstance {
            creation_id: None,
            expected_revision: None,
            replace_instances: Vec::new(),
            name: existing.name.clone(),
            artifact_sha256: input.artifact_sha256,
            enabled: existing.enabled,
            configuration: saved.configuration,
            secrets: Some(saved.secrets),
            bindings: saved.bindings,
        };
        // 沿用同一份快照的 CAS 和状态迁移流程，不能在检查目标后重新读取并覆盖并发配置。
        self.configure_from_snapshot(snapshot, Some(existing), configuration, context)
            .await
    }

    pub async fn rollback_plan(&self, id: &str) -> Result<PluginRollbackPlan, AdminError> {
        let snapshot = self
            .store
            .load_instances()
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        let instance = snapshot
            .instances
            .iter()
            .find(|instance| instance.id == id)
            .ok_or_else(|| AdminError::not_found("插件实例不存在"))?;
        let artifacts = self.list().await?;
        let current = artifacts
            .iter()
            .find(|artifact| artifact.metadata.sha256 == instance.artifact_sha256)
            .ok_or_else(|| AdminError::not_found("当前插件制品不存在"))?;
        let saved_versions = self
            .store
            .configuration_versions(id)
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        let mut targets = Vec::new();
        for artifact in artifacts.iter().filter(|artifact| {
            artifact.accepted_at.is_some() && saved_versions.contains(&artifact.metadata.sha256)
        }) {
            if let Some(version) = rollback_version(&current.metadata, &artifact.metadata)? {
                targets.push((
                    version,
                    PluginRollbackTarget {
                        artifact_sha256: artifact.metadata.sha256.clone(),
                        version: artifact.metadata.version.clone(),
                        platforms: artifact.metadata.platforms.clone(),
                    },
                ));
            }
        }
        targets.sort_by(|(left, left_target), (right, right_target)| {
            right.cmp_precedence(left).then_with(|| {
                left_target
                    .artifact_sha256
                    .cmp(&right_target.artifact_sha256)
            })
        });
        Ok(PluginRollbackPlan {
            instance_revision: instance.revision.get(),
            current_version: current.metadata.version.clone(),
            targets: targets.into_iter().map(|(_, target)| target).collect(),
        })
    }

    pub async fn version_plan(
        &self,
        id: &str,
        digest: &str,
    ) -> Result<PluginVersionPlan, AdminError> {
        let snapshot = self
            .store
            .load_instances()
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        let instance = snapshot
            .instances
            .iter()
            .find(|instance| instance.id == id)
            .ok_or_else(|| AdminError::not_found("插件实例不存在"))?;
        let (configuration, restored) = self.version_configuration(instance, digest).await?;
        Ok(PluginVersionPlan {
            instance_revision: instance.revision.get(),
            artifact_sha256: digest.to_owned(),
            configuration: configuration.configuration,
            secret_fields: configuration.secrets.into_keys().collect(),
            bindings: configuration.bindings,
            restored,
        })
    }

    pub async fn switch_instance_version(
        &self,
        id: &str,
        input: RollbackPluginInstance,
        context: &MutationContext,
    ) -> Result<PluginInstanceMutation, AdminError> {
        let snapshot = self
            .store
            .load_instances()
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        let existing = snapshot
            .instances
            .iter()
            .find(|instance| instance.id == id)
            .cloned()
            .ok_or_else(|| AdminError::not_found("插件实例不存在"))?;
        if existing.revision.get() != input.expected_revision {
            return Err(AdminError::conflict("插件设置已变更，请刷新后重试"));
        }
        if existing.artifact_sha256 == input.artifact_sha256 {
            return Err(AdminError::invalid("已在使用此版本"));
        }
        let (configuration, _) = self
            .version_configuration(&existing, &input.artifact_sha256)
            .await?;
        let request = ConfigurePluginInstance {
            creation_id: None,
            expected_revision: Some(existing.revision.get()),
            replace_instances: Vec::new(),
            name: existing.name.clone(),
            artifact_sha256: input.artifact_sha256,
            enabled: existing.enabled,
            configuration: configuration.configuration,
            secrets: Some(configuration.secrets),
            bindings: configuration.bindings,
        };
        // 同一次读取的 CAS 覆盖草稿计算、准备与提交，不能覆盖并发设置。
        self.configure_from_snapshot(snapshot, Some(existing), request, context)
            .await
    }

    async fn version_configuration(
        &self,
        instance: &PluginInstance,
        digest: &str,
    ) -> Result<(PluginVersionConfiguration, bool), AdminError> {
        let artifacts = self
            .store
            .list_artifacts()
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        let current = artifacts
            .iter()
            .find(|artifact| artifact.metadata.sha256 == instance.artifact_sha256)
            .ok_or_else(|| AdminError::not_found("当前插件版本不存在"))?;
        let target = artifacts
            .iter()
            .find(|artifact| artifact.metadata.sha256 == digest && artifact.accepted_at.is_some())
            .ok_or_else(|| AdminError::not_found("目标插件版本尚未安装"))?;
        if current.metadata.plugin_id != target.metadata.plugin_id {
            return Err(AdminError::invalid("只能切换同一插件的版本"));
        }
        if digest != instance.artifact_sha256
            && let Some(saved) = self
                .store
                .load_version_configuration(&instance.id, digest)
                .await
                .map_err(|error| map_store_error(error, "plugin"))?
        {
            return Ok((saved, true));
        }
        let mut configuration = instance.configuration.clone();
        // 只补充缺失的默认值，保留显式值及未知字段，让不兼容项由校验反馈给用户。
        merge_missing_defaults(&mut configuration, configuration_defaults(&target.metadata));
        Ok((
            PluginVersionConfiguration {
                configuration,
                secrets: instance.secrets.clone(),
                bindings: version_bindings(instance, &current.metadata, &target.metadata),
            },
            false,
        ))
    }
}

fn version_bindings(
    instance: &PluginInstance,
    current: &PluginArtifactMetadata,
    target: &PluginArtifactMetadata,
) -> Vec<crate::model::plugins::instances::PluginCapabilityBinding> {
    let mut bindings = default_bindings(target);
    // 已关闭的既有能力继续关闭，新声明能力才采用包默认值。
    bindings.retain(|binding| {
        let capability = target
            .contributes
            .iter()
            .find(|(_, contribution)| contribution.id == binding.contribution)
            .map(|(capability, _)| capability);
        let previous = capability.and_then(|capability| current.contributes.get(capability));
        previous.is_none_or(|contribution| {
            !contribution.stages.contains(&binding.stage)
                || instance
                    .bindings
                    .iter()
                    .any(|old| old.contribution == contribution.id && old.stage == binding.stage)
        })
    });
    for previous in &instance.bindings {
        let Some((capability, _)) = current
            .contributes
            .iter()
            .find(|(_, contribution)| contribution.id == previous.contribution)
        else {
            continue;
        };
        let Some(contribution) = target
            .contributes
            .get(capability)
            .filter(|contribution| contribution.stages.contains(&previous.stage))
        else {
            continue;
        };
        let mut binding = previous.clone();
        binding.contribution.clone_from(&contribution.id);
        if let Some(default) = bindings.iter_mut().find(|default| {
            default.contribution == binding.contribution && default.stage == binding.stage
        }) {
            *default = binding;
        } else {
            bindings.push(binding);
        }
    }
    bindings
}

fn rollback_version(
    current: &PluginArtifactMetadata,
    target: &PluginArtifactMetadata,
) -> Result<Option<semver::Version>, AdminError> {
    if current.plugin_id != target.plugin_id {
        return Ok(None);
    }
    let current_version = semver::Version::parse(&current.version)
        .map_err(|_| AdminError::invalid("当前插件版本无效"))?;
    let target_version = semver::Version::parse(&target.version)
        .map_err(|_| AdminError::invalid("回滚目标版本无效"))?;
    Ok(target_version
        .cmp_precedence(&current_version)
        .is_lt()
        .then_some(target_version))
}

fn merge_missing_defaults(current: &mut serde_json::Value, defaults: serde_json::Value) {
    let (Some(current), serde_json::Value::Object(defaults)) = (current.as_object_mut(), defaults)
    else {
        return;
    };
    for (key, default) in defaults {
        if let Some(value) = current.get_mut(&key) {
            merge_missing_defaults(value, default);
        } else {
            current.insert(key, default);
        }
    }
}
