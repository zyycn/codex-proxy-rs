//! 从本地目录、凭据版本、额度和租约信号构建本轮候选

use super::AntigravityProvider;
use crate::transport::unsent;
use gateway_core::{
    account::{
        AccountCandidate, AccountSelectionContext, AccountStatus, CredentialRevision,
        ProviderAccountId,
    },
    engine::AttemptContext,
    error::{ProviderError, ProviderErrorKind as Kind},
    provider_ports::{ProviderConcurrencyPool, ProviderCooldownScope},
    routing::UpstreamModelId,
};
use std::{collections::BTreeMap, time::SystemTime};

pub(super) enum Project {
    Catalog(String),
    Discover,
}

pub(super) struct CandidatePool {
    pub(super) accounts: Vec<AccountCandidate>,
    pub(super) projects: BTreeMap<ProviderAccountId, (CredentialRevision, Project)>,
    pub(super) selection: AccountSelectionContext,
}

impl AntigravityProvider {
    pub(super) async fn load_candidates(
        &self,
        model: &UpstreamModelId,
        ctx: &AttemptContext,
    ) -> Result<CandidatePool, ProviderError> {
        let required = ctx.required_account();
        let diagnostic = ctx.is_diagnostic_required_account();
        let (accounts, projects) = if diagnostic {
            let id = required.ok_or_else(|| unsent(Kind::NoEligibleAccount))?;
            let account = self
                .accounts
                .get_account(id)
                .await
                .map_err(super::unavailable)?
                .filter(|account| account.provider() == &self.kind)
                .ok_or_else(|| unsent(Kind::NoEligibleAccount))?;
            let projects = BTreeMap::from([(id.clone(), (account.revision(), Project::Discover))]);
            (vec![account], projects)
        } else {
            let projects = self.catalog_projects(model, ctx).await?;
            let ids: Vec<_> = projects.keys().cloned().collect();
            if ids.is_empty() {
                return Err(unsent(Kind::NoEligibleAccount));
            }
            let accounts = self
                .accounts
                .list_for_provider_accounts(&self.kind, &ids)
                .await
                .map_err(super::unavailable)?;
            (accounts, projects)
        };
        let ids: Vec<_> = accounts
            .iter()
            .map(|account| account.id().clone())
            .collect();
        let mut quota = if diagnostic {
            BTreeMap::new()
        } else {
            self.quota
                .scheduling(&accounts, model.as_str())
                .await
                .map_err(super::unavailable)?
        };
        let scheduling = self
            .leases
            .load_state(
                ctx.client_api_key_ref(),
                &self.kind,
                &ids,
                ProviderConcurrencyPool::Shared,
            )
            .await
            .map_err(super::unavailable)?;
        let mut excluded = ctx.excluded_accounts().clone();
        if ctx.account_scope().is_some() {
            let scope = ProviderCooldownScope::upstream_model(model.clone());
            let (account_cooldowns, model_cooldowns) = tokio::join!(
                self.cooldowns.read_many(&ids),
                self.cooldowns.read_scoped_many(&ids, &scope)
            );
            let now = SystemTime::now();
            for ((account, local), model_cooldown) in
                accounts.iter().zip(account_cooldowns).zip(model_cooldowns)
            {
                let revision = account.revision();
                let local = local.map_err(super::unavailable)?;
                let model_cooldown = model_cooldown.map_err(super::unavailable)?;
                if local.is_some_and(|cooldown| {
                    cooldown.credential_revision() == revision
                        && cooldown.scheduling_state().is_active(now)
                }) || model_cooldown.is_some_and(|cooldown| {
                    cooldown.credential_revision() == revision && cooldown.until() > now
                }) {
                    excluded.insert(account.id().clone());
                }
            }
        }
        let now = SystemTime::now();
        let mut candidates = Vec::new();
        let mut model_exhausted = false;
        for account in accounts {
            let (exhausted, quota_signals) = quota.remove(account.id()).unwrap_or_default();
            if projects
                .get(account.id())
                .is_none_or(|(revision, _)| *revision != account.revision())
                || (!diagnostic && !account.model_access().allows(model.as_str()))
                || ctx.excludes_proxy(account.outbound_proxy())
            {
                continue;
            }
            if exhausted {
                model_exhausted |= !excluded.contains(account.id())
                    && matches!(
                        account.status_projection(now, None).status,
                        AccountStatus::Normal | AccountStatus::QuotaExhausted
                    );
                continue;
            }
            let (failure_rate, first_output_latency) =
                self.feedback.scheduling_signals(&self.kind, account.id());
            let signals = scheduling
                .signals()
                .get(account.id())
                .cloned()
                .ok_or_else(|| unsent(Kind::ProviderInfrastructureUnavailable))?
                .with_runtime_health(failure_rate, first_output_latency)
                .with_provider_quota(quota_signals);
            candidates.push(AccountCandidate {
                account,
                signals,
                inherited_concurrency: None,
            });
        }
        if candidates.is_empty() && model_exhausted {
            return Err(unsent(Kind::QuotaExhausted));
        }
        let selection = AccountSelectionContext {
            policy: ctx.account_selection_policy(),
            now,
            excluded_accounts: excluded,
            busy_accounts: Default::default(),
            preferred_account: required.cloned(),
            preferred_account_overrides_weight: false,
            round_robin_cursor: scheduling.round_robin_cursor(),
            eligibility: ctx.account_eligibility(),
            account_scope: ctx.account_scope().cloned(),
            reserved_concurrency: 0,
        };
        Ok(CandidatePool {
            accounts: candidates,
            projects,
            selection,
        })
    }

    async fn catalog_projects(
        &self,
        model: &UpstreamModelId,
        ctx: &AttemptContext,
    ) -> Result<BTreeMap<ProviderAccountId, (CredentialRevision, Project)>, ProviderError> {
        let required = ctx.required_account();
        let mut projects = BTreeMap::new();
        for (observations, catalog) in self.catalog.catalogs().await? {
            if !catalog.models.contains_key(model.as_str()) {
                continue;
            }
            for observation in observations {
                let account_id = observation.account_id;
                if required.is_none_or(|id| id == &account_id)
                    && ctx.account_scope().map_or(required.is_some(), |scope| {
                        scope.allows_model(&account_id, model.as_str())
                    })
                {
                    projects.insert(
                        account_id,
                        (
                            observation.revision,
                            Project::Catalog(catalog.project.clone()),
                        ),
                    );
                }
            }
        }
        Ok(projects)
    }
}
