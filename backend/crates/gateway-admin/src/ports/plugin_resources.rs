//! 插件自有资源的管理入口与原子持久化端口。

use super::store::AdminStoreResult;
use crate::model::{
    AdminError, MutationContext,
    account_groups::{CreateAccountGroup, NewAccountGroup},
    client_keys::NewClientKey,
    plugin_resources::{
        GroupMembersChange, GroupMembersChanged, ManagedKeyConfig, ManagedResource,
        PluginResourceOwner, ResourceMutation,
    },
};
use async_trait::async_trait;

#[async_trait]
pub trait PluginResourceAccess: Send + Sync {
    async fn ensure_group(
        &self,
        owner: &PluginResourceOwner,
        resource_key: String,
        command: CreateAccountGroup,
        context: &MutationContext,
    ) -> Result<ManagedResource, AdminError>;
    async fn ensure_key(
        &self,
        owner: &PluginResourceOwner,
        resource_key: String,
        groups: Vec<String>,
        command: ManagedKeyConfig,
        context: &MutationContext,
    ) -> Result<ManagedResource, AdminError>;
    async fn change_members(
        &self,
        owner: &PluginResourceOwner,
        command: GroupMembersChange,
        context: &MutationContext,
    ) -> Result<GroupMembersChanged, AdminError>;
}

#[async_trait]
pub trait PluginResourceStore: Send + Sync {
    async fn ensure_group(
        &self,
        owner: &PluginResourceOwner,
        resource_key: String,
        command: NewAccountGroup,
        context: &MutationContext,
    ) -> AdminStoreResult<ResourceMutation<ManagedResource>>;
    async fn ensure_key(
        &self,
        owner: &PluginResourceOwner,
        resource_key: String,
        groups: Vec<String>,
        command: NewClientKey,
        context: &MutationContext,
    ) -> AdminStoreResult<ResourceMutation<ManagedResource>>;
    async fn change_members(
        &self,
        owner: &PluginResourceOwner,
        command: GroupMembersChange,
        context: &MutationContext,
    ) -> AdminStoreResult<ResourceMutation<GroupMembersChanged>>;
}
