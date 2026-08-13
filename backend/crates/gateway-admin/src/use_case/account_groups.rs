//! Account group management use cases.

use std::sync::Arc;

use async_trait::async_trait;
use gateway_core::routing::{AccountGroupId, snapshot::SnapshotControl};
use uuid::Uuid;

use crate::{
    model::{
        AdminError, MutationContext,
        account_groups::{
            AccountGroupListQuery, AccountGroupMembers, AccountGroupMutation, AccountGroupPage,
            CreateAccountGroup, DeleteAccountGroup, NewAccountGroup, SetAccountGroupEnabled,
            UpdateAccountGroup,
        },
    },
    ports::store::AccountGroupStore,
};

use super::{map_store_error, publish_committed};

/// API-facing account group management service.
#[async_trait]
pub trait AccountGroupService: Send + Sync {
    async fn list(&self, query: AccountGroupListQuery) -> Result<AccountGroupPage, AdminError>;
    async fn members(&self, id: &AccountGroupId) -> Result<AccountGroupMembers, AdminError>;
    async fn create(
        &self,
        context: &MutationContext,
        command: CreateAccountGroup,
    ) -> Result<AccountGroupMutation, AdminError>;
    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateAccountGroup,
    ) -> Result<AccountGroupMutation, AdminError>;
    async fn set_enabled(
        &self,
        context: &MutationContext,
        command: SetAccountGroupEnabled,
    ) -> Result<AccountGroupMutation, AdminError>;
    async fn delete(
        &self,
        context: &MutationContext,
        command: DeleteAccountGroup,
    ) -> Result<AccountGroupMutation, AdminError>;
}

pub(crate) struct DefaultAccountGroupService {
    store: Arc<dyn AccountGroupStore>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl DefaultAccountGroupService {
    #[must_use]
    pub(crate) fn new(
        store: Arc<dyn AccountGroupStore>,
        snapshot: Arc<dyn SnapshotControl>,
    ) -> Self {
        Self { store, snapshot }
    }

    async fn publish(
        &self,
        result: Result<AccountGroupMutation, crate::ports::store::AdminStoreError>,
    ) -> Result<AccountGroupMutation, AdminError> {
        let result = result.map_err(|error| map_store_error(error, "account group"))?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }
}

#[async_trait]
impl AccountGroupService for DefaultAccountGroupService {
    async fn list(&self, query: AccountGroupListQuery) -> Result<AccountGroupPage, AdminError> {
        self.store
            .list_account_groups(query)
            .await
            .map_err(|error| map_store_error(error, "account group"))
    }

    async fn members(&self, id: &AccountGroupId) -> Result<AccountGroupMembers, AdminError> {
        self.store
            .account_group_members(id)
            .await
            .map_err(|error| map_store_error(error, "account group"))
    }

    async fn create(
        &self,
        context: &MutationContext,
        command: CreateAccountGroup,
    ) -> Result<AccountGroupMutation, AdminError> {
        let id = AccountGroupId::new(format!("grp_{}", Uuid::now_v7().simple()))
            .map_err(|_| AdminError::internal("Failed to create account group ID"))?;
        self.publish(
            self.store
                .create_account_group(
                    NewAccountGroup {
                        id,
                        name: command.name,
                        description: command.description,
                        color: command.color,
                    },
                    context,
                )
                .await,
        )
        .await
    }

    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateAccountGroup,
    ) -> Result<AccountGroupMutation, AdminError> {
        self.publish(self.store.update_account_group(command, context).await)
            .await
    }

    async fn set_enabled(
        &self,
        context: &MutationContext,
        command: SetAccountGroupEnabled,
    ) -> Result<AccountGroupMutation, AdminError> {
        self.publish(self.store.set_account_group_enabled(command, context).await)
            .await
    }

    async fn delete(
        &self,
        context: &MutationContext,
        command: DeleteAccountGroup,
    ) -> Result<AccountGroupMutation, AdminError> {
        self.publish(self.store.delete_account_group(command, context).await)
            .await
    }
}
