//! 管理控制面的用例实现。

pub mod accounts;
pub mod auth;
pub mod catalog;
pub mod client_keys;
pub mod observability;
pub mod openai;
pub mod settings;
pub mod system;
pub mod xai;

use crate::{
    model::{
        AdminError, AdminErrorKind, MutationContext,
        accounts::{DeleteAccount, SetAccountEnabled},
        provider_credentials::{
            AuthorizationMutationTarget, CredentialDetails, CredentialMutation,
            CredentialMutationResult, CredentialRotationCommit, PendingAuthorizationMutation,
            PreparedAuthorizationCommit, PreparedAuthorizationCredential, PreparedCredentialImport,
            PreparedCredentialRotation, StartAuthorization,
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
        Some(target) => {
            let details =
                required_credential(accounts, provider_kind, &target.account_id, resource).await?;
            if details.credential.provider_instance_id != command.provider_instance_id
                || details.credential.credential_revision != target.credential_revision
            {
                return Err(AdminError::conflict(format!(
                    "{resource} reauthorization target is stale"
                )));
            }
            AuthorizationMutationTarget::Reauthorize {
                provider_instance_id: command.provider_instance_id.clone(),
                account_id: target.account_id.clone(),
                expected_credential_revision: target.credential_revision,
            }
        }
        None => AuthorizationMutationTarget::Create {
            provider_instance_id: command.provider_instance_id.clone(),
            name: command.name.clone(),
        },
    };
    Ok(PendingAuthorizationMutation::new(
        command.expected_config_revision,
        provider_kind.clone(),
        target,
        crate::model::provider_credentials::AuthorizationOwnerBinding::from_context(
            &command.context,
        ),
    ))
}

fn validate_prepared_import(
    provider_kind: &ProviderKind,
    provider_instance_id: &gateway_core::routing::ProviderInstanceId,
    prepared: &PreparedCredentialImport,
    resource: &'static str,
) -> Result<(), AdminError> {
    if prepared.provider_kind != *provider_kind
        || prepared.provider_instance_id != *provider_instance_id
        || prepared.credentials.iter().any(|credential| {
            credential.provider_kind != *provider_kind
                || credential.provider_instance_id != *provider_instance_id
        })
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
        || facts.provider_instance_id != account.provider_instance_id
        || facts.expected_credential_revision != account.credential_revision
    {
        return Err(AdminError::conflict(format!(
            "{resource} prepared facts do not match the current credential"
        )));
    }
    Ok(())
}

fn validate_authorization_commit(
    provider_kind: &ProviderKind,
    context: &MutationContext,
    prepared: &PreparedAuthorizationCommit,
    resource: &'static str,
) -> Result<(), AdminError> {
    if prepared.pending.provider_kind() != provider_kind
        || !prepared.pending.owner_binding().matches_context(context)
    {
        return Err(AdminError::conflict(format!(
            "{resource} pending authorization binding is invalid"
        )));
    }
    let matches_target = match (prepared.pending.target(), &prepared.credential) {
        (
            AuthorizationMutationTarget::Create {
                provider_instance_id,
                ..
            },
            PreparedAuthorizationCredential::Create(credential),
        ) => {
            credential.provider_kind == *provider_kind
                && credential.provider_instance_id == *provider_instance_id
        }
        (
            AuthorizationMutationTarget::Reauthorize {
                provider_instance_id,
                account_id,
                expected_credential_revision,
            },
            PreparedAuthorizationCredential::Reauthorize(credential),
        ) => {
            let facts = credential.facts();
            facts.provider_kind == *provider_kind
                && facts.provider_instance_id == *provider_instance_id
                && facts.account_id == *account_id
                && facts.expected_credential_revision == *expected_credential_revision
        }
        _ => false,
    };
    if !matches_target {
        return Err(AdminError::conflict(format!(
            "{resource} prepared credential does not match its pending target"
        )));
    }
    Ok(())
}

async fn commit_authorization(
    accounts: &dyn AccountStore,
    prepared: PreparedAuthorizationCommit,
    context: &MutationContext,
    resource: &'static str,
) -> Result<CredentialMutationResult, AdminError> {
    let (command, guard) = prepared.into_commit();
    match accounts.commit_authorization(command, context).await {
        Ok(result) => {
            if let Some(guard) = guard {
                guard.finish();
            }
            Ok(result)
        }
        Err(error) => {
            drop(guard);
            Err(map_store_error(error, resource))
        }
    }
}

async fn commit_credential_rotation(
    accounts: &dyn AccountStore,
    expected_config_revision: crate::model::Revision,
    prepared: PreparedCredentialRotation,
    context: &MutationContext,
    resource: &'static str,
) -> Result<CredentialMutationResult, AdminError> {
    let (facts, guard) = prepared.into_parts();
    match accounts
        .commit_credential_rotation(
            CredentialRotationCommit {
                expected_config_revision,
                prepared: facts,
            },
            context,
        )
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
    expected_config_revision: crate::model::Revision,
    prepared: PreparedCredentialRotation,
    context: &MutationContext,
    resource: &'static str,
) -> Result<CredentialMutationResult, AdminError> {
    let (facts, guard) = prepared.into_parts();
    match accounts
        .commit_credential_refresh(
            CredentialRotationCommit {
                expected_config_revision,
                prepared: facts,
            },
            context,
        )
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

async fn set_credential_enabled(
    accounts: &dyn AccountStore,
    provider: &dyn ProviderAdmin,
    command: CredentialMutation,
    enabled: bool,
    resource: &'static str,
) -> Result<CredentialMutationResult, AdminError> {
    required_credential(
        accounts,
        provider.provider_kind(),
        &command.account_id,
        resource,
    )
    .await?;
    let account_id = command.account_id;
    let revision = accounts
        .set_account_enabled(
            SetAccountEnabled {
                expected_config_revision: command.expected_config_revision,
                account_id: account_id.as_str().to_owned(),
                enabled,
            },
            &command.context,
        )
        .await
        .map_err(|error| map_store_error(error, resource))?;
    if !enabled {
        provider.account_unavailable(&account_id).await;
    }
    Ok(CredentialMutationResult {
        config_revision: revision,
        account_id,
        credential_revision: None,
    })
}

async fn delete_credential(
    accounts: &dyn AccountStore,
    provider: &dyn ProviderAdmin,
    command: CredentialMutation,
    resource: &'static str,
) -> Result<CredentialMutationResult, AdminError> {
    required_credential(
        accounts,
        provider.provider_kind(),
        &command.account_id,
        resource,
    )
    .await?;
    let account_id = command.account_id;
    let revision = accounts
        .delete_account(
            DeleteAccount {
                expected_config_revision: command.expected_config_revision,
                account_id: account_id.as_str().to_owned(),
            },
            &command.context,
        )
        .await
        .map_err(|error| map_store_error(error, resource))?;
    provider.account_unavailable(&account_id).await;
    Ok(CredentialMutationResult {
        config_revision: revision,
        account_id,
        credential_revision: None,
    })
}
