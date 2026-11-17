//! 管理端额度快照与历史请求观测的只读投影

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
        requests
            .iter()
            .zip(identities)
            .map(|(request, identity)| {
                let (id, revision) = identity?;
                let snapshot = if request.account.authentication_kind
                    == crate::credential::CODEX_AUTHENTICATION_KIND_API_KEY
                {
                    return Ok(empty_quota());
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
                    &request.account.quota_settings,
                ))
            })
            .collect()
    }
}

pub(super) fn history_window(
    document: &ProviderDocument,
    key: &str,
) -> Option<(crate::credential::CodexQuotaWindow, Option<String>)> {
    let headers = document
        .expose_to_provider()
        .expose_to_provider()
        .get("rateLimitHeaders")?;
    let headers: Vec<(String, String)> = serde::Deserialize::deserialize(headers).ok()?;
    let parsed = gateway_protocol::openai::events::parse_rate_limit_headers(&headers)?;
    let window = crate::credential::observed_quota_window(&parsed, key)?;
    Some((window, parsed.plan_type))
}

pub(super) fn history_reading(
    document: &ProviderDocument,
    key: &str,
) -> Option<gateway_admin::model::quota_history::QuotaReading> {
    let (window, plan_type) = history_window(document, key)?;
    Some(gateway_admin::model::quota_history::QuotaReading {
        plan_type,
        used_percent: window.used_percent()?,
        reset_at: window.reset_at(),
    })
}
