use std::collections::{BTreeMap, BTreeSet};

use gateway_core::{policy::ClientApiKeyId, routing::AccountGroupId};
use secrecy::ExposeSecret;

use super::PluginsService;
use crate::{
    model::{
        AdminError, MutationContext, Revision,
        plugins::instances::{
            ConfigurePluginInstance, PluginInstance, PluginInstanceMutation,
            PluginInstanceReplacement, PluginInstanceRuntime, PluginInstanceRuntimeStatus,
            PluginInstanceSnapshot, PluginInstanceView, PluginPermissionGrant,
        },
        plugins::state::{PluginStateCommit, PluginStateConfiguration},
    },
    use_case::{map_store_error, publish_committed},
};

impl PluginsService {
    pub async fn instances(&self) -> Result<Vec<PluginInstanceView>, AdminError> {
        let snapshot = self
            .store
            .load_instances()
            .await
            .map_err(|e| map_store_error(e, "plugin"))?;
        let published = self.published.snapshot_for_diagnostics();
        let published_revision = published.as_ref().map(|snapshot| snapshot.revision().get());
        let published_ready = published
            .as_ref()
            .is_some_and(|snapshot| snapshot.extensions().is_none_or(|set| set.can_serve()));
        let mut diagnostics = self
            .preparation
            .runtime_diagnostics(
                &snapshot,
                published_revision,
                published
                    .as_ref()
                    .and_then(|snapshot| snapshot.extensions()),
            )
            .await
            .unwrap_or_default();
        let target_revision = snapshot.config_revision.get();
        let artifacts = self
            .list()
            .await?
            .into_iter()
            .map(|artifact| (artifact.metadata.sha256.clone(), artifact.metadata))
            .collect::<BTreeMap<_, _>>();
        let mut views = Vec::with_capacity(snapshot.instances.len());
        for instance in snapshot.instances {
            let metadata = artifacts
                .get(&instance.artifact_sha256)
                .ok_or_else(|| AdminError::not_found("插件制品不存在"))?;
            let configuration_required = !self
                .preparation
                .configuration_ready(instance.clone(), metadata)
                .await?;
            let runtime = diagnostics.remove(&instance.id).unwrap_or_else(|| {
                let running = instance.enabled
                    && published_ready
                    && published_revision == Some(target_revision);
                PluginInstanceRuntime {
                    status: if !instance.enabled {
                        PluginInstanceRuntimeStatus::Disabled
                    } else if running {
                        PluginInstanceRuntimeStatus::Running
                    } else {
                        PluginInstanceRuntimeStatus::AwaitingPublication
                    },
                    actual_revision: running.then_some(instance.revision.get()),
                    actual_artifact_sha256: running.then(|| instance.artifact_sha256.clone()),
                    failure: None,
                    draining_revisions: Vec::new(),
                    retained_continuations: 0,
                }
            });
            views.push(PluginInstanceView {
                configuration_required,
                running: runtime.status == PluginInstanceRuntimeStatus::Running,
                published_revision,
                runtime,
                instance,
            });
        }
        Ok(views)
    }

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

    pub(super) async fn configure_from_snapshot(
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
            return Err(AdminError::invalid("请先安装并接受该版本声明的权限"));
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
            grants: artifact
                .metadata
                .requested_permissions
                .iter()
                .cloned()
                .map(|permission| PluginPermissionGrant { permission })
                .collect(),
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
        let prepared = self
            .preparation
            .prepare(expected_revision, snapshot.clone())
            .await?;
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
        let prepared = if result.config_revision == candidate_revision
            && result.instance.revision == candidate_revision
        {
            prepared
        } else {
            snapshot.config_revision = result.config_revision;
            snapshot
                .instances
                .retain(|item| item.id != result.instance.id);
            snapshot.instances.push(result.instance.clone());
            apply_replacements(&mut snapshot, &replacements);
            self.preparation
                .prepare(result.config_revision, snapshot)
                .await?
        };
        self.preparation
            .activate_state(&prepared, &result.instance)
            .await?;
        // prepared 的强引用跨过提交与唯一发布入口，防止候选在被读取之前回收。
        publish_committed(self.snapshots.as_ref(), result.config_revision).await?;
        drop(prepared);
        Ok(result)
    }

    async fn configure_instance_with_state_transition(
        &self,
        mut snapshot: PluginInstanceSnapshot,
        previous: PluginInstance,
        mut target: PluginInstance,
        target_state: PluginStateConfiguration,
        replacements: &[PluginInstanceReplacement],
        context: &MutationContext,
    ) -> Result<PluginInstanceMutation, AdminError> {
        let previous_state = self.preparation.validate(previous.clone()).await?;
        let original_revision = snapshot.config_revision;
        // 即使最终保持停用，迁移也必须由目标制品的受控候选执行。
        let mut migration_instance = target.clone();
        migration_instance.enabled = true;
        snapshot.config_revision = migration_instance.revision;
        snapshot
            .instances
            .retain(|instance| instance.id != migration_instance.id);
        snapshot.instances.push(migration_instance);
        apply_replacements(&mut snapshot, replacements);
        let migration_prepared = self
            .preparation
            .prepare(original_revision, snapshot)
            .await?;

        let mut disabled_revision = None;
        let disabled = if previous.enabled {
            let mut disabled = previous.clone();
            disabled.enabled = false;
            let mutation = self
                .store
                .save_instance_with_state(
                    disabled,
                    original_revision,
                    PluginStateCommit {
                        configuration: previous_state.clone(),
                        transition_id: None,
                    },
                    context,
                )
                .await
                .map_err(|error| map_store_error(error, "plugin"))?;
            publish_committed(self.snapshots.as_ref(), mutation.config_revision).await?;
            self.preparation
                .quiesce_instance(&previous.id, &previous.artifact_sha256, previous.revision)
                .await;
            disabled_revision = Some(mutation.config_revision);
            mutation.instance
        } else {
            previous.clone()
        };
        let expected_revision = disabled_revision.unwrap_or(original_revision);
        let transition = match self
            .state
            .begin_transition(
                &disabled.id,
                disabled.revision,
                &target.artifact_sha256,
                target_state.clone(),
            )
            .await
        {
            Ok(transition) => transition,
            Err(error) => {
                return Err(self
                    .recover_failed_state_transition(
                        None,
                        &previous,
                        &previous_state,
                        disabled_revision,
                        context,
                        error,
                    )
                    .await);
            }
        };
        if let Err(error) = self
            .preparation
            .migrate_state(&migration_prepared, transition.clone())
            .await
        {
            return Err(self
                .recover_failed_state_transition(
                    Some(&transition.id),
                    &previous,
                    &previous_state,
                    disabled_revision,
                    context,
                    error,
                )
                .await);
        }

        let mut current = self
            .store
            .load_instances()
            .await
            .map_err(|error| map_store_error(error, "plugin"))?;
        if current.config_revision != expected_revision {
            return Err(self
                .recover_failed_state_transition(
                    Some(&transition.id),
                    &previous,
                    &previous_state,
                    disabled_revision,
                    context,
                    AdminError::conflict("迁移期间插件配置已发生变化"),
                )
                .await);
        }
        let candidate_revision = next_revision(expected_revision)?;
        target.revision = candidate_revision;
        current.instances.retain(|item| item.id != target.id);
        current.instances.push(target.clone());
        current.config_revision = candidate_revision;
        apply_replacements(&mut current, replacements);
        let prepared = match self
            .preparation
            .prepare(expected_revision, current.clone())
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                return Err(self
                    .recover_failed_state_transition(
                        Some(&transition.id),
                        &previous,
                        &previous_state,
                        disabled_revision,
                        context,
                        error,
                    )
                    .await);
            }
        };
        let result = match self
            .store
            .save_instance_replacing(
                target,
                expected_revision,
                PluginStateCommit {
                    configuration: target_state,
                    transition_id: Some(transition.id.clone()),
                },
                replacements,
                context,
            )
            .await
        {
            Ok(result) => result,
            Err(error) => {
                return Err(self
                    .recover_failed_state_transition(
                        Some(&transition.id),
                        &previous,
                        &previous_state,
                        disabled_revision,
                        context,
                        map_store_error(error, "plugin"),
                    )
                    .await);
            }
        };
        let prepared = if result.config_revision == candidate_revision
            && result.instance.revision == candidate_revision
        {
            prepared
        } else {
            current.config_revision = result.config_revision;
            current
                .instances
                .retain(|item| item.id != result.instance.id);
            current.instances.push(result.instance.clone());
            apply_replacements(&mut current, replacements);
            self.preparation
                .prepare(result.config_revision, current)
                .await?
        };
        self.preparation
            .activate_state(&prepared, &result.instance)
            .await?;
        publish_committed(self.snapshots.as_ref(), result.config_revision).await?;
        drop(migration_prepared);
        drop(prepared);
        Ok(result)
    }

    async fn recover_failed_state_transition(
        &self,
        transition_id: Option<&str>,
        previous: &PluginInstance,
        previous_state: &PluginStateConfiguration,
        disabled_revision: Option<Revision>,
        context: &MutationContext,
        original_error: AdminError,
    ) -> AdminError {
        if let Some(transition_id) = transition_id {
            self.state.abort_transition(transition_id).await;
        }
        let Some(disabled_revision) = disabled_revision else {
            return original_error;
        };
        let restored = async {
            let mut snapshot = self
                .store
                .load_instances()
                .await
                .map_err(|error| map_store_error(error, "plugin"))?;
            if snapshot.config_revision != disabled_revision {
                return Err(AdminError::conflict(
                    "状态迁移失败；实例已保持停用，且配置已被其他操作修改",
                ));
            }
            let candidate_revision = next_revision(disabled_revision)?;
            let mut restore = previous.clone();
            restore.enabled = true;
            restore.revision = candidate_revision;
            snapshot.instances.retain(|item| item.id != restore.id);
            snapshot.instances.push(restore.clone());
            snapshot.config_revision = candidate_revision;
            let prepared = self
                .preparation
                .prepare(disabled_revision, snapshot.clone())
                .await?;
            let mutation = self
                .store
                .save_instance_with_state(
                    restore,
                    disabled_revision,
                    PluginStateCommit {
                        configuration: previous_state.clone(),
                        transition_id: None,
                    },
                    context,
                )
                .await
                .map_err(|error| map_store_error(error, "plugin"))?;
            let prepared = if mutation.config_revision == candidate_revision
                && mutation.instance.revision == candidate_revision
            {
                prepared
            } else {
                snapshot.config_revision = mutation.config_revision;
                snapshot
                    .instances
                    .retain(|item| item.id != mutation.instance.id);
                snapshot.instances.push(mutation.instance.clone());
                self.preparation
                    .prepare(mutation.config_revision, snapshot)
                    .await?
            };
            self.preparation
                .activate_state(&prepared, &mutation.instance)
                .await?;
            publish_committed(self.snapshots.as_ref(), mutation.config_revision).await?;
            Ok::<(), AdminError>(())
        }
        .await;
        match restored {
            Ok(()) => original_error,
            Err(error) => {
                tracing::warn!(
                    error_kind = ?error.kind(),
                    "plugin state migration failed and the previous instance could not be restored"
                );
                AdminError::conflict("状态迁移失败；为避免覆盖并发配置，插件实例已保持停用")
            }
        }
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

    /// 紧急管理修复不要求损坏插件成功准备；提交后沿用现有发布与暂停合同。
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

fn apply_replacements(
    snapshot: &mut PluginInstanceSnapshot,
    replacements: &[PluginInstanceReplacement],
) {
    for instance in &mut snapshot.instances {
        if replacements.iter().any(|item| item.id == instance.id) {
            instance.enabled = false;
            instance.revision = snapshot.config_revision;
        }
    }
}

fn same_creation_request(existing: &PluginInstance, input: &ConfigurePluginInstance) -> bool {
    let same_secrets = input
        .secrets
        .as_ref()
        .map_or(existing.secrets.is_empty(), |secrets| {
            secrets.len() == existing.secrets.len()
                && secrets.iter().all(|(key, value)| {
                    existing
                        .secrets
                        .get(key)
                        .is_some_and(|current| current.expose_secret() == value.expose_secret())
                })
        });
    existing.name == input.name.trim()
        && existing.artifact_sha256 == input.artifact_sha256
        && existing.enabled == input.enabled
        && existing.configuration == input.configuration
        && existing.bindings == input.bindings
        && same_secrets
}

fn validate_input(instance: &PluginInstance) -> Result<(), AdminError> {
    if instance.name.is_empty()
        || instance.name.len() > 128
        || instance.name.chars().any(char::is_control)
        || instance.artifact_sha256.len() != 64
        || !instance
            .artifact_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !instance.configuration.is_object()
        || serde_json::to_vec(&instance.configuration).map_or(true, |bytes| bytes.len() > 48 * 1024)
        || instance.secrets.len() > 64
        || instance.grants.len() > 32
        || instance.bindings.len() > 64
    {
        return Err(AdminError::invalid("插件实例配置不合法或超过大小限制"));
    }
    use secrecy::ExposeSecret as _;
    if instance
        .secrets
        .iter()
        .any(|(key, value)| key.len() > 64 || value.expose_secret().len() > 8192)
        || instance
            .secrets
            .values()
            .map(|value| value.expose_secret().len())
            .sum::<usize>()
            > 48 * 1024
    {
        return Err(AdminError::invalid("插件敏感配置超过大小限制"));
    }
    if instance.bindings.iter().any(|binding| {
        binding.contribution.is_empty()
            || binding.contribution.len() > 128
            || !binding
                .contribution
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
            || binding.client_key_ids.len() > 256
            || binding.account_group_ids.len() > 256
            || binding.client_key_ids.iter().collect::<BTreeSet<_>>().len()
                != binding.client_key_ids.len()
            || binding
                .account_group_ids
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != binding.account_group_ids.len()
            || binding
                .client_key_ids
                .iter()
                .any(|id| ClientApiKeyId::new(id.clone()).is_err())
            || binding
                .account_group_ids
                .iter()
                .any(|id| AccountGroupId::new(id.clone()).is_err())
            || binding.identity_bindings.len() > 256
            || binding
                .identity_bindings
                .iter()
                .map(|identity| &identity.principal)
                .collect::<BTreeSet<_>>()
                .len()
                != binding.identity_bindings.len()
            || binding.identity_bindings.iter().any(|identity| {
                identity.principal.trim() != identity.principal
                    || identity.principal.is_empty()
                    || identity.principal.len() > 256
                    || identity.principal.chars().any(char::is_control)
                    || ClientApiKeyId::new(identity.client_key_id.clone()).is_err()
            })
    }) {
        return Err(AdminError::invalid("插件绑定的范围或入口认证身份不合法"));
    }
    Ok(())
}

fn next_revision(revision: Revision) -> Result<Revision, AdminError> {
    Revision::new(
        revision
            .get()
            .checked_add(1)
            .ok_or_else(|| AdminError::internal("配置版本已耗尽"))?,
    )
    .map_err(|_| AdminError::internal("配置版本不合法"))
}
