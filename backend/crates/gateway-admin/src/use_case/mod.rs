//! 管理控制面的用例实现。

pub mod account_groups;
pub mod accounts;
pub mod auth;
pub mod backup;
pub mod client_keys;
pub mod observability;
pub mod openai;
pub mod settings;
pub mod system;
pub mod xai;

use crate::{
    model::{
        AdminError, AdminErrorKind, MutationContext,
        accounts::DeleteAccounts,
        provider_credentials::{
            AuthorizationMutationTarget, CredentialDeletion, CredentialDeletionResult,
            CredentialDetails, CredentialMutationResult, CredentialRotationCommit,
            PendingAuthorizationMutation, PreparedAuthorizationCommit,
            PreparedAuthorizationCredential, PreparedCredentialImport, PreparedCredentialRotation,
            StartAuthorization,
        },
    },
    ports::{
        provider::ProviderAdmin,
        store::{AccountStore, AdminStoreError, AdminStoreErrorKind},
    },
};
use gateway_core::{
    engine::credential::ProviderAccountId,
    routing::{ConfigRevision, ProviderKind, snapshot::SnapshotControl},
};

fn map_store_error(error: AdminStoreError, resource: &'static str) -> AdminError {
    let kind = match error.kind() {
        AdminStoreErrorKind::Invalid => AdminErrorKind::Invalid,
        AdminStoreErrorKind::NotFound => AdminErrorKind::NotFound,
        AdminStoreErrorKind::StaleRevision | AdminStoreErrorKind::Conflict => {
            AdminErrorKind::Conflict
        }
        AdminStoreErrorKind::Unavailable => AdminErrorKind::Unavailable,
    };
    AdminError::new(kind, format!("{resource} operation failed"))
}

fn map_provider_error(
    error: crate::ports::provider::ProviderAdminError,
    resource: &'static str,
) -> AdminError {
    use crate::ports::provider::ProviderAdminErrorKind;

    let kind = match error.kind() {
        ProviderAdminErrorKind::Invalid => AdminErrorKind::Invalid,
        ProviderAdminErrorKind::Unsupported => AdminErrorKind::Invalid,
        ProviderAdminErrorKind::NotFound => AdminErrorKind::NotFound,
        ProviderAdminErrorKind::Conflict => AdminErrorKind::Conflict,
        ProviderAdminErrorKind::Unavailable => AdminErrorKind::Unavailable,
        ProviderAdminErrorKind::Internal => AdminErrorKind::Internal,
    };
    AdminError::new(kind, format!("{resource} operation failed"))
}

async fn publish_committed(
    snapshot: &dyn SnapshotControl,
    revision: crate::model::Revision,
) -> Result<(), AdminError> {
    let revision = ConfigRevision::new(revision.get())
        .map_err(|_| AdminError::internal("Committed configuration revision is invalid"))?;
    snapshot.publish_committed(revision).await;
    Ok(())
}

async fn required_credential(
    accounts: &dyn AccountStore,
    provider_kind: &ProviderKind,
    account_id: &ProviderAccountId,
    resource: &'static str,
) -> Result<CredentialDetails, AdminError> {
    accounts
        .credential_details(provider_kind, account_id)
        .await
        .map_err(|error| map_store_error(error, resource))?
        .ok_or_else(|| AdminError::not_found(format!("{resource} was not found")))
}

async fn pending_authorization(
    accounts: &dyn AccountStore,
    provider_kind: &ProviderKind,
    command: &StartAuthorization,
    resource: &'static str,
) -> Result<PendingAuthorizationMutation, AdminError> {
    let target = match &command.reauthorization {
        Some(account_id) => {
            required_credential(accounts, provider_kind, account_id, resource).await?;
            AuthorizationMutationTarget::Reauthorize {
                account_id: account_id.clone(),
            }
        }
        None => AuthorizationMutationTarget::Create {
            name: command.name.clone(),
        },
    };
    Ok(PendingAuthorizationMutation::new(
        provider_kind.clone(),
        target,
        crate::model::provider_credentials::AuthorizationOwnerBinding::from_context(
            &command.context,
        ),
    ))
}

fn validate_prepared_import(
    provider_kind: &ProviderKind,
    prepared: &PreparedCredentialImport,
    resource: &'static str,
) -> Result<(), AdminError> {
    if prepared.provider_kind != *provider_kind
        || prepared
            .credentials
            .iter()
            .any(|credential| credential.provider_kind != *provider_kind)
    {
        return Err(AdminError::conflict(format!(
            "{resource} prepared facts do not match the requested Provider scope"
        )));
    }
    Ok(())
}

fn validate_prepared_rotation(
    account: &crate::model::accounts::AccountRecord,
    prepared: &PreparedCredentialRotation,
    resource: &'static str,
) -> Result<(), AdminError> {
    let facts = prepared.facts();
    if facts.account_id.as_str() != account.id.as_str()
        || facts.provider_kind != account.provider_kind
    {
        return Err(AdminError::conflict(format!(
            "{resource} prepared facts do not match the current credential"
        )));
    }
    Ok(())
}

async fn validate_authorization_commit(
    provider_kind: &ProviderKind,
    context: &MutationContext,
    prepared: PreparedAuthorizationCommit,
    resource: &'static str,
) -> Result<PreparedAuthorizationCommit, AdminError> {
    if prepared.pending.provider_kind() != provider_kind
        || !prepared.pending.owner_binding().matches_context(context)
    {
        let validation_error = AdminError::conflict(format!(
            "{resource} pending authorization binding is invalid"
        ));
        if let Err(error) = prepared.abort().await {
            tracing::warn!(
                resource,
                settlement_error = %error,
                "OAuth authorization claim release failed after preparation validation"
            );
            return Err(error);
        }
        return Err(validation_error);
    }
    let matches_target = match (prepared.pending.target(), &prepared.credential) {
        (
            AuthorizationMutationTarget::Create { .. },
            PreparedAuthorizationCredential::Create(credential),
        ) => credential.provider_kind == *provider_kind,
        (
            AuthorizationMutationTarget::Reauthorize { account_id },
            PreparedAuthorizationCredential::Reauthorize(credential),
        ) => {
            let facts = credential.facts();
            facts.provider_kind == *provider_kind && facts.account_id == *account_id
        }
        _ => false,
    };
    if !matches_target {
        let validation_error = AdminError::conflict(format!(
            "{resource} prepared credential does not match its pending target"
        ));
        if let Err(error) = prepared.abort().await {
            tracing::warn!(
                resource,
                settlement_error = %error,
                "OAuth authorization claim release failed after preparation validation"
            );
            return Err(error);
        }
        return Err(validation_error);
    }
    Ok(prepared)
}

async fn commit_authorization(
    accounts: &dyn AccountStore,
    prepared: PreparedAuthorizationCommit,
    context: &MutationContext,
    resource: &'static str,
) -> Result<CredentialMutationResult, AdminError> {
    let crate::model::provider_credentials::AuthorizationCommitSettlement {
        command,
        credential_guard,
        authorization_guard,
    } = prepared.into_commit();
    match accounts.commit_authorization(command, context).await {
        Ok(result) => {
            if let Some(guard) = credential_guard {
                guard.finish();
            }
            if let Some(guard) = authorization_guard {
                guard.commit().await?;
            }
            Ok(result)
        }
        Err(error) => {
            drop(credential_guard);
            if let Some(guard) = authorization_guard
                && let Err(settlement_error) = guard.abort().await
            {
                tracing::warn!(
                    resource,
                    store_error = %error,
                    settlement_error = %settlement_error,
                    "OAuth authorization claim release failed after Store commit failure"
                );
                return Err(settlement_error);
            }
            Err(map_store_error(error, resource))
        }
    }
}

async fn commit_credential_rotation(
    accounts: &dyn AccountStore,
    prepared: PreparedCredentialRotation,
    context: &MutationContext,
    resource: &'static str,
) -> Result<CredentialMutationResult, AdminError> {
    let (facts, guard) = prepared.into_parts();
    match accounts
        .commit_credential_rotation(CredentialRotationCommit { prepared: facts }, context)
        .await
    {
        Ok(result) => {
            guard.finish();
            Ok(result)
        }
        Err(error) => {
            drop(guard);
            Err(map_store_error(error, resource))
        }
    }
}

async fn commit_credential_refresh(
    accounts: &dyn AccountStore,
    prepared: PreparedCredentialRotation,
    context: &MutationContext,
    resource: &'static str,
) -> Result<CredentialMutationResult, AdminError> {
    let (facts, guard) = prepared.into_parts();
    match accounts
        .commit_credential_refresh(CredentialRotationCommit { prepared: facts }, context)
        .await
    {
        Ok(result) => {
            guard.finish();
            Ok(result)
        }
        Err(error) => {
            drop(guard);
            Err(map_store_error(error, resource))
        }
    }
}

async fn delete_credentials(
    accounts: &dyn AccountStore,
    provider: &dyn ProviderAdmin,
    command: CredentialDeletion,
    resource: &'static str,
) -> Result<CredentialDeletionResult, AdminError> {
    for account_id in &command.account_ids {
        required_credential(accounts, provider.provider_kind(), account_id, resource).await?;
    }
    let account_ids = command.account_ids;
    let revision = accounts
        .delete_accounts(
            DeleteAccounts {
                account_ids: account_ids
                    .iter()
                    .map(|account_id| account_id.as_str().to_owned())
                    .collect(),
            },
            &command.context,
        )
        .await
        .map_err(|error| map_store_error(error, resource))?;
    for account_id in &account_ids {
        provider.account_unavailable(account_id).await;
    }
    provider.account_facts_changed(&account_ids).await;
    Ok(CredentialDeletionResult {
        config_revision: revision,
        account_ids,
    })
}
