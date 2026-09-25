//! 插件宿主账号回调；复用凭据事务，不建立第二套账号权威。

use std::sync::Arc;

use async_trait::async_trait;
use gateway_core::{account::ProviderAccountId, routing::ProviderKind, runtime::SnapshotControl};

use crate::{
    model::{
        AdminError, MutationContext,
        provider_credentials::{
            CredentialImportCommit, CredentialRotationCommit, PluginAccountCredential,
            PluginAccountListQuery, PluginAccountPage, PluginAccountSaveResult,
            PreparedCredentialImport, PreparedPluginAccountSave,
        },
    },
    ports::{
        plugin_accounts::PluginAccountAccess, provider::ProviderAdminRegistry, store::AccountStore,
    },
};

use super::{
    map_provider_error, map_store_error, publish_credentials_and_observe_quota,
    required_credential, required_plugin_credential, validate_prepared_import,
    validate_prepared_rotation_facts,
};

pub(crate) struct DefaultPluginAccountAccess {
    providers: ProviderAdminRegistry,
    accounts: Arc<dyn AccountStore>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl DefaultPluginAccountAccess {
    #[must_use]
    pub(crate) fn new(
        providers: ProviderAdminRegistry,
        accounts: Arc<dyn AccountStore>,
        snapshot: Arc<dyn SnapshotControl>,
    ) -> Self {
        Self {
            providers,
            accounts,
            snapshot,
        }
    }

    fn provider(
        &self,
        provider_kind: &ProviderKind,
    ) -> Result<std::sync::Arc<dyn crate::ports::provider::ProviderAdmin>, AdminError> {
        self.providers
            .require(provider_kind)
            .map_err(|error| map_provider_error(error, "plugin account access"))
    }
}

#[async_trait]
impl PluginAccountAccess for DefaultPluginAccountAccess {
    async fn list(&self, query: PluginAccountListQuery) -> Result<PluginAccountPage, AdminError> {
        self.accounts
            .list_plugin_accounts(query)
            .await
            .map_err(|error| map_store_error(error, "plugin account list"))
    }

    async fn get_runtime(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<crate::model::accounts::AccountRecord, AdminError> {
        Ok(
            required_plugin_credential(
                self.accounts.as_ref(),
                account_id,
                "plugin account runtime",
            )
            .await?
            .credential,
        )
    }

    async fn get_credential(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<PluginAccountCredential, AdminError> {
        let credential = self
            .accounts
            .load_credential_for_plugin(account_id)
            .await
            .map_err(|error| map_store_error(error, "plugin credential"))?
            .ok_or_else(|| AdminError::not_found("Provider 凭据不存在"))?;
        Ok(PluginAccountCredential {
            account: credential.account,
            provider_material: credential.provider_material,
        })
    }

    async fn get_quota(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<crate::model::provider_credentials::ProviderQuota, AdminError> {
        let account = self.get_runtime(account_id).await?;
        self.provider(&account.provider_kind)?
            .quota(crate::model::provider_credentials::ProviderQuotaRequest {
                account_id: account_id.clone(),
                refresh: false,
                rolling_usage: None,
            })
            .await
            .map_err(|error| map_provider_error(error, "plugin quota facts"))
    }
    async fn save(
        &self,
        command: PreparedPluginAccountSave,
        context: &MutationContext,
    ) -> Result<PluginAccountSaveResult, AdminError> {
        match command {
            PreparedPluginAccountSave::Create(prepared) => {
                let provider_kind = prepared.provider_kind.clone();
                let account_id = prepared.account_id.clone();
                let provider = self.provider(&provider_kind)?;
                let prepared = PreparedCredentialImport {
                    provider_kind: provider_kind.clone(),
                    credentials: vec![prepared],
                };
                validate_prepared_import(&provider_kind, &prepared, "plugin account save")?;
                let result = self
                    .accounts
                    .commit_credential_import(
                        CredentialImportCommit {
                            outbound_proxy: None,
                            settings: None,
                            prepared,
                        },
                        context,
                    )
                    .await
                    .map_err(|error| map_store_error(error, "plugin account save"))?;
                if result.credential_ids.as_slice() != std::slice::from_ref(&account_id) {
                    return Err(AdminError::internal("账号保存结果与准备目标不一致"));
                }
                let credential_revision = required_credential(
                    self.accounts.as_ref(),
                    &provider_kind,
                    &account_id,
                    "plugin account save",
                )
                .await?
                .credential
                .credential_revision;
                publish_credentials_and_observe_quota(
                    &provider,
                    self.snapshot.as_ref(),
                    result.config_revision,
                    std::slice::from_ref(&account_id),
                    &context.request_id,
                )
                .await?;
                Ok(PluginAccountSaveResult {
                    config_revision: result.config_revision,
                    account_id,
                    credential_revision,
                })
            }
            PreparedPluginAccountSave::Replace {
                facts,
                authentication_kind,
            } => {
                let account = required_credential(
                    self.accounts.as_ref(),
                    &facts.provider_kind,
                    &facts.account_id,
                    "plugin account save",
                )
                .await?
                .credential;
                validate_prepared_rotation_facts(&account, &facts)?;
                if account.authentication_kind != authentication_kind {
                    return Err(AdminError::conflict("插件准备结果与当前认证类型不一致"));
                }
                let provider = self.provider(&facts.provider_kind)?;
                let command = CredentialRotationCommit {
                    prepared: facts,
                    settings: None,
                };
                let result = self
                    .accounts
                    .commit_credential_rotation(command, context)
                    .await
                    .map_err(|error| map_store_error(error, "plugin account save"))?;
                let credential_revision = result
                    .credential_revision
                    .ok_or_else(|| AdminError::internal("账号保存未返回凭据版本"))?;
                publish_credentials_and_observe_quota(
                    &provider,
                    self.snapshot.as_ref(),
                    result.config_revision,
                    std::slice::from_ref(&result.account_id),
                    &context.request_id,
                )
                .await?;
                Ok(PluginAccountSaveResult {
                    config_revision: result.config_revision,
                    account_id: result.account_id,
                    credential_revision,
                })
            }
        }
    }
}
