//! 插件私有状态迁移的排空、暂存提交与失败恢复编排

use super::{
    PluginsService,
    configuration::{apply_replacements, next_revision},
};
use crate::{
    model::{
        AdminError, MutationContext, Revision,
        plugins::{
            instances::{
                PluginInstance, PluginInstanceMutation, PluginInstanceReplacement,
                PluginInstanceSnapshot,
            },
            state::{PluginStateCommit, PluginStateConfiguration},
        },
    },
    use_case::{map_store_error, publish_committed},
};

impl PluginsService {
    pub(super) async fn configure_instance_with_state_transition(
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
        // 即使最终保持停用，迁移也必须由目标制品的受控候选执行
        let mut migration_instance = target.clone();
        migration_instance.enabled = true;
        snapshot.config_revision = migration_instance.revision;
        snapshot
            .instances
            .retain(|instance| instance.id != migration_instance.id);
        snapshot.instances.push(migration_instance);
        apply_replacements(&mut snapshot, replacements);
        let migration_prepared = self.preparation.prepare(snapshot).await?;

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
        // 迁移开始后的所有提交前失败都走同一恢复出口；提交成功后不再回滚旧配置
        let committed = async {
            self.preparation
                .migrate_state(&migration_prepared, transition.clone())
                .await?;
            let mut current = self
                .store
                .load_instances()
                .await
                .map_err(|error| map_store_error(error, "plugin"))?;
            if current.config_revision != expected_revision {
                return Err(AdminError::conflict("迁移期间插件配置已发生变化"));
            }
            let candidate_revision = next_revision(expected_revision)?;
            target.revision = candidate_revision;
            current.instances.retain(|item| item.id != target.id);
            current.instances.push(target.clone());
            current.config_revision = candidate_revision;
            apply_replacements(&mut current, replacements);
            let prepared = self.preparation.prepare(current.clone()).await?;
            let result = self
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
                .map_err(|error| map_store_error(error, "plugin"))?;
            Ok((current, prepared, result))
        }
        .await;
        let (current, prepared, result) = match committed {
            Ok(committed) => committed,
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
        self.publish_instance(current, prepared, &result, replacements)
            .await?;
        drop(migration_prepared);
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
            let prepared = self.preparation.prepare(snapshot.clone()).await?;
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
            self.publish_instance(snapshot, prepared, &mutation, &[])
                .await?;
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
}
