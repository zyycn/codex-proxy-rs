//! xAI 管理资源的中立 ProviderAdmin 委托。

use std::sync::Arc;

use async_trait::async_trait;
use gateway_core::routing::snapshot::SnapshotControl;

use crate::{
    model::{
        AdminError,
        provider_credentials::{
            AuthorizationStarted, CompleteAuthorization, CredentialDeletion,
            CredentialDeletionResult, CredentialImportCommit, CredentialImportResult,
            CredentialListQuery, CredentialMutation, CredentialMutationResult, CredentialPage,
            ImportCredentials, PrepareCredentialImport, StartAuthorization,
        },
    },
    ports::{provider::ProviderAdmin, store::AccountStore},
};

use super::{
    commit_authorization, delete_credentials, map_provider_error, map_store_error,
    pending_authorization, publish_committed, set_credential_enabled,
    validate_authorization_commit, validate_prepared_import,
};

/// xAI 固定管理路由消费的服务。
#[async_trait]
pub trait XaiService: Send + Sync {
    async fn list(&self, query: CredentialListQuery) -> Result<CredentialPage, AdminError>;
    async fn import_document(
        &self,
        command: ImportCredentials,
    ) -> Result<CredentialImportResult, AdminError>;
    async fn start_authorization(
        &self,
        command: StartAuthorization,
    ) -> Result<AuthorizationStarted, AdminError>;
    async fn complete_authorization(
        &self,
        command: CompleteAuthorization,
    ) -> Result<CredentialMutationResult, AdminError>;
    async fn enable(
        &self,
        command: CredentialMutation,
    ) -> Result<CredentialMutationResult, AdminError>;
    async fn disable(
        &self,
        command: CredentialMutation,
    ) -> Result<CredentialMutationResult, AdminError>;
    async fn delete(
        &self,
        command: CredentialDeletion,
    ) -> Result<CredentialDeletionResult, AdminError>;
}

pub(crate) struct DefaultXaiService {
    provider: Arc<dyn ProviderAdmin>,
    accounts: Arc<dyn AccountStore>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl DefaultXaiService {
    #[must_use]
    pub(crate) fn new(
        provider: Arc<dyn ProviderAdmin>,
        accounts: Arc<dyn AccountStore>,
        snapshot: Arc<dyn SnapshotControl>,
    ) -> Self {
        Self {
            provider,
            accounts,
            snapshot,
        }
    }
}

#[async_trait]
impl XaiService for DefaultXaiService {
    async fn list(&self, query: CredentialListQuery) -> Result<CredentialPage, AdminError> {
        self.accounts
            .list_credentials(self.provider.provider_kind(), query)
            .await
            .map_err(|error| map_store_error(error, "xAI credential"))
    }

    async fn import_document(
        &self,
        command: ImportCredentials,
    ) -> Result<CredentialImportResult, AdminError> {
        let context = command.context;
        let expected_config_revision = command.expected_config_revision;
        let prepared = self
            .provider
            .prepare_import(PrepareCredentialImport {
                document: command.document,
            })
            .await
            .map_err(|error| map_provider_error(error, "xAI credential import"))?;
        validate_prepared_import(
            self.provider.provider_kind(),
            &prepared,
            "xAI credential import",
        )?;
        let result = self
            .accounts
            .commit_credential_import(
                CredentialImportCommit {
                    expected_config_revision,
                    prepared,
                },
                &context,
            )
            .await
            .map_err(|error| map_store_error(error, "xAI credential import"))?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn start_authorization(
        &self,
        command: StartAuthorization,
    ) -> Result<AuthorizationStarted, AdminError> {
        let pending = pending_authorization(
            self.accounts.as_ref(),
            self.provider.provider_kind(),
            &command,
            "xAI credential",
        )
        .await?;
        self.provider
            .start_authorization(pending)
            .await
            .map_err(|error| map_provider_error(error, "xAI authorization"))
    }

    async fn complete_authorization(
        &self,
        command: CompleteAuthorization,
    ) -> Result<CredentialMutationResult, AdminError> {
        let context = command.context.clone();
        let prepared = self
            .provider
            .complete_authorization(command)
            .await
            .map_err(|error| map_provider_error(error, "xAI authorization"))?;
        validate_authorization_commit(
            self.provider.provider_kind(),
            &context,
            &prepared,
            "xAI authorization",
        )?;
        let result = commit_authorization(
            self.accounts.as_ref(),
            prepared,
            &context,
            "xAI authorization",
        )
        .await?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn enable(
        &self,
        command: CredentialMutation,
    ) -> Result<CredentialMutationResult, AdminError> {
        let result = set_credential_enabled(
            self.accounts.as_ref(),
            self.provider.as_ref(),
            command,
            true,
            "xAI credential",
        )
        .await?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn disable(
        &self,
        command: CredentialMutation,
    ) -> Result<CredentialMutationResult, AdminError> {
        let result = set_credential_enabled(
            self.accounts.as_ref(),
            self.provider.as_ref(),
            command,
            false,
            "xAI credential",
        )
        .await?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn delete(
        &self,
        command: CredentialDeletion,
    ) -> Result<CredentialDeletionResult, AdminError> {
        let result = delete_credentials(
            self.accounts.as_ref(),
            self.provider.as_ref(),
            command,
            "xAI credential",
        )
        .await?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }
}
