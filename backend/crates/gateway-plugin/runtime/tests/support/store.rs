//! 插件运行时测试使用的内存存储与状态迁移替身

use std::{collections::BTreeMap, sync::Mutex};

use async_trait::async_trait;
use gateway_admin::{
    model::{
        MutationContext, Revision,
        plugins::{
            InspectedPluginArtifact, InstalledPluginArtifact, PluginArtifactMutation, PluginSource,
            distribution::{PluginSourceBinding, SourceCredential, SourceCredentialInfo},
            instances::{PluginInstance, PluginInstanceMutation, PluginInstanceSnapshot},
            state::{
                ApplyPluginStateMigration, DeletePluginState, PluginStateConfiguration,
                PluginStateMigrationBatch, PluginStateOwner, PluginStateOwnerRequest,
                PluginStateRecord, PluginStateTransition, PluginStateWrite, PutPluginState,
            },
        },
    },
    ports::{
        plugins::{PluginStateStore, PluginStateStoreResult, PluginStore},
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
    },
};

pub struct Store {
    pub artifacts: BTreeMap<String, InspectedPluginArtifact>,
    pub snapshot: Mutex<PluginInstanceSnapshot>,
}

#[async_trait]
impl PluginStateStore for Store {
    async fn load_owner(
        &self,
        request: PluginStateOwnerRequest,
    ) -> PluginStateStoreResult<Option<PluginStateOwner>> {
        if !request.configuration.namespaces.is_empty() {
            return Ok(None);
        }
        Ok(Some(PluginStateOwner::from_store(
            request.instance_id,
            request.artifact_sha256,
            request.instance_revision,
            BTreeMap::new(),
        )))
    }

    async fn get(
        &self,
        _: &PluginStateOwner,
        _: &str,
        _: &str,
    ) -> PluginStateStoreResult<Option<PluginStateRecord>> {
        Err(gateway_admin::ports::plugins::PluginStateStoreError::new(
            gateway_admin::ports::plugins::PluginStateStoreErrorKind::Unavailable,
        ))
    }

    async fn put(
        &self,
        _: &PluginStateOwner,
        _: PutPluginState,
    ) -> PluginStateStoreResult<PluginStateWrite> {
        Err(gateway_admin::ports::plugins::PluginStateStoreError::new(
            gateway_admin::ports::plugins::PluginStateStoreErrorKind::Unavailable,
        ))
    }

    async fn delete(
        &self,
        _: &PluginStateOwner,
        _: DeletePluginState,
    ) -> PluginStateStoreResult<bool> {
        Err(gateway_admin::ports::plugins::PluginStateStoreError::new(
            gateway_admin::ports::plugins::PluginStateStoreErrorKind::Unavailable,
        ))
    }

    async fn transition_required(
        &self,
        _: &str,
        _: &PluginStateConfiguration,
    ) -> PluginStateStoreResult<bool> {
        Ok(false)
    }

    async fn begin_transition(
        &self,
        _: &str,
        _: Revision,
        _: &str,
        _: PluginStateConfiguration,
    ) -> PluginStateStoreResult<PluginStateTransition> {
        Err(gateway_admin::ports::plugins::PluginStateStoreError::new(
            gateway_admin::ports::plugins::PluginStateStoreErrorKind::Unavailable,
        ))
    }

    async fn migration_batch(
        &self,
        _: &str,
        _: &str,
        _: u32,
    ) -> PluginStateStoreResult<PluginStateMigrationBatch> {
        Err(gateway_admin::ports::plugins::PluginStateStoreError::new(
            gateway_admin::ports::plugins::PluginStateStoreErrorKind::Unavailable,
        ))
    }

    async fn apply_migration_batch(
        &self,
        _: ApplyPluginStateMigration,
    ) -> PluginStateStoreResult<()> {
        Err(gateway_admin::ports::plugins::PluginStateStoreError::new(
            gateway_admin::ports::plugins::PluginStateStoreErrorKind::Unavailable,
        ))
    }

    async fn abort_transition(&self, _: &str) -> PluginStateStoreResult<()> {
        Ok(())
    }
}

fn unused() -> AdminStoreError {
    AdminStoreError::new(
        AdminStoreErrorKind::Unavailable,
        "plugin",
        "unused test operation",
    )
}

#[async_trait]
impl PluginStore for Store {
    async fn management_target_is_current(
        &self,
        target: &gateway_admin::model::plugins::management::PluginManagementTarget,
    ) -> AdminStoreResult<bool> {
        Ok(self
            .load_instances()
            .await?
            .instances
            .iter()
            .any(|instance| {
                instance.enabled
                    && instance.trusted_process
                    && instance.id == target.instance_id
                    && instance.artifact_sha256 == target.artifact_sha256
                    && instance.revision.get() == target.revision
            }))
    }
    async fn accept_artifact(
        &self,
        _: &str,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        Err(unused())
    }
    async fn list_artifacts(&self) -> AdminStoreResult<Vec<InstalledPluginArtifact>> {
        Err(unused())
    }
    async fn load_artifact(&self, digest: &str) -> AdminStoreResult<InspectedPluginArtifact> {
        self.artifacts.get(digest).cloned().ok_or_else(unused)
    }
    async fn install_artifact(
        &self,
        _: InspectedPluginArtifact,
        _: PluginSource,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginArtifactMutation> {
        Err(unused())
    }
    async fn delete_artifact(&self, _: &str, _: &MutationContext) -> AdminStoreResult<Revision> {
        Err(unused())
    }
    async fn list_source_credentials(&self) -> AdminStoreResult<Vec<SourceCredentialInfo>> {
        Err(unused())
    }
    async fn load_source_credential(&self, _: &str) -> AdminStoreResult<SourceCredential> {
        Err(unused())
    }
    async fn save_source_credential(
        &self,
        _: SourceCredential,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unused())
    }
    async fn delete_source_credential(
        &self,
        _: &str,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unused())
    }
    async fn list_update_sources(&self) -> AdminStoreResult<Vec<PluginSourceBinding>> {
        Err(unused())
    }
    async fn change_update_source(
        &self,
        _: PluginSourceBinding,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unused())
    }
    async fn load_instances(&self) -> AdminStoreResult<PluginInstanceSnapshot> {
        Ok(self.snapshot.lock().unwrap().clone())
    }
    async fn save_instance(
        &self,
        mut instance: PluginInstance,
        expected: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<PluginInstanceMutation> {
        let mut snapshot = self.snapshot.lock().unwrap();
        if snapshot.config_revision != expected {
            return Err(AdminStoreError::new(
                AdminStoreErrorKind::Conflict,
                "plugin",
                "configuration changed",
            ));
        }
        let next = Revision::new(expected.get() + 1).unwrap();
        instance.revision = next;
        snapshot.config_revision = next;
        snapshot.instances.retain(|item| item.id != instance.id);
        snapshot.instances.push(instance.clone());
        Ok(PluginInstanceMutation {
            config_revision: next,
            instance,
        })
    }
    async fn delete_instance(
        &self,
        _: &str,
        _: Revision,
        _: &MutationContext,
    ) -> AdminStoreResult<Revision> {
        Err(unused())
    }
}
