//! 插件实例的配置提交、发布、停用与删除用例

use std::collections::BTreeSet;

use super::{
    PluginsService,
    configuration::{apply_replacements, next_revision, same_creation_request, validate_input},
};
use crate::{
    model::{
        AdminError, MutationContext, Revision,
        plugins::{
            instances::{
                ConfigurePluginInstance, PluginInstance, PluginInstanceMutation,
                PluginInstanceSnapshot,
            },
            state::PluginStateCommit,
        },
    },
    use_case::{map_store_error, publish_committed},
};

impl PluginsService {
    pub async fn configure_instance(
        &self,
        id: Option<&str>,
        input: ConfigurePluginInstance,
        context: &MutationContext,
    ) -> Result<PluginInstanceMutation, AdminError> {
        if id.is_some() && input.creation_id.is_some() {
            return Err(AdminError::invalid("编辑配置不能使用创建标识"));
        }
        if id.is_none() && input.expected_revision.is_some() {
            return Err(AdminError::invalid("创建配置不能指定已有配置版本"));
        }
        if let Some(creation_id) = &input.creation_id {
            let parsed = uuid::Uuid::parse_str(creation_id)
                .map_err(|_| AdminError::invalid("配置创建标识必须是 UUID"))?;
            if parsed.to_string() != *creation_id {
                return Err(AdminError::invalid("配置创建标识必须使用标准 UUID 格式"));
            }
        }
        let snapshot = self
            .store
            .load_instances()
            .await
            .map_err(|e| map_store_error(e, "plugin"))?;
        let existing = match id {
            Some(id) => Some(
                snapshot
                    .instances
                    .iter()
                    .find(|instance| instance.id == id)
                    .cloned()
                    .ok_or_else(|| AdminError::not_found("插件实例不存在"))?,
            ),
            None => input.creation_id.as_ref().and_then(|creation_id| {
                snapshot
                    .instances
                    .iter()
                    .find(|instance| instance.id == *creation_id)
                    .cloned()
            }),
        };
        if let Some(existing) = &existing {
            if id.is_none() && !same_creation_request(existing, &input) {
                return Err(AdminError::conflict("此配置已创建，请加载最新配置后编辑"));
            }
            if input
                .expected_revision
                .is_some_and(|revision| revision != existing.revision.get())
            {
                return Err(AdminError::conflict("插件配置已变更，请加载最新配置后重试"));
            }
        }
        self.configure_from_snapshot(snapshot, existing, input, context)
            .await
    }

    pub(in crate::use_case::plugins) async fn configure_from_snapshot(
        &self,
        mut snapshot: PluginInstanceSnapshot,
        existing: Option<PluginInstance>,
        input: ConfigurePluginInstance,
        context: &MutationContext,
    ) -> Result<PluginInstanceMutation, AdminError> {
        let expected_revision = snapshot.config_revision;
        let candidate_revision = next_revision(expected_revision)?;
        let artifacts = self
            .store
            .list_artifacts()
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.metadata.sha256 == input.artifact_sha256)
            .ok_or_else(|| AdminError::not_found("插件制品不存在"))?;
        if artifact.accepted_at.is_none() {
            return Err(AdminError::invalid("请先安装并确认完整信任该版本"));
        }
        if input.enabled
            && let Some(warning) = self
                .package_status(&input.artifact_sha256)
                .await?
                .load_error
        {
            return Err(AdminError::invalid(format!(
                "{warning}，无法启动，请安装兼容版本"
            )));
        }
        let same_plugin = |digest: &str| {
            artifacts.iter().any(|item| {
                item.metadata.sha256 == digest
                    && item.metadata.plugin_id == artifact.metadata.plugin_id
            })
        };
        if existing
            .as_ref()
            .is_some_and(|previous| !same_plugin(&previous.artifact_sha256))
        {
            return Err(AdminError::invalid("不能将已有配置切换为另一个插件"));
        }
        if input.enabled
            && snapshot.instances.iter().any(|other| {
                other.enabled
                    && existing
                        .as_ref()
                        .is_none_or(|current| current.id != other.id)
                    && same_plugin(&other.artifact_sha256)
                    && !input
                        .replace_instances
                        .iter()
                        .any(|replacement| replacement.id == other.id)
            })
        {
            return Err(AdminError::conflict(
                "此插件已有启用配置，请先切换或停用旧配置",
            ));
        }
        let saved = if let Some(previous) = &existing
            && previous.artifact_sha256 != input.artifact_sha256
            && input.secrets.is_none()
        {
            self.store
                .load_version_configuration(&previous.id, &input.artifact_sha256)
                .await
                .map_err(|error| map_store_error(error, "plugin"))?
        } else {
            None
        };
        let instance = PluginInstance {
            id: existing.as_ref().map_or_else(
                || {
                    input
                        .creation_id
                        .clone()
                        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string())
                },
                |instance| instance.id.clone(),
            ),
            name: input.name.trim().to_owned(),
            artifact_sha256: input.artifact_sha256,
            enabled: input.enabled,
            trusted_process: true,
            configuration: input.configuration,
            secrets: input
                .secrets
                .or_else(|| saved.map(|saved| saved.secrets))
                .unwrap_or_else(|| {
                    existing
                        .as_ref()
                        .map_or_else(Default::default, |instance| instance.secrets.clone())
                }),
            bindings: input.bindings,
            revision: candidate_revision,
        };
        validate_input(&instance)?;
        if existing
            .as_ref()
            .is_some_and(|previous| previous.artifact_sha256 != instance.artifact_sha256)
            && !self
                .preparation
                .configuration_ready(instance.clone(), &artifact.metadata)
                .await?
        {
            return Err(AdminError::invalid(
                "目标版本仍缺少必填设置，请补充后再切换",
            ));
        }

        let replacements = input.replace_instances;
        if !replacements.is_empty() {
            if !instance.enabled || replacements.len() > 256 {
                return Err(AdminError::invalid(
                    "仅启用配置时可以切换其他配置，单次最多 256 项",
                ));
            }
            let mut ids = BTreeSet::new();
            for replacement in &replacements {
                if replacement.id == instance.id || !ids.insert(&replacement.id) {
                    return Err(AdminError::invalid("停用列表不能重复或包含当前配置"));
                }
                let previous = snapshot
                    .instances
                    .iter()
                    .find(|item| item.id == replacement.id)
                    .ok_or_else(|| AdminError::conflict("待停用配置已变更，请重新确认"))?;
                if !previous.enabled || previous.revision.get() != replacement.expected_revision {
                    return Err(AdminError::conflict("待停用配置已变更，请重新确认"));
                }
                if !artifacts.iter().any(|item| {
                    item.metadata.sha256 == previous.artifact_sha256
                        && item.metadata.plugin_id == artifact.metadata.plugin_id
                }) {
                    return Err(AdminError::invalid("只能切换同一插件的配置"));
                }
            }
        }
        let state = self.preparation.validate(instance.clone()).await?;
        if let Some(existing) = &existing
            && self.state.transition_required(&existing.id, &state).await?
        {
            return self
                .configure_instance_with_state_transition(
                    snapshot,
                    existing.clone(),
                    instance,
                    state,
                    &replacements,
                    context,
                )
                .await;
        }
        snapshot.instances.retain(|item| item.id != instance.id);
        snapshot.instances.push(instance.clone());
        snapshot.config_revision = candidate_revision;
        apply_replacements(&mut snapshot, &replacements);
        let prepared = self.preparation.prepare(snapshot.clone()).await?;
        let result = self
            .store
            .save_instance_replacing(
                instance,
                expected_revision,
                PluginStateCommit {
                    configuration: state,
                    transition_id: None,
                },
                &replacements,
                context,
            )
            .await
            .map_err(|e| map_store_error(e, "plugin"))?;
        self.publish_instance(snapshot, prepared, &result, &replacements)
            .await?;
        Ok(result)
    }

    pub async fn delete_instance(
        &self,
        id: &str,
        context: &MutationContext,
    ) -> Result<Revision, AdminError> {
        let snapshot = self
            .store
            .load_instances()
            .await
            .map_err(|e| map_store_error(e, "plugin"))?;
        let instance = snapshot
            .instances
            .iter()
            .find(|instance| instance.id == id)
            .ok_or_else(|| AdminError::not_found("插件实例不存在"))?;
        if instance.enabled {
            return Err(AdminError::conflict("请先停用插件实例"));
        }
        let revision = self
            .store
            .delete_instance(id, snapshot.config_revision, context)
            .await
            .map_err(|e| map_store_error(e, "plugin"))?;
        publish_committed(self.snapshots.as_ref(), revision).await?;
        Ok(revision)
    }

    /// 紧急管理修复不要求损坏插件成功准备；提交后沿用现有发布与暂停合同
    pub async fn disable_instance(
        &self,
        id: &str,
        context: &MutationContext,
    ) -> Result<PluginInstanceMutation, AdminError> {
        let PluginInstanceSnapshot {
            config_revision,
            instances,
        } = self
            .store
            .load_instances()
            .await
            .map_err(|e| map_store_error(e, "plugin"))?;
        let mut instance = instances
            .into_iter()
            .find(|instance| instance.id == id)
            .ok_or_else(|| AdminError::not_found("插件实例不存在"))?;
        instance.enabled = false;
        let result = self
            .store
            .save_instance(instance, config_revision, context)
            .await
            .map_err(|e| map_store_error(e, "plugin"))?;
        publish_committed(self.snapshots.as_ref(), result.config_revision).await?;
        Ok(result)
    }
}
