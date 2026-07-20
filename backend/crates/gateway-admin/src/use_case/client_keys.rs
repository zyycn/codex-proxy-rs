//! Client API Key 管理用例。

use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::routing::snapshot::SnapshotControl;
use rand_core::{OsRng, RngCore as _};
use uuid::Uuid;

use crate::{
    model::{
        AdminError, MutationContext,
        client_keys::{
            ClientKeyCursorValue, ClientKeyListQuery, ClientKeyMutation, ClientKeyPage,
            ClientKeySecret, ClientKeySortField, CreateClientKey, CreatedClientKey,
            DeleteClientKey, NewClientKey, SetClientKeyEnabled, UpdateClientKey,
        },
    },
    ports::store::ClientKeyStore,
};

use super::{map_store_error, publish_committed};

/// API 消费的 Client Key 管理服务。
#[async_trait]
pub trait ClientKeyService: Send + Sync {
    async fn list(&self, query: ClientKeyListQuery) -> Result<ClientKeyPage, AdminError>;
    async fn reveal(&self, id: &ClientApiKeyId) -> Result<ClientKeySecret, AdminError>;
    async fn create(
        &self,
        context: &MutationContext,
        command: CreateClientKey,
    ) -> Result<CreatedClientKey, AdminError>;
    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateClientKey,
    ) -> Result<ClientKeyMutation, AdminError>;
    async fn set_enabled(
        &self,
        context: &MutationContext,
        command: SetClientKeyEnabled,
    ) -> Result<ClientKeyMutation, AdminError>;
    async fn delete(
        &self,
        context: &MutationContext,
        command: DeleteClientKey,
    ) -> Result<ClientKeyMutation, AdminError>;
}

pub(crate) struct DefaultClientKeyService {
    store: Arc<dyn ClientKeyStore>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl DefaultClientKeyService {
    #[must_use]
    pub(crate) fn new(store: Arc<dyn ClientKeyStore>, snapshot: Arc<dyn SnapshotControl>) -> Self {
        Self { store, snapshot }
    }
}

#[async_trait]
impl ClientKeyService for DefaultClientKeyService {
    async fn list(&self, query: ClientKeyListQuery) -> Result<ClientKeyPage, AdminError> {
        validate_cursor(&query)?;
        self.store
            .list_client_keys(query)
            .await
            .map_err(|error| map_store_error(error, "client API key"))
    }

    async fn reveal(&self, id: &ClientApiKeyId) -> Result<ClientKeySecret, AdminError> {
        self.store
            .reveal_client_key(id)
            .await
            .map_err(|error| map_store_error(error, "client API key"))?
            .ok_or_else(|| AdminError::not_found("Client API key was not found"))
    }

    async fn create(
        &self,
        context: &MutationContext,
        command: CreateClientKey,
    ) -> Result<CreatedClientKey, AdminError> {
        let id = ClientApiKeyId::new(format!("key_{}", Uuid::now_v7().simple()))
            .map_err(|_| AdminError::internal("Failed to create Client API key ID"))?;
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let plaintext = format!("sk_{}", URL_SAFE_NO_PAD.encode(bytes));
        let (config_revision, record) = self
            .store
            .create_client_key(
                NewClientKey {
                    expected_config_revision: command.expected_config_revision,
                    id,
                    name: command.name,
                    label: command.label,
                    provider_kind: command.provider_kind,
                    limits: command.limits,
                    plaintext: plaintext.clone(),
                },
                context,
            )
            .await
            .map_err(|error| map_store_error(error, "client API key"))?;
        publish_committed(self.snapshot.as_ref(), config_revision).await?;
        Ok(CreatedClientKey {
            config_revision,
            secret: ClientKeySecret::new(record, plaintext),
        })
    }

    async fn update(
        &self,
        context: &MutationContext,
        command: UpdateClientKey,
    ) -> Result<ClientKeyMutation, AdminError> {
        let id = command.id.clone();
        let (config_revision, record) = self
            .store
            .update_client_key(command, context)
            .await
            .map_err(|error| map_store_error(error, "client API key"))?;
        publish_committed(self.snapshot.as_ref(), config_revision).await?;
        Ok(ClientKeyMutation {
            config_revision,
            record: Some(record),
            id,
        })
    }

    async fn set_enabled(
        &self,
        context: &MutationContext,
        command: SetClientKeyEnabled,
    ) -> Result<ClientKeyMutation, AdminError> {
        let id = command.id.clone();
        let (config_revision, record) =
            self.store
                .set_client_key_enabled(command, context)
                .await
                .map_err(|error| map_store_error(error, "client API key"))?;
        publish_committed(self.snapshot.as_ref(), config_revision).await?;
        Ok(ClientKeyMutation {
            config_revision,
            record: Some(record),
            id,
        })
    }

    async fn delete(
        &self,
        context: &MutationContext,
        command: DeleteClientKey,
    ) -> Result<ClientKeyMutation, AdminError> {
        let id = command.id.clone();
        let config_revision = self
            .store
            .delete_client_key(command, context)
            .await
            .map_err(|error| map_store_error(error, "client API key"))?;
        publish_committed(self.snapshot.as_ref(), config_revision).await?;
        Ok(ClientKeyMutation {
            config_revision,
            record: None,
            id,
        })
    }
}

fn validate_cursor(query: &ClientKeyListQuery) -> Result<(), AdminError> {
    let Some(cursor) = &query.cursor else {
        return Ok(());
    };
    if cursor.sort != query.sort {
        return Err(AdminError::invalid(
            "Client API key cursor sort does not match the query",
        ));
    }
    let matches = matches!(
        (cursor.sort.field, &cursor.value),
        (ClientKeySortField::Name, ClientKeyCursorValue::Name(value)) if !value.trim().is_empty()
    ) || matches!(
        (cursor.sort.field, &cursor.value),
        (
            ClientKeySortField::Enabled,
            ClientKeyCursorValue::Enabled(_)
        ) | (
            ClientKeySortField::CreatedAt,
            ClientKeyCursorValue::CreatedAt(_)
        ) | (
            ClientKeySortField::LastUsedAt,
            ClientKeyCursorValue::LastUsedAt(_)
        )
    );
    if matches {
        Ok(())
    } else {
        Err(AdminError::invalid("Invalid Client API key cursor"))
    }
}
