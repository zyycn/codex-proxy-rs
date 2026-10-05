//! 已冻结 Provider 的凭据工作流；Admin 保持事务、审计与发布的唯一所有权

use std::sync::Arc;

use gateway_core::runtime::SnapshotControl;

use crate::{
    model::{
        AdminError,
        provider_credentials::{
            AuthorizationCommitResult, AuthorizationReceiptKey, AuthorizationStarted,
            CompleteAuthorization, CredentialDeletion, CredentialDeletionResult,
            CredentialImportCommit, CredentialImportResult, CredentialMutationResult,
            ImportCredentials, PrepareCredentialImport, PrepareCredentialRotation,
            RotateCredential, StartAuthorization,
        },
    },
    ports::{provider::ProviderAdmin, store::AccountStore},
};

use super::super::{
    commit_authorization, commit_credential_rotation, delete_credentials, map_provider_error,
    map_store_error, pending_authorization, publish_committed,
    publish_credentials_and_observe_quota, required_credential, validate_authorization_commit,
    validate_prepared_import, validate_prepared_rotation,
};

pub struct ProviderCredentials {
    provider: Arc<dyn ProviderAdmin>,
    accounts: Arc<dyn AccountStore>,
    proxies: Arc<dyn crate::ports::proxy::ProxyStore>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl ProviderCredentials {
    #[must_use]
    pub(crate) fn new(
        provider: Arc<dyn ProviderAdmin>,
        accounts: Arc<dyn AccountStore>,
        proxies: Arc<dyn crate::ports::proxy::ProxyStore>,
        snapshot: Arc<dyn SnapshotControl>,
    ) -> Self {
        Self {
            provider,
            accounts,
            proxies,
            snapshot,
        }
    }
    pub async fn import_document(
        &self,
        command: ImportCredentials,
    ) -> Result<CredentialImportResult, AdminError> {
        let context = command.context;
        let proxy_reservation = super::super::import_proxy_binding(
            self.proxies.as_ref(),
            command.outbound_proxy_id.as_deref(),
        )
        .await?;
        let outbound_proxy = proxy_reservation
            .as_ref()
            .map(|reservation| reservation.binding.clone());
        let prepared = self
            .provider
            .prepare_import(PrepareCredentialImport {
                default_outbound_proxy: outbound_proxy
                    .as_ref()
                    .map(|binding| binding.proxy.clone()),
                document: command.document,
            })
            .await
            .map_err(|error| map_provider_error(error, "provider credential import"))?;
        validate_prepared_import(
            self.provider.provider_kind(),
            &prepared,
            "provider credential import",
        )?;
        let result = self
            .accounts
            .commit_credential_import(
                CredentialImportCommit {
                    outbound_proxy,
                    prepared,
                    settings: command.settings,
                },
                &context,
            )
            .await
            .map_err(|error| map_store_error(error, "provider credential import"))?;
        drop(proxy_reservation);
        publish_credentials_and_observe_quota(
            &self.provider,
            self.snapshot.as_ref(),
            result.config_revision,
            &result.credential_ids,
            &context.request_id,
        )
        .await?;
        Ok(result)
    }

    pub async fn start_authorization(
        &self,
        command: StartAuthorization,
    ) -> Result<AuthorizationStarted, AdminError> {
        let pending = pending_authorization(
            self.accounts.as_ref(),
            self.proxies.as_ref(),
            self.provider.provider_kind(),
            &command,
            "provider credential",
        )
        .await?;
        self.provider
            .start_authorization(pending)
            .await
            .map_err(|error| map_provider_error(error, "provider authorization"))
    }

    pub async fn rotate(
        &self,
        command: RotateCredential,
    ) -> Result<CredentialMutationResult, AdminError> {
        let context = command.mutation.context;
        let account_id = command.mutation.account_id;
        if command
            .settings
            .as_ref()
            .is_some_and(|settings| settings.account_id != account_id.as_str())
        {
            return Err(AdminError::invalid("凭据和账号设置的目标不一致"));
        }
        let disable_account = command
            .settings
            .as_ref()
            .is_some_and(|settings| !settings.enabled);
        let details = required_credential(
            self.accounts.as_ref(),
            self.provider.provider_kind(),
            &account_id,
            "provider credential rotation",
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
            .map_err(|error| map_provider_error(error, "provider credential rotation"))?;
        validate_prepared_rotation(&account, &prepared, "provider credential rotation")?;
        let result = commit_credential_rotation(
            self.accounts.as_ref(),
            prepared,
            command.settings,
            &context,
            "provider credential rotation",
        )
        .await?;
        if disable_account {
            self.provider.account_unavailable(&account_id).await;
        }
        self.provider
            .account_facts_changed(std::slice::from_ref(&result.account_id))
            .await;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }

    pub async fn delete(
        &self,
        command: CredentialDeletion,
    ) -> Result<CredentialDeletionResult, AdminError> {
        let result = delete_credentials(
            self.accounts.as_ref(),
            self.provider.as_ref(),
            command,
            "provider credential",
        )
        .await?;
        publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }
}

impl super::CredentialsService {
    pub async fn complete_authorization(
        &self,
        kind: &gateway_core::routing::ProviderKind,
        mut command: CompleteAuthorization,
    ) -> Result<CredentialMutationResult, AdminError> {
        let context = command.context.clone();
        let key = AuthorizationReceiptKey::new(kind.clone(), &command.flow_id, &context)?;
        if let Some(result) = self
            .accounts
            .authorization_receipt(&key)
            .await
            .map_err(|error| map_store_error(error, "authorization receipt"))?
        {
            publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
            return Ok(result);
        }
        let scope = self.for_provider(kind)?;
        let provider = &scope.provider;
        let settings = command.settings.take();
        let prepared = provider
            .complete_authorization(command)
            .await
            .map_err(|error| map_provider_error(error, "provider authorization"))?;
        let prepared =
            validate_authorization_commit(kind, &context, prepared, "provider authorization")
                .await?;
        let result = commit_authorization(
            self.accounts.as_ref(),
            prepared,
            key,
            settings,
            &context,
            "provider authorization",
        )
        .await?;
        let result = self
            .publish_authorization(provider, result, &context.request_id)
            .await?;
        Ok(result)
    }

    async fn publish_authorization(
        &self,
        provider: &Arc<dyn ProviderAdmin>,
        outcome: AuthorizationCommitResult,
        request_id: &str,
    ) -> Result<CredentialMutationResult, AdminError> {
        let result = outcome.result;
        if outcome.newly_committed {
            publish_credentials_and_observe_quota(
                provider,
                self.snapshot.as_ref(),
                result.config_revision,
                std::slice::from_ref(&result.account_id),
                request_id,
            )
            .await?;
        } else {
            publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        }
        Ok(result)
    }
}
