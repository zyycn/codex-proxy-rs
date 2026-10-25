//! 插件状态的激活、迁移与实例排空，事务事实由状态端口提交

use super::{PluginRuntime, runtime::shutdown_sessions};
use async_trait::async_trait;
use gateway_admin::{
    model::{
        AdminError, Revision,
        plugins::{
            instances::PluginInstance,
            state::{
                ApplyPluginStateMigration, PluginStateMigrationAction, PluginStateMigrationChange,
                PluginStateTransition,
            },
        },
    },
    ports::plugins::PluginStateStoreErrorKind,
};
use gateway_core::routing::extensions::ExtensionSetReference;
use gateway_plugin_sdk::{
    Stage,
    call::host::{
        StateMigrationChange, StateMigrationRecord, StateMigrationRequest, StateMigrationResult,
    },
};
use std::{
    sync::{Arc, Weak},
    time::Duration,
};

impl PluginRuntime {
    async fn run_state_migration(
        &self,
        prepared: &ExtensionSetReference,
        transition: PluginStateTransition,
    ) -> Result<(), AdminError> {
        let set = self.prepared_set(prepared).await?;
        let instance = set
            .sessions
            .iter()
            .find(|instance| {
                instance.instance_id == transition.instance_id
                    && instance.artifact_sha256 == transition.artifact_sha256
            })
            .ok_or_else(|| AdminError::conflict("状态迁移候选插件已失效"))?;
        for namespace in &transition.namespaces {
            let mut batches = 0usize;
            loop {
                batches += 1;
                if batches > 200 {
                    return Err(AdminError::invalid("插件状态迁移超过有界批次数"));
                }
                let batch = self
                    .state
                    .migration_batch(&transition.id, &namespace.namespace, 64)
                    .await
                    .map_err(map_state_admin_error)?;
                let expected_keys = batch
                    .records
                    .iter()
                    .map(|record| record.key.clone())
                    .collect::<Vec<_>>();
                if batch.records.is_empty() {
                    self.state
                        .apply_migration_batch(ApplyPluginStateMigration {
                            transition_id: transition.id.clone(),
                            namespace: namespace.namespace.clone(),
                            cursor: batch.cursor,
                            expected_keys,
                            changes: Vec::new(),
                        })
                        .await
                        .map_err(map_state_admin_error)?;
                    break;
                }
                let request = StateMigrationRequest {
                    namespace: namespace.namespace.clone(),
                    from_schema_version: namespace.from_schema_version,
                    to_schema_version: namespace.to_schema_version,
                    records: batch
                        .records
                        .iter()
                        .map(|record| StateMigrationRecord {
                            key: record.key.clone(),
                            value: record.value.clone(),
                            version: record.version,
                        })
                        .collect(),
                };
                let reply = instance
                    .session
                    .call(
                        "plugin.state.migrate",
                        instance.session.context(
                            Stage::Configuration,
                            instance.session.budget(Stage::Configuration),
                        ),
                        serde_json::json!({}),
                        serde_json::to_vec(&request)
                            .map_err(|_| AdminError::internal("状态迁移批次无法编码"))?,
                    )
                    .await
                    .map_err(|_| AdminError::invalid("插件状态迁移调用失败"))?;
                if reply.result != serde_json::json!({}) {
                    return Err(AdminError::invalid("插件状态迁移结果信封无效"));
                }
                let result: StateMigrationResult = serde_json::from_slice(&reply.payload)
                    .map_err(|_| AdminError::invalid("插件状态迁移结果无效"))?;
                if result.changes.len() != batch.records.len()
                    || result
                        .changes
                        .iter()
                        .zip(&expected_keys)
                        .any(|(change, expected)| change.key() != expected)
                {
                    return Err(AdminError::invalid("插件状态迁移结果未逐项对应源批次"));
                }
                let mut changes = Vec::with_capacity(result.changes.len());
                for (change, source) in result.changes.into_iter().zip(&batch.records) {
                    let (key, action) = match change {
                        StateMigrationChange::Keep { key } => {
                            instance
                                .private_state
                                .validate_value(&namespace.namespace, &source.value)?;
                            (key, PluginStateMigrationAction::Keep)
                        }
                        StateMigrationChange::Replace { key, value } => {
                            instance
                                .private_state
                                .validate_value(&namespace.namespace, &value)?;
                            (key, PluginStateMigrationAction::Replace(value))
                        }
                        StateMigrationChange::Delete { key } => {
                            (key, PluginStateMigrationAction::Delete)
                        }
                    };
                    changes.push(PluginStateMigrationChange { key, action });
                }
                self.state
                    .apply_migration_batch(ApplyPluginStateMigration {
                        transition_id: transition.id.clone(),
                        namespace: namespace.namespace.clone(),
                        cursor: batch.cursor,
                        expected_keys,
                        changes,
                    })
                    .await
                    .map_err(map_state_admin_error)?;
            }
        }
        Ok(())
    }
}
#[async_trait]
impl gateway_admin::ports::plugins::PluginStateLifecycle for PluginRuntime {
    async fn activate_state(
        &self,
        prepared: &ExtensionSetReference,
        instance: &PluginInstance,
    ) -> Result<(), AdminError> {
        if !instance.enabled {
            return Ok(());
        }
        let set = self.prepared_set(prepared).await?;
        let prepared = set
            .sessions
            .iter()
            .find(|prepared| {
                prepared.instance_id == instance.id
                    && prepared.artifact_sha256 == instance.artifact_sha256
            })
            .ok_or_else(|| AdminError::conflict("插件候选实例已失效"))?;
        let owner = self
            .state
            .load_owner(prepared.private_state.owner_request(instance))
            .await
            .map_err(map_state_admin_error)?
            .ok_or_else(|| AdminError::conflict("插件状态 fence 尚未提交"))?;
        prepared.private_state.activate(owner);
        Ok(())
    }

    async fn quiesce_instance(&self, instance_id: &str, artifact_sha256: &str, revision: Revision) {
        let mut sessions = Vec::new();
        for set in self
            .prepared
            .lock()
            .await
            .values()
            .filter_map(Weak::upgrade)
        {
            for prepared in &set.sessions {
                if prepared.instance_id == instance_id
                    && prepared.artifact_sha256 == artifact_sha256
                    && prepared.revision == revision
                    && !sessions
                        .iter()
                        .any(|session| Arc::ptr_eq(session, &prepared.session))
                {
                    sessions.push(prepared.session.clone());
                }
            }
        }
        shutdown_sessions(sessions, Duration::from_secs(2)).await;
    }

    async fn migrate_state(
        &self,
        prepared: &ExtensionSetReference,
        transition: PluginStateTransition,
    ) -> Result<(), AdminError> {
        tokio::select! {
            biased;
            () = self.shutting_down.cancelled() => Err(AdminError::unavailable("插件运行时正在关闭")),
            result = self.run_state_migration(prepared, transition) => result,
        }
    }
}
pub(super) fn map_state_admin_error(
    error: gateway_admin::ports::plugins::PluginStateStoreError,
) -> AdminError {
    match error.kind() {
        PluginStateStoreErrorKind::Invalid => AdminError::invalid("插件状态请求不合法"),
        PluginStateStoreErrorKind::NotFound => AdminError::not_found("插件状态迁移不存在"),
        PluginStateStoreErrorKind::Conflict => AdminError::conflict("插件状态版本或迁移游标冲突"),
        PluginStateStoreErrorKind::Quota => AdminError::conflict("插件状态超过声明配额"),
        PluginStateStoreErrorKind::PermissionDenied => {
            AdminError::conflict("插件状态 fence 已失效")
        }
        PluginStateStoreErrorKind::Unavailable => AdminError::unavailable("插件状态存储暂不可用"),
    }
}
