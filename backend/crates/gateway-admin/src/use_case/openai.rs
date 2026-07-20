//! OpenAI 管理资源的中立 ProviderAdmin 委托。

use std::sync::Arc;

use async_trait::async_trait;
use gateway_core::{engine::credential::ProviderAccountId, routing::snapshot::SnapshotControl};

use crate::{
    model::{
        AdminError,
        provider_credentials::{
            AuthorizationStarted, CompleteAuthorization, CredentialDetails, CredentialImportCommit,
            CredentialImportResult, CredentialListQuery, CredentialMutation,
            CredentialMutationResult, CredentialPage, ImportCredentials, PrepareCredentialImport,
            PrepareCredentialRotation, RotateCredential, StartAuthorization,
        },
    },
    ports::{provider::ProviderAdmin, store::AccountStore},
};

use super::{
    commit_authorization, commit_credential_rotation, delete_credential, map_provider_error,
    map_store_error, pending_authorization, publish_committed, required_credential,
    set_credential_enabled, validate_authorization_commit, validate_prepared_import,
    validate_prepared_rotation,
};

/// OpenAI 固定管理路由消费的服务。
#[async_trait]
pub trait OpenAiService: Send + Sync {
    async fn list(&self, query: CredentialListQuery) -> Result<CredentialPage, AdminError>;
    async fn details(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CredentialDetails, AdminError>;
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
    async fn rotate(
        &self,
        command: RotateCredential,
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
        command: CredentialMutation,
    ) -> Result<CredentialMutationResult, AdminError>;
}

pub(crate) struct DefaultOpenAiService {
    provider: Arc<dyn ProviderAdmin>,
    accounts: Arc<dyn AccountStore>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl DefaultOpenAiService {
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
impl OpenAiService for DefaultOpenAiService {
    async fn list(&self, query: CredentialListQuery) -> Result<CredentialPage, AdminError> {
        self.accounts
            .list_credentials(self.provider.provider_kind(), query)
            .await
            .map_err(|error| map_store_error(error, "OpenAI credential"))
    }

    async fn details(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<CredentialDetails, AdminError> {
        required_credential(
            self.accounts.as_ref(),
            self.provider.provider_kind(),
            account_id,
            "OpenAI credential",
        )
        .await
    }

    async fn import_document(
        &self,
        command: ImportCredentials,
    ) -> Result<CredentialImportResult, AdminError> {
        let context = command.context;
        let expected_config_revision = command.expected_config_revision;
        let provider_instance_id = command.provider_instance_id;
        let prepared = self
            .provider
            .prepare_import(PrepareCredentialImport {
                provider_instance_id: provider_instance_id.clone(),
                document: command.document,
            })
            .await
            .map_err(|error| map_provider_error(error, "OpenAI credential import"))?;
        validate_prepared_import(
            self.provider.provider_kind(),
            &provider_instance_id,
            &prepared,
            "OpenAI credential import",
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
            .map_err(|error| map_store_error(error, "OpenAI credential import"))?;
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
            "OpenAI credential",
        )
        .await?;
        self.provider
            .start_authorization(pending)
            .await
            .map_err(|error| map_provider_error(error, "OpenAI authorization"))
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
            .map_err(|error| map_provider_error(error, "OpenAI authorization"))?;
        validate_authorization_commit(
            self.provider.provider_kind(),
            &context,
            &prepared,
            "OpenAI authorization",
        )?;
        let result = commit_authorization(
            self.accounts.as_ref(),
            prepared,
            &context,
            "OpenAI authorization",
        )
        .await?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn rotate(
        &self,
        command: RotateCredential,
    ) -> Result<CredentialMutationResult, AdminError> {
        let context = command.mutation.context;
        let expected_config_revision = command.mutation.expected_config_revision;
        let account_id = command.mutation.account_id;
        let details = required_credential(
            self.accounts.as_ref(),
            self.provider.provider_kind(),
            &account_id,
            "OpenAI credential rotation",
        )
        .await?;
        if details.credential.credential_revision != command.expected_credential_revision {
            return Err(AdminError::conflict(
                "OpenAI credential rotation revision is stale",
            ));
        }
        let account = details.credential;
        let prepared = self
            .provider
            .prepare_rotation(PrepareCredentialRotation {
                account: account.clone(),
                expected_credential_revision: command.expected_credential_revision,
                provider_material: command.provider_material,
            })
            .await
            .map_err(|error| map_provider_error(error, "OpenAI credential rotation"))?;
        validate_prepared_rotation(&account, &prepared, "OpenAI credential rotation")?;
        let result = commit_credential_rotation(
            self.accounts.as_ref(),
            expected_config_revision,
            prepared,
            &context,
            "OpenAI credential rotation",
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
            "OpenAI credential",
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
            "OpenAI credential",
        )
        .await?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    async fn delete(
        &self,
        command: CredentialMutation,
    ) -> Result<CredentialMutationResult, AdminError> {
        let result = delete_credential(
            self.accounts.as_ref(),
            self.provider.as_ref(),
            command,
            "OpenAI credential",
        )
        .await?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }
}
