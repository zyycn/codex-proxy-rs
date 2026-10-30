//! 凭据变更共享的绑定校验、事务结算、失效与提交后观察

use crate::{
    model::{
        AdminError, MutationContext,
        accounts::DeleteAccounts,
        provider_credentials::{
            AuthorizationMutationTarget, CredentialDeletion, CredentialDeletionResult,
            CredentialDetails, CredentialMutationResult, CredentialRotationCommit,
            PendingAuthorizationMutation, PreparedAuthorizationCommit,
            PreparedAuthorizationCredential, PreparedCredentialImport, PreparedCredentialRotation,
            ProviderQuotaRequest, StartAuthorization,
        },
    },
    ports::{provider::ProviderAdmin, store::AccountStore},
};
use gateway_core::{account::ProviderAccountId, routing::ProviderKind, runtime::SnapshotControl};

use super::{map_store_error, publish_committed};

pub(super) async fn publish_credentials_and_observe_quota(
    peer: Option<&dyn crate::ports::peer::PeerUpstreamService>,
    provider: &std::sync::Arc<dyn crate::ports::provider::ProviderAdmin>,
    snapshot: &dyn SnapshotControl,
    revision: crate::model::Revision,
    account_ids: &[ProviderAccountId],
    request_id: &str,
) -> Result<(), AdminError> {
    notify_account_facts_changed(peer, provider.as_ref(), account_ids).await;
    publish_committed(snapshot, revision).await?;

    // 凭据已经提交；额度只是可重建的观察，不能拖住管理请求或回滚提交结果
    let provider = provider.clone();
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
                    provider = %provider.provider_kind().as_str(),
                    account_id = %account_id.as_str(),
                    quota_error = ?error.kind(),
                    "initial quota observation failed"
                );
            }
        }
    });
    Ok(())
}

pub(super) async fn required_credential(
    accounts: &dyn AccountStore,
    provider_kind: &ProviderKind,
    account_id: &ProviderAccountId,
    resource: &'static str,
) -> Result<CredentialDetails, AdminError> {
    accounts
        .credential_details(provider_kind, account_id)
        .await
        .map_err(|error| map_store_error(error, resource))?
        .ok_or_else(|| AdminError::not_found("服务商凭据不存在"))
}

pub(super) async fn required_plugin_credential(
    accounts: &dyn AccountStore,
    account_id: &ProviderAccountId,
    resource: &'static str,
) -> Result<CredentialDetails, AdminError> {
    accounts
        .credential_details_by_id(account_id)
        .await
        .map_err(|error| map_store_error(error, resource))?
        .ok_or_else(|| AdminError::not_found("服务商凭据不存在"))
}

pub(super) async fn import_proxy_binding(
    proxies: &dyn crate::ports::proxy::ProxyStore,
    id: Option<&str>,
) -> Result<Option<crate::ports::proxy::ProxyImportReservation>, AdminError> {
    let Some(id) = id else { return Ok(None) };
    let reservation = proxies
        .reserve_import(id)
        .await
        .map_err(|error| map_store_error(error, "proxy"))?;
    Ok(Some(reservation))
}

pub(super) async fn pending_authorization(
    accounts: &dyn AccountStore,
    proxies: &dyn crate::ports::proxy::ProxyStore,
    provider_kind: &ProviderKind,
    command: &StartAuthorization,
    resource: &'static str,
) -> Result<PendingAuthorizationMutation, AdminError> {
    let proxy;
    let mut proxy_id = None;
    let target = match &command.reauthorization {
        Some(account_id) => {
            proxy = required_credential(accounts, provider_kind, account_id, resource)
                .await?
                .credential
                .effective_outbound_proxy()
                .cloned();
            AuthorizationMutationTarget::Reauthorize {
                account_id: account_id.clone(),
            }
        }
        None => {
            use crate::model::proxies::AccountProxySelection;
            proxy = match &command.outbound_proxy {
                Some(AccountProxySelection::Url(value)) => Some(value.clone()),
                Some(AccountProxySelection::Saved(id)) => {
                    let record = proxies
                        .get(id)
                        .await
                        .map_err(|error| map_store_error(error, "proxy"))?;
                    proxy_id = Some(record.id);
                    if !record.enabled {
                        return Err(AdminError::invalid("代理已禁用，请启用后再选择"));
                    }
                    Some(record.proxy)
                }
                Some(AccountProxySelection::Direct) | None => None,
            };
            AuthorizationMutationTarget::Create {
                name: command.name.clone(),
            }
        }
    };
    Ok(PendingAuthorizationMutation::new(
        provider_kind.clone(),
        target,
        crate::model::provider_credentials::AuthorizationOwnerBinding::from_context(
            &command.context,
        ),
    )
    .with_outbound_proxy(proxy)
    .with_outbound_proxy_id(proxy_id))
}

pub(super) fn validate_prepared_import(
    provider_kind: &ProviderKind,
    prepared: &PreparedCredentialImport,
    _resource: &'static str,
) -> Result<(), AdminError> {
    if prepared.provider_kind != *provider_kind
        || prepared
            .credentials
            .iter()
            .any(|credential| credential.provider_kind != *provider_kind)
    {
        return Err(AdminError::conflict("服务商返回的凭据与请求范围不符"));
    }
    Ok(())
}

pub(super) fn validate_prepared_rotation(
    account: &crate::model::accounts::AccountRecord,
    prepared: &PreparedCredentialRotation,
    _resource: &'static str,
) -> Result<(), AdminError> {
    validate_prepared_rotation_facts(account, prepared.facts())
}

pub(super) fn validate_prepared_rotation_facts(
    account: &crate::model::accounts::AccountRecord,
    facts: &crate::model::provider_credentials::PreparedCredentialRotationFacts,
) -> Result<(), AdminError> {
    if facts.account_id.as_str() != account.id.as_str()
        || facts.provider_kind != account.provider_kind
    {
        return Err(AdminError::conflict("凭据处理结果与当前凭据不符"));
    }
    Ok(())
}

pub(super) async fn validate_authorization_commit(
    provider_kind: &ProviderKind,
    context: &MutationContext,
    prepared: PreparedAuthorizationCommit,
    resource: &'static str,
) -> Result<PreparedAuthorizationCommit, AdminError> {
    if prepared.pending.provider_kind() != provider_kind
        || !prepared.pending.owner_binding().matches_context(context)
    {
        let validation_error = AdminError::conflict("OAuth 授权绑定已失效，请重新发起授权");
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
        let validation_error = AdminError::conflict("OAuth 凭据与待提交目标不一致");
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

pub(super) async fn commit_authorization(
    accounts: &dyn AccountStore,
    prepared: PreparedAuthorizationCommit,
    key: crate::model::provider_credentials::AuthorizationReceiptKey,
    settings: Option<crate::model::accounts::AccountImportSettings>,
    context: &MutationContext,
    resource: &'static str,
) -> Result<crate::model::provider_credentials::AuthorizationCommitResult, AdminError> {
    if settings.is_some()
        && matches!(
            &prepared.credential,
            PreparedAuthorizationCredential::Reauthorize(_)
        )
    {
        prepared.abort().await?;
        return Err(AdminError::invalid("重新授权不能修改账号设置"));
    }
    let crate::model::provider_credentials::AuthorizationCommitSettlement {
        command,
        credential_guard,
        authorization_guard,
    } = prepared.into_commit(settings, key);
    match accounts.commit_authorization(command, context).await {
        Ok(result) => {
            if let Some(guard) = credential_guard
                && result.newly_committed
            {
                guard.finish();
            }
            if let Some(guard) = authorization_guard
                && let Err(error) = guard.commit().await
            {
                // 账号与回执已经原子提交，Redis 清理失败不能把确定的成功改写为失败
                tracing::warn!(resource, settlement_error = %error, "authorization committed but pending cleanup failed");
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

pub(super) async fn commit_credential_rotation(
    accounts: &dyn AccountStore,
    prepared: PreparedCredentialRotation,
    settings: Option<crate::model::accounts::UpdateAccount>,
    context: &MutationContext,
    resource: &'static str,
) -> Result<CredentialMutationResult, AdminError> {
    let (facts, guard) = prepared.into_parts();
    match accounts
        .commit_credential_rotation(
            CredentialRotationCommit {
                prepared: facts,
                settings,
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

pub(super) async fn commit_credential_refresh(
    accounts: &dyn AccountStore,
    prepared: PreparedCredentialRotation,
    context: &MutationContext,
    resource: &'static str,
) -> Result<CredentialMutationResult, AdminError> {
    let (facts, guard) = prepared.into_parts();
    match accounts
        .commit_credential_refresh(
            CredentialRotationCommit {
                prepared: facts,
                settings: None,
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

pub(super) async fn delete_credentials(
    peer: Option<&dyn crate::ports::peer::PeerUpstreamService>,
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
        notify_account_unavailable(peer, provider, account_id).await;
    }
    notify_account_facts_changed(peer, provider, &account_ids).await;
    Ok(CredentialDeletionResult {
        config_revision: revision,
        account_ids,
    })
}

pub(super) async fn notify_account_facts_changed(
    peer: Option<&dyn crate::ports::peer::PeerUpstreamService>,
    provider: &dyn ProviderAdmin,
    ids: &[ProviderAccountId],
) {
    provider.account_facts_changed(ids).await;
    if let Some(peer) = peer {
        for id in ids {
            if let Err(error) = peer.account_changed(id).await {
                tracing::warn!(account_id = %id, error = %error, "upstream information plan reconciliation failed");
            }
        }
    }
}

pub(super) async fn notify_account_unavailable(
    peer: Option<&dyn crate::ports::peer::PeerUpstreamService>,
    provider: &dyn ProviderAdmin,
    id: &ProviderAccountId,
) {
    if let Some(peer) = peer {
        peer.forget(id);
    }
    provider.account_unavailable(id).await;
}
