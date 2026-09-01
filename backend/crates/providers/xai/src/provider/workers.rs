//! xAI Provider 向 Host 贡献的后台 worker。

use super::*;

pub(super) const WORKER_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
pub(super) const WORKER_MAXIMUM_BACKOFF: Duration = Duration::from_secs(60);
pub(super) const WORKER_LEASE_TTL: Duration = Duration::from_secs(15 * 60);
pub(super) const WORKER_LEASE_RENEWAL: Duration = Duration::from_secs(5 * 60);
pub(super) const OAUTH_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
pub(super) const QUOTA_CATALOG_INTERVAL: Duration = Duration::from_secs(5 * 60);
// rolling 24h 描述的是上游用量窗口，不代表从本次观测起封禁 24 小时；
// 缺少可信 reset 时间时按短周期探测策略恢复检查。
pub(super) const EXHAUSTED_QUOTA_FALLBACK_RECHECK_INTERVAL: Duration = Duration::from_secs(10 * 60);
pub(super) const EXHAUSTED_QUOTA_REFRESH_RETRY_INTERVAL: Duration = QUOTA_CATALOG_INTERVAL;
pub(super) const CLI_RELEASE_WORKER_OWNER: &str = "xai-cli-release";

pub(crate) fn worker_contributions(
    refresh: Arc<GrokCredentialRefreshService>,
    quota: Arc<GrokCredentialQuotaService>,
    catalog: Arc<GrokCredentialCatalogService>,
    accounts: Arc<dyn ProviderAccountStore>,
    provider_kind: ProviderKind,
    cli_release: Arc<GrokCliReleaseService>,
) -> Result<Vec<WorkerContribution>, WorkerDefinitionError> {
    let refresh_id = WorkerId::try_new(WorkerKind::OAuthRefresh, XAI_PROVIDER_NAME)?;
    let catalog_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, XAI_PROVIDER_NAME)?;
    let release_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, CLI_RELEASE_WORKER_OWNER)?;
    Ok(vec![
        WorkerContribution::Registration(scheduled_registration(
            refresh_id,
            OAUTH_REFRESH_INTERVAL,
            Box::new(XaiOAuthRefreshTask { service: refresh }),
        )?),
        WorkerContribution::Registration(scheduled_registration(
            catalog_id,
            QUOTA_CATALOG_INTERVAL,
            Box::new(XaiQuotaCatalogTask {
                accounts,
                quota,
                catalog,
                provider_kind,
                last_periodic_refresh_at: Mutex::new(BTreeMap::new()),
            }),
        )?),
        WorkerContribution::Registration(scheduled_registration(
            release_id,
            GROK_CLI_RELEASE_POLL_INTERVAL,
            Box::new(XaiCliReleaseTask {
                service: cli_release,
            }),
        )?),
    ])
}

pub(super) fn scheduled_registration(
    id: WorkerId,
    interval: Duration,
    task: Box<dyn ScheduledTask>,
) -> Result<WorkerRegistration, WorkerDefinitionError> {
    let schedule = WorkerSchedule::try_new(
        interval,
        WORKER_INITIAL_BACKOFF,
        WORKER_MAXIMUM_BACKOFF,
        WORKER_LEASE_TTL,
        WORKER_LEASE_RENEWAL,
    )?;
    let lease = WorkerLeaseRequest::try_new(id.clone(), WORKER_LEASE_TTL)?;
    WorkerRegistration::try_new(
        id,
        WorkerRunnable::Scheduled {
            schedule,
            lease: Some(lease),
            task,
        },
    )
}

pub(super) struct XaiOAuthRefreshTask {
    service: Arc<GrokCredentialRefreshService>,
}

pub(super) struct XaiCliReleaseTask {
    service: Arc<GrokCliReleaseService>,
}

impl ScheduledTask for XaiCliReleaseTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let refresh = self.service.refresh();
            tokio::pin!(refresh);
            let result = tokio::select! {
                () = context.cancellation().cancelled() => return Ok(()),
                result = &mut refresh => result,
            };
            if let Err(error) = result {
                tracing::warn!(error = %error, "xAI CLI release check failed");
            }
            Ok(())
        })
    }
}

impl ScheduledTask for XaiOAuthRefreshTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            let outcomes = self.service.refresh_due().await.map_err(|error| {
                tracing::error!(error = %error, "xAI OAuth refresh cycle failed");
                WorkerTaskError::safe("xAI OAuth refresh failed")
            })?;
            let failures = outcomes
                .iter()
                .filter(|outcome| {
                    matches!(
                        outcome,
                        GrokCredentialRefreshOutcome::Ambiguous { .. }
                            | GrokCredentialRefreshOutcome::Transient { .. }
                            | GrokCredentialRefreshOutcome::Failed { .. }
                    )
                })
                .count();
            if !outcomes.is_empty() {
                tracing::info!(
                    accounts = outcomes.len(),
                    failures,
                    "xAI OAuth refresh cycle completed"
                );
            }
            if failures > 0 {
                tracing::warn!(failures, "xAI OAuth refresh cycle contained failures");
            }
            Ok(())
        })
    }
}

pub(super) struct XaiQuotaCatalogTask {
    accounts: Arc<dyn ProviderAccountStore>,
    quota: Arc<GrokCredentialQuotaService>,
    catalog: Arc<GrokCredentialCatalogService>,
    provider_kind: ProviderKind,
    last_periodic_refresh_at: Mutex<BTreeMap<ProviderAccountId, Instant>>,
}

impl ScheduledTask for XaiQuotaCatalogTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let accounts = self
                .accounts
                .list_for_provider(&self.provider_kind)
                .await
                .map_err(|_| WorkerTaskError::safe("xAI Provider accounts unavailable"))?;
            let mut failures = 0_u64;
            let now = SystemTime::now();
            let accounts = self.reserve_periodic_refreshes(accounts, now);
            for account in accounts {
                if context.cancellation().is_cancelled() {
                    return Ok(());
                }
                match self.quota.refresh_account(account.id()).await {
                    Ok(_) | Err(GrokQuotaError::AccountUnavailable) => {}
                    Err(_) => failures = failures.saturating_add(1),
                }
            }
            match self.catalog.query_models().await {
                Ok(_) | Err(GrokCredentialCatalogError::NoEligibleCredential) => {}
                Err(_) => failures = failures.saturating_add(1),
            }
            if failures == 0 {
                Ok(())
            } else {
                Err(WorkerTaskError::safe(
                    "xAI quota or catalog synchronization failed",
                ))
            }
        })
    }
}

impl XaiQuotaCatalogTask {
    fn reserve_periodic_refreshes(
        &self,
        accounts: Vec<ProviderAccount>,
        now: SystemTime,
    ) -> Vec<ProviderAccount> {
        let candidates = accounts
            .into_iter()
            .filter(|account| eligible_quota_worker_account(account, now))
            .collect::<Vec<_>>();
        let candidate_ids = candidates
            .iter()
            .map(|account| account.id().clone())
            .collect::<BTreeSet<_>>();
        let now = Instant::now();
        let mut last_periodic_refresh_at = self
            .last_periodic_refresh_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        last_periodic_refresh_at.retain(|account_id, _| candidate_ids.contains(account_id));
        candidates
            .into_iter()
            .filter(|account| {
                let due = last_periodic_refresh_at
                    .get(account.id())
                    .is_none_or(|last| {
                        now.saturating_duration_since(*last)
                            >= EXHAUSTED_QUOTA_REFRESH_RETRY_INTERVAL
                    });
                if due {
                    last_periodic_refresh_at.insert(account.id().clone(), now);
                }
                due
            })
            .collect()
    }
}

pub(super) fn eligible_quota_worker_account(account: &ProviderAccount, now: SystemTime) -> bool {
    account.enabled()
        && account
            .access_token_expires_at()
            .is_some_and(|expires_at| expires_at > now)
        && account
            .quota()
            .exhaustion_refresh_due(now, EXHAUSTED_QUOTA_FALLBACK_RECHECK_INTERVAL)
}
