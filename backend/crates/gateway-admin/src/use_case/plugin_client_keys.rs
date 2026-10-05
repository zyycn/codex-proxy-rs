//! 插件 Client Key 管理；复用管理服务，只开放非秘密目录与预算操作

use std::sync::Arc;

use async_trait::async_trait;
use gateway_core::{engine::budget::ClientBudgetStatus, policy::ClientApiKeyId};

use crate::{
    model::{
        AdminError, MutationContext,
        client_keys::{
            ClientKeyBudgetMutationOrigin, ClientKeyCursor, ClientKeyCursorValue,
            ClientKeyListQuery, ClientKeyPageSize, ClientKeySort, ClientKeySortField,
            ResetClientKeyBudget, SortDirection, UpdateClientKeyBudgetLimits,
        },
        plugin_client_keys::{
            PluginClientKey, PluginClientKeyCursor, PluginClientKeyFacts, PluginClientKeyListQuery,
            PluginClientKeyPage,
        },
        plugin_resources::PluginResourceOwner,
    },
    ports::plugin_client_keys::PluginClientKeyAccess,
    use_case::client_keys::ClientKeyService,
};

pub(crate) struct DefaultPluginClientKeyAccess {
    service: Arc<dyn ClientKeyService>,
}

impl DefaultPluginClientKeyAccess {
    pub(crate) fn new(service: Arc<dyn ClientKeyService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl PluginClientKeyAccess for DefaultPluginClientKeyAccess {
    async fn facts(&self, id: &ClientApiKeyId) -> Result<PluginClientKeyFacts, AdminError> {
        let key = self.service.get(id).await?;
        Ok(PluginClientKeyFacts {
            id: key.id,
            enabled: key.enabled,
            group_ids: key.groups.into_iter().map(|group| group.id).collect(),
        })
    }

    async fn budget(&self, id: &ClientApiKeyId) -> Result<ClientBudgetStatus, AdminError> {
        self.service.budget(id).await
    }

    async fn update_budget_limits(
        &self,
        owner: &PluginResourceOwner,
        command: UpdateClientKeyBudgetLimits,
        context: &MutationContext,
    ) -> Result<ClientApiKeyId, AdminError> {
        self.service
            .update_budget_limits(
                context,
                command,
                ClientKeyBudgetMutationOrigin::Plugin(owner.clone()),
            )
            .await
    }

    async fn reset_budget(
        &self,
        owner: &PluginResourceOwner,
        command: ResetClientKeyBudget,
        context: &MutationContext,
    ) -> Result<ClientApiKeyId, AdminError> {
        self.service
            .reset_budget(
                context,
                command,
                ClientKeyBudgetMutationOrigin::Plugin(owner.clone()),
            )
            .await
    }

    async fn list(
        &self,
        query: PluginClientKeyListQuery,
    ) -> Result<PluginClientKeyPage, AdminError> {
        let sort = ClientKeySort {
            field: ClientKeySortField::Name,
            direction: SortDirection::Asc,
        };
        let page_size = ClientKeyPageSize::new(query.limit.get())
            .map_err(|_| AdminError::internal("插件 Client Key 页大小不合法"))?;
        let page = self
            .service
            .list(ClientKeyListQuery {
                cursor: query.cursor.map(|cursor| ClientKeyCursor {
                    sort,
                    value: ClientKeyCursorValue::Name(cursor.name),
                    id: cursor.id,
                }),
                page_size,
                search: None,
                sort,
            })
            .await?;
        let next_cursor = page
            .next_cursor
            .map(|cursor| match cursor.value {
                ClientKeyCursorValue::Name(name)
                    if cursor.sort.field == ClientKeySortField::Name
                        && cursor.sort.direction == SortDirection::Asc =>
                {
                    Ok(PluginClientKeyCursor {
                        name,
                        id: cursor.id,
                    })
                }
                _ => Err(AdminError::internal(
                    "Client Key 服务返回了不匹配的插件分页游标",
                )),
            })
            .transpose()?;
        Ok(PluginClientKeyPage {
            items: page
                .items
                .into_iter()
                .map(|item| PluginClientKey {
                    id: item.id,
                    name: item.name,
                    enabled: item.enabled,
                })
                .collect(),
            next_cursor,
        })
    }
}
