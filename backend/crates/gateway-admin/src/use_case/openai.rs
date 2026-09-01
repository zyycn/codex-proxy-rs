//! OpenAI 管理资源的中立 ProviderAdmin 委托。

use std::sync::Arc;

use async_trait::async_trait;
use gateway_core::{account::ProviderAccountId, runtime::SnapshotControl};

use crate::{
    model::{
        AdminError,
        provider_credentials::{
            AuthorizationStarted, CompleteAuthorization, CredentialDeletion,
            CredentialDeletionResult, CredentialDetails, CredentialImportCommit,
            CredentialImportResult, CredentialListQuery, CredentialMutationResult, CredentialPage,
            ImportCredentials, PrepareCredentialImport, PrepareCredentialRotation,
            ProviderQuotaRequest, RotateCredential, StartAuthorization,
        },
    },
    ports::{provider::ProviderAdmin, store::AccountStore},
};

use super::{
    commit_authorization, commit_credential_rotation, delete_credentials, map_provider_error,
    map_store_error, pending_authorization, publish_committed, required_credential,
    validate_authorization_commit, validate_prepared_import, validate_prepared_rotation,
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
    async fn delete(
        &self,
        command: CredentialDeletion,
    ) -> Result<CredentialDeletionResult, AdminError>;
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

    fn observe_initial_quotas(&self, account_ids: &[ProviderAccountId], request_id: &str) {
        let provider = Arc::clone(&self.provider);
        let account_ids = account_ids.to_vec();
        let request_id = request_id.to_owned();
        tokio::spawn(async move {
            for account_id in account_ids {
                if let Err(error) = provider
                    .quota(ProviderQuotaRequest {
                        account_id: account_id.clone(),
                        refresh: true,
                        rolling_usage: None,
                    })
                    .await
                {
                    tracing::warn!(
                        request_id,
                        account_id = %account_id.as_str(),
                        quota_error = ?error.kind(),
                        "OpenAI initial quota observation failed"
                    );
                }
            }
        });
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
        let prepared = self
            .provider
            .prepare_import(PrepareCredentialImport {
                document: command.document,
            })
            .await
            .map_err(|error| map_provider_error(error, "OpenAI credential import"))?;
        validate_prepared_import(
            self.provider.provider_kind(),
            &prepared,
            "OpenAI credential import",
        )?;
        let result = self
            .accounts
            .commit_credential_import(CredentialImportCommit { prepared }, &context)
            .await
            .map_err(|error| map_store_error(error, "OpenAI credential import"))?;
        self.provider
            .account_facts_changed(&result.credential_ids)
            .await;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        // 导入完成后立即做一次观察；失败不回滚已经提交的 credential。
        self.observe_initial_quotas(&result.credential_ids, &context.request_id);
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
        let prepared = validate_authorization_commit(
            self.provider.provider_kind(),
            &context,
            prepared,
            "OpenAI authorization",
        )
        .await?;
        let result = commit_authorization(
            self.accounts.as_ref(),
            prepared,
            &context,
            "OpenAI authorization",
        )
        .await?;
        self.provider
            .account_facts_changed(std::slice::from_ref(&result.account_id))
            .await;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        self.observe_initial_quotas(
            std::slice::from_ref(&result.account_id),
            &context.request_id,
        );
        Ok(result)
    }

    async fn rotate(
        &self,
        command: RotateCredential,
    ) -> Result<CredentialMutationResult, AdminError> {
        let context = command.mutation.context;
        let account_id = command.mutation.account_id;
        let details = required_credential(
            self.accounts.as_ref(),
            self.provider.provider_kind(),
            &account_id,
            "OpenAI credential rotation",
        )
        .await?;
        let account = details.credential;
        let prepared = self
            .provider
            .prepare_rotation(PrepareCredentialRotation {
                account: account.clone(),
                provider_material: command.provider_material,
            })
            .await
            .map_err(|error| map_provider_error(error, "OpenAI credential rotation"))?;
        validate_prepared_rotation(&account, &prepared, "OpenAI credential rotation")?;
        let result = commit_credential_rotation(
            self.accounts.as_ref(),
            prepared,
            &context,
            "OpenAI credential rotation",
        )
        .await?;
        self.provider
            .account_facts_changed(std::slice::from_ref(&result.account_id))
            .await;
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
            "OpenAI credential",
        )
        .await?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }
}
