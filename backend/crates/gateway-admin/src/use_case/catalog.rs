//! Provider instance 目录用例。

use std::sync::Arc;

use async_trait::async_trait;
use gateway_core::routing::{ProviderInstanceId, snapshot::SnapshotControl};

use crate::{
    model::{
        AdminError, MutationContext, Revision,
        catalog::{
            CatalogListQuery, CreateProviderInstance, DeleteProviderInstance,
            ProviderInstanceDetail, ProviderInstanceMutation, ProviderInstancePage,
            SetProviderInstanceEnabled, UpdateProviderInstance,
        },
    },
    ports::store::CatalogStore,
};

use super::{map_store_error, publish_committed};

/// API 消费的 Provider instance 管理服务。
#[async_trait]
pub trait CatalogService: Send + Sync {
    async fn list(&self, query: CatalogListQuery) -> Result<ProviderInstancePage, AdminError>;
    async fn get(&self, id: &ProviderInstanceId) -> Result<ProviderInstanceDetail, AdminError>;
    async fn create(
        &self,
        context: &MutationContext,
        command: CreateProviderInstance,
    ) -> Result<ProviderInstanceMutation, AdminError>;
    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateProviderInstance,
    ) -> Result<ProviderInstanceMutation, AdminError>;
    async fn set_enabled(
        &self,
        context: &MutationContext,
        command: SetProviderInstanceEnabled,
    ) -> Result<ProviderInstanceMutation, AdminError>;
    async fn delete(
        &self,
        context: &MutationContext,
        command: DeleteProviderInstance,
    ) -> Result<Revision, AdminError>;
}

pub(crate) struct DefaultCatalogService {
    store: Arc<dyn CatalogStore>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl DefaultCatalogService {
    #[must_use]
    pub(crate) fn new(store: Arc<dyn CatalogStore>, snapshot: Arc<dyn SnapshotControl>) -> Self {
        Self { store, snapshot }
    }
}

#[async_trait]
impl CatalogService for DefaultCatalogService {
    async fn list(&self, query: CatalogListQuery) -> Result<ProviderInstancePage, AdminError> {
        let catalog = self
            .store
            .list_provider_instances(true)
            .await
            .map_err(|error| map_store_error(error, "provider instance catalog"))?;
        let start = match query.cursor.as_ref() {
            Some(cursor) => catalog
                .items
                .iter()
                .position(|item| &item.id == cursor)
                .map(|index| index + 1)
                .ok_or_else(|| AdminError::invalid("Invalid provider instance cursor"))?,
            None => 0,
        };
        let limit = usize::from(query.page_size.get());
        let has_more = catalog.items.len().saturating_sub(start) > limit;
        let items = catalog
            .items
            .into_iter()
            .skip(start)
            .take(limit)
            .collect::<Vec<_>>();
        let next_cursor = has_more
            .then(|| items.last().map(|item| item.id.clone()))
            .flatten();
        Ok(ProviderInstancePage {
            config_revision: catalog.config_revision,
            items,
            next_cursor,
        })
    }

    async fn get(&self, id: &ProviderInstanceId) -> Result<ProviderInstanceDetail, AdminError> {
        self.store
            .load_provider_instance(id)
            .await
            .map_err(|error| map_store_error(error, "provider instance"))?
            .ok_or_else(|| AdminError::not_found("Provider instance was not found"))
    }

    async fn create(
        &self,
        context: &MutationContext,
        command: CreateProviderInstance,
    ) -> Result<ProviderInstanceMutation, AdminError> {
        let result = self
            .store
            .create_provider_instance(command, context)
            .await
            .map_err(|error| map_store_error(error, "provider instance"))?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateProviderInstance,
    ) -> Result<ProviderInstanceMutation, AdminError> {
        let result = self
            .store
            .update_provider_instance(command, context)
            .await
            .map_err(|error| map_store_error(error, "provider instance"))?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn set_enabled(
        &self,
        context: &MutationContext,
        command: SetProviderInstanceEnabled,
    ) -> Result<ProviderInstanceMutation, AdminError> {
        let result = self
            .store
            .set_provider_instance_enabled(command, context)
            .await
            .map_err(|error| map_store_error(error, "provider instance"))?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn delete(
        &self,
        context: &MutationContext,
        command: DeleteProviderInstance,
    ) -> Result<Revision, AdminError> {
        let revision = self
            .store
            .delete_provider_instance(command, context)
            .await
            .map_err(|error| map_store_error(error, "provider instance"))?;
        publish_committed(self.snapshot.as_ref(), revision).await?;
        Ok(revision)
    }
}
