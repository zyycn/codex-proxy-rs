//! 账号查询合并原生额度与公共互联投影，保持列表摘要和详情窗口边界

use crate::{
    model::{
        accounts::AccountRecord,
        provider_credentials::{ProviderQuota, ProviderQuotaReadRequest, ProviderQuotaRequest},
    },
    ports::provider::{ProviderAdmin, ProviderAdminError, ProviderAdminErrorKind},
};
use gateway_core::account::{CredentialRevision, ProviderAccountId, UpstreamConnection};

fn connection(account: &AccountRecord) -> Result<UpstreamConnection, ProviderAdminError> {
    let invalid = || ProviderAdminError::new(ProviderAdminErrorKind::Invalid);
    Ok(UpstreamConnection::new(
        ProviderAccountId::new(account.id.clone()).map_err(|_| invalid())?,
        CredentialRevision::new(account.credential_revision.get()).map_err(|_| invalid())?,
        account.effective_outbound_proxy(),
    ))
}

pub(crate) fn store_error(error: gateway_core::error::StoreError) -> ProviderAdminError {
    use gateway_core::error::StoreErrorKind;
    let kind = match error.kind() {
        StoreErrorKind::Conflict => ProviderAdminErrorKind::Conflict,
        StoreErrorKind::InvalidData | StoreErrorKind::InvalidState => {
            ProviderAdminErrorKind::NotFound
        }
        StoreErrorKind::Unavailable => ProviderAdminErrorKind::Unavailable,
        _ => ProviderAdminErrorKind::Internal,
    };
    ProviderAdminError::new(kind).with_source(error)
}

pub(crate) async fn read_quota(
    peer: Option<&dyn crate::ports::peer::PeerUpstreamService>,
    provider: &dyn ProviderAdmin,
    account: &AccountRecord,
    request: ProviderQuotaRequest,
) -> Result<ProviderQuota, ProviderAdminError> {
    if !request.refresh
        && provider.supports_peer_connection(&account.authentication_kind)
        && let Some(peer) = peer
    {
        return peer
            .read_quota(&request.account_id)
            .await
            .map(|quota| quota.unwrap_or_else(ProviderQuota::default))
            .map_err(store_error);
    }
    provider.quota(request).await
}

pub(crate) async fn read_quotas(
    peer: Option<&dyn crate::ports::peer::PeerUpstreamService>,
    provider: &dyn ProviderAdmin,
    requests: &[ProviderQuotaReadRequest<'_>],
) -> Vec<Result<ProviderQuota, ProviderAdminError>> {
    let mut results = provider.read_quotas(requests).await;
    let Some(peer) = peer else {
        return results;
    };
    if results.len() != requests.len() {
        return results;
    }
    let indices = requests
        .iter()
        .enumerate()
        .filter_map(|(index, request)| {
            provider
                .supports_peer_connection(&request.account.authentication_kind)
                .then_some(index)
        })
        .collect::<Vec<_>>();
    let queries = indices
        .iter()
        .map(|&index| ProviderQuotaReadRequest {
            account: requests[index].account,
            rolling_usage: requests[index].rolling_usage,
        })
        .collect::<Vec<_>>();
    for (index, quota) in indices.into_iter().zip(peer.read_quotas(&queries).await) {
        results[index] = quota;
    }
    results
}

/// 列表只加载轻量摘要，连接身份过期时丢弃旧观测
pub(super) async fn read_peer_quotas(
    service: &super::PeerInfoService,
    requests: &[ProviderQuotaReadRequest<'_>],
) -> Vec<Result<ProviderQuota, ProviderAdminError>> {
    let connections = requests
        .iter()
        .map(|request| connection(request.account))
        .collect::<Vec<_>>();
    let ids = connections
        .iter()
        .filter_map(|connection| {
            connection
                .as_ref()
                .ok()
                .map(|value| value.account_id.clone())
        })
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return connections
            .into_iter()
            .map(|connection| connection.map(|_| ProviderQuota::default()))
            .collect();
    }
    let summaries = service
        .read_summaries(&ids)
        .await
        .map(|records| {
            records
                .into_iter()
                .map(|(connection, quota)| (connection.account_id.clone(), (connection, quota)))
                .collect::<std::collections::BTreeMap<_, _>>()
        })
        .map_err(store_error);
    connections
        .into_iter()
        .map(|connection| {
            connection.and_then(|connection| {
                Ok(summaries
                    .as_ref()
                    .map_err(Clone::clone)?
                    .get(&connection.account_id)
                    .filter(|(observed, _)| *observed == connection)
                    .map_or_else(ProviderQuota::default, |(_, quota)| quota.clone()))
            })
        })
        .collect()
}
