use crate::{
    model::{
        AdminError, MutationContext,
        account_groups::{CreateAccountGroup, NewAccountGroup},
        client_keys::NewClientKey,
        plugin_resources::{
            GroupMembersChange, GroupMembersChanged, ManagedKeyConfig, ManagedResource,
            PluginResourceOwner, ResourceMutation,
        },
    },
    ports::plugin_resources::{PluginResourceAccess, PluginResourceStore},
};
use async_trait::async_trait;
use gateway_core::{policy::ClientApiKeyId, routing::AccountGroupId, runtime::SnapshotControl};
use std::sync::Arc;
use uuid::Uuid;

pub(crate) struct DefaultPluginResourceAccess {
    pub store: Arc<dyn PluginResourceStore>,
    pub snapshot: Arc<dyn SnapshotControl>,
}

impl DefaultPluginResourceAccess {
    async fn publish<T>(
        &self,
        result: crate::ports::store::AdminStoreResult<ResourceMutation<T>>,
    ) -> Result<T, AdminError> {
        let result = result.map_err(|error| super::map_store_error(error, "plugin resource"))?;
        if let Some(revision) = result.revision {
            super::publish_committed(self.snapshot.as_ref(), revision).await?;
        }
        Ok(result.value)
    }
}

#[async_trait]
impl PluginResourceAccess for DefaultPluginResourceAccess {
    async fn ensure_group(
        &self,
        owner: &PluginResourceOwner,
        resource_key: String,
        command: CreateAccountGroup,
        context: &MutationContext,
    ) -> Result<ManagedResource, AdminError> {
        let id = AccountGroupId::new(format!("grp_{}", Uuid::now_v7().simple()))
            .map_err(|_| AdminError::internal("创建账号组 ID 失败"))?;
        self.publish(
            self.store
                .ensure_group(
                    owner,
                    resource_key,
                    NewAccountGroup {
                        id,
                        name: command.name,
                        description: command.description,
                        color: command.color,
                        disable_fast: command.disable_fast,
                    },
                    context,
                )
                .await,
        )
        .await
    }

    async fn ensure_key(
        &self,
        owner: &PluginResourceOwner,
        resource_key: String,
        groups: Vec<String>,
        command: ManagedKeyConfig,
        context: &MutationContext,
    ) -> Result<ManagedResource, AdminError> {
        let id = ClientApiKeyId::new(format!("key_{}", Uuid::now_v7().simple()))
            .map_err(|_| AdminError::internal("创建 Key ID 失败"))?;
        self.publish(
            self.store
                .ensure_key(
                    owner,
                    resource_key,
                    groups,
                    NewClientKey {
                        id,
                        name: command.name,
                        label: None,
                        group_ids: Vec::new(),
                        limits: command.limits,
                        budget: command.budget,
                        request_profile_overrides: Default::default(),
                        plaintext: super::client_keys::generate_key(),
                    },
                    context,
                )
                .await,
        )
        .await
    }

    async fn change_members(
        &self,
        owner: &PluginResourceOwner,
        command: GroupMembersChange,
        context: &MutationContext,
    ) -> Result<GroupMembersChanged, AdminError> {
        self.publish(self.store.change_members(owner, command, context).await)
            .await
    }
}
