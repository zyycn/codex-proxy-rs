//! 管理端批量读取原生额度与互联快照，不在列表查询中访问上游

use super::*;

impl OpenAiAdminProvider {
    pub(super) async fn read_quota_snapshots(
        &self,
        requests: &[ProviderQuotaReadRequest<'_>],
    ) -> Vec<Result<ProviderQuota, ProviderAdminError>> {
        let identities = requests
            .iter()
            .map(|request| {
                if request.account.provider_kind != self.provider_kind {
                    return Err(provider_admin_error(ProviderAdminErrorKind::NotFound));
                }
                let id = ProviderAccountId::new(request.account.id.clone())
                    .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
                let revision =
                    CredentialRevision::new(request.account.credential_revision.get())
                        .map_err(|_| provider_admin_error(ProviderAdminErrorKind::Invalid))?;
                Ok((id, revision))
            })
            .collect::<Vec<_>>();
        let ids = requests
            .iter()
            .zip(&identities)
            .filter(|(request, _)| {
                request.account.authentication_kind
                    != crate::credential::CODEX_AUTHENTICATION_KIND_API_KEY
            })
            .filter_map(|(_, identity)| identity.as_ref().ok().map(|(id, _)| id.clone()))
            .collect::<Vec<_>>();
        let observations = if ids.is_empty() {
            Ok(BTreeMap::new())
        } else {
            self.accounts
                .get_quotas(&ids)
                .await
                .map_err(|error| map_quota_error(error.into()))
                .map(|values| {
                    values
                        .into_iter()
                        .map(|observation| (observation.account_id.clone(), observation))
                        .collect::<BTreeMap<_, _>>()
                })
        };
        let peer_ids = requests
            .iter()
            .zip(&identities)
            .filter(|(request, _)| {
                request.account.authentication_kind
                    == crate::credential::CODEX_AUTHENTICATION_KIND_API_KEY
            })
            .filter_map(|(_, identity)| identity.as_ref().ok().map(|(id, _)| id.clone()))
            .collect::<Vec<_>>();
        let peer_observations = if peer_ids.is_empty() {
            Ok(BTreeMap::new())
        } else {
            self.peer_info
                .read_summaries(&peer_ids)
                .await
                .map_err(map_store_error)
                .map(|records| {
                    records
                        .into_iter()
                        .map(|(connection, quota)| {
                            (connection.account_id.clone(), (connection, quota))
                        })
                        .collect::<BTreeMap<_, _>>()
                })
        };
        requests
            .iter()
            .zip(identities)
            .map(|(request, identity)| {
                let (id, revision) = identity?;
                let snapshot = if request.account.authentication_kind
                    == crate::credential::CODEX_AUTHENTICATION_KIND_API_KEY
                {
                    let connection = gateway_core::account::UpstreamConnection::new(
                        id.clone(),
                        revision,
                        request.account.effective_outbound_proxy(),
                    );
                    return Ok(peer_observations
                        .as_ref()
                        .map_err(Clone::clone)?
                        .get(&id)
                        .filter(|(observed, _)| *observed == connection)
                        .map_or_else(|| project_quota(None, false), |(_, quota)| quota.clone()));
                } else {
                    observations
                        .as_ref()
                        .map_err(Clone::clone)?
                        .get(&id)
                        .map(|observation| {
                            self.quota.project_observation(&id, revision, observation)
                        })
                        .transpose()
                        .map_err(map_quota_error)?
                        .flatten()
                };
                Ok(project_quota(
                    snapshot,
                    request.account.quota.is_exhausted(),
                ))
            })
            .collect()
    }
}
