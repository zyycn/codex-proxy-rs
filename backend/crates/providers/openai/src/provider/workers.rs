//! OpenAI Provider 向 Host 贡献的后台 worker

use super::*;
use crate::transport::profile::cli_release::CliReleaseService;
use crate::transport::profile::platform_release::PlatformDesktopReleaseService;
use gateway_core::account::OutboundProxy;
use gateway_core::provider_ports::ProviderRuntimePolicyPort;
use gateway_core::task::WorkerDaemonContext;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::transport::profile::ReleaseNetwork;

pub(crate) struct ClientReleaseServices {
    pub runtime_policy: Arc<dyn ProviderRuntimePolicyPort>,
    pub desktop_network: ReleaseNetwork,
    pub cli_network: ReleaseNetwork,
    pub platform_network: ReleaseNetwork,
    pub desktop: Arc<CodexDesktopReleaseService>,
    pub cli: Arc<CliReleaseService>,
    pub platforms: Arc<PlatformDesktopReleaseService>,
}

pub(super) const WORKER_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
pub(super) const WORKER_MAXIMUM_BACKOFF: Duration = Duration::from_secs(60);
pub(super) const WORKER_LEASE_TTL: Duration = Duration::from_secs(15 * 60);
pub(super) const WORKER_LEASE_RENEWAL: Duration = Duration::from_secs(5 * 60);
const RELEASE_PROXY_CHECK_INTERVAL: Duration = Duration::from_secs(30);

pub(super) const OAUTH_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
pub(super) const QUOTA_CHECK_INTERVAL: Duration = Duration::from_secs(30);
pub(super) const DESKTOP_RELEASE_WORKER_OWNER: &str = "openai-desktop-release";
pub(super) const MODEL_ETAG_WORKER_OWNER: &str = "openai-model-etag";
pub(super) const MODEL_CATALOG_WORKER_OWNER: &str = "openai-model-catalog";
pub(super) const WARMUP_WORKER_OWNER: &str = "openai-account-warmup";
pub(super) const WARMUP_CHECK_INTERVAL: Duration = Duration::from_secs(30);

pub(crate) fn worker_contributions(
    timezone: gateway_core::time::DeploymentTimeZone,
    refresh: Arc<CodexCredentialRefreshService>,
    quota: Arc<CodexCredentialQuotaService>,
    catalog: Arc<CodexCredentialCatalogService>,
    quota_refresh_policy: CodexQuotaRefreshPolicy,
    oauth_refresh_enabled: bool,
    releases: ClientReleaseServices,
) -> Result<Vec<WorkerContribution>, WorkerDefinitionError> {
    let refresh_id = WorkerId::try_new(WorkerKind::OAuthRefresh, PROVIDER_NAME)?;
    let quota_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, PROVIDER_NAME)?;
    let catalog_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, MODEL_CATALOG_WORKER_OWNER)?;
    let etag_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, MODEL_ETAG_WORKER_OWNER)?;
    let desktop_release_id =
        WorkerId::try_new(WorkerKind::QuotaCatalogHealth, DESKTOP_RELEASE_WORKER_OWNER)?;
    let cli_release_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, "openai-cli-release")?;
    let warmup_id = WorkerId::try_new(WorkerKind::QuotaCatalogHealth, WARMUP_WORKER_OWNER)?;
    let catalog_interval = quota_refresh_policy.interval();
    let mut contributions = Vec::new();
    if oauth_refresh_enabled {
        contributions.push(WorkerContribution::Registration(scheduled_registration(
            refresh_id,
            OAUTH_REFRESH_INTERVAL,
            Box::new(OpenAiOAuthRefreshTask {
                service: refresh,
                catalog: Arc::clone(&catalog),
            }),
        )?));
    }
    contributions.extend([
        WorkerContribution::Registration(scheduled_registration(
            WorkerId::try_new(
                WorkerKind::QuotaCatalogHealth,
                "openai-platform-desktop-release",
            )?,
            RELEASE_PROXY_CHECK_INTERVAL,
            Box::new(ReleaseCheckTask::new(
                Arc::clone(&releases.runtime_policy),
                releases.platform_network,
                Box::new(OpenAiPlatformDesktopReleaseTask {
                    service: releases.platforms,
                }),
            )),
        )?),
        WorkerContribution::Registration(scheduled_registration(
            cli_release_id,
            RELEASE_PROXY_CHECK_INTERVAL,
            Box::new(ReleaseCheckTask::new(
                Arc::clone(&releases.runtime_policy),
                releases.cli_network,
                Box::new(OpenAiCliReleaseTask {
                    service: releases.cli,
                }),
            )),
        )?),
        WorkerContribution::Registration(scheduled_registration(
            quota_id,
            QUOTA_CHECK_INTERVAL,
            Box::new(OpenAiQuotaTask {
                quota: Arc::clone(&quota),
            }),
        )?),
        WorkerContribution::Registration(scheduled_registration(
            warmup_id,
            WARMUP_CHECK_INTERVAL,
            Box::new(OpenAiWarmupTask::new(Arc::clone(&quota), timezone)),
        )?),
        WorkerContribution::Registration(scheduled_registration(
            catalog_id,
            catalog_interval,
            Box::new(OpenAiCatalogTask {
                catalog: Arc::clone(&catalog),
                interval: catalog_interval,
            }),
        )?),
        WorkerContribution::Registration(WorkerRegistration::try_new(
            etag_id,
            WorkerRunnable::Daemon {
                restart: DaemonRestartPolicy::try_new(
                    WORKER_INITIAL_BACKOFF,
                    WORKER_MAXIMUM_BACKOFF,
                )?,
                task: Box::new(OpenAiCatalogEtagTask { catalog }),
            },
        )?),
        WorkerContribution::Registration(scheduled_registration(
            desktop_release_id,
            RELEASE_PROXY_CHECK_INTERVAL,
            Box::new(ReleaseCheckTask::new(
                Arc::clone(&releases.runtime_policy),
                releases.desktop_network,
                Box::new(OpenAiDesktopReleaseTask {
                    service: releases.desktop,
                }),
            )),
        )?),
    ]);
    Ok(contributions)
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

pub(super) struct OpenAiOAuthRefreshTask {
    catalog: Arc<CodexCredentialCatalogService>,
    service: Arc<CodexCredentialRefreshService>,
}

impl ScheduledTask for OpenAiOAuthRefreshTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            let outcomes = self.service.refresh_due().await.map_err(|error| {
                WorkerTaskError::safe("OpenAI OAuth refresh failed").with_source(error)
            })?;
            let mut changed = Vec::new();
            let mut refreshed = 0_u64;
            let mut invalidated = 0_u64;
            let mut banned = 0_u64;
            let mut transient = 0_u64;
            let mut lease_unavailable = 0_u64;
            let mut stale = 0_u64;
            let mut failed = 0_u64;
            let mut transient_accounts = Vec::new();
            let mut failed_accounts = Vec::new();
            for outcome in &outcomes {
                match outcome {
                    CodexCredentialRefreshOutcome::Refreshed { account_id, .. } => {
                        refreshed += 1;
                        changed.push(account_id);
                    }
                    CodexCredentialRefreshOutcome::Invalidated { account_id } => {
                        invalidated += 1;
                        changed.push(account_id);
                    }
                    CodexCredentialRefreshOutcome::Banned { account_id } => {
                        banned += 1;
                        changed.push(account_id);
                    }
                    CodexCredentialRefreshOutcome::Transient { account_id } => {
                        transient += 1;
                        transient_accounts.push(account_id);
                    }
                    CodexCredentialRefreshOutcome::LeaseUnavailable { .. } => {
                        lease_unavailable += 1;
                    }
                    CodexCredentialRefreshOutcome::Stale { .. } => stale += 1,
                    CodexCredentialRefreshOutcome::Failed { account_id } => {
                        failed += 1;
                        failed_accounts.push(account_id);
                    }
                }
            }
            if !changed.is_empty() {
                let ids = changed
                    .into_iter()
                    .map(|id| gateway_core::account::ProviderAccountId::new(id.as_str()))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| {
                        WorkerTaskError::safe("OpenAI 账号 ID 无效").with_source(error)
                    })?;
                self.catalog
                    .invalidate_accounts(&ids)
                    .await
                    .map_err(|error| {
                        WorkerTaskError::safe("OpenAI 凭据变更后目录失效失败").with_source(error)
                    })?;
            }
            if !outcomes.is_empty() {
                tracing::info!(
                    refreshed,
                    invalidated,
                    banned,
                    transient,
                    lease_unavailable,
                    stale,
                    failed,
                    "OpenAI OAuth refresh cycle completed"
                );
            }
            if transient > 0 || failed > 0 {
                tracing::warn!(
                    refreshed,
                    invalidated,
                    banned,
                    transient,
                    lease_unavailable,
                    stale,
                    failed,
                    transient_accounts = ?transient_accounts,
                    failed_accounts = ?failed_accounts,
                    "OpenAI OAuth refresh cycle contained operational failures"
                );
            }
            Ok(())
        })
    }
}

pub(super) struct OpenAiQuotaTask {
    quota: Arc<CodexCredentialQuotaService>,
}

pub(super) struct OpenAiCatalogTask {
    catalog: Arc<CodexCredentialCatalogService>,
    /// 配置的目录刷新周期；用于成功周期尾部的随机抖动。
    interval: Duration,
}

pub(super) struct OpenAiCatalogEtagTask {
    catalog: Arc<CodexCredentialCatalogService>,
}

pub(super) struct OpenAiDesktopReleaseTask {
    service: Arc<CodexDesktopReleaseService>,
}

impl ScheduledTask for OpenAiDesktopReleaseTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let refresh = self.service.refresh();
            tokio::pin!(refresh);
            let result = tokio::select! {
                () = context.cancellation().cancelled() => return Ok(()),
                result = &mut refresh => result,
            };
            if let Err(error) = result {
                // 上游检查失败已经作为 Provider 观察事实保存；本周期本身正常完成，
                // 避免 Host 的短退避持续请求固定官方 appcast
                tracing::warn!(error = %error, "OpenAI Desktop release check failed");
            }
            Ok(())
        })
    }
}

impl ScheduledTask for OpenAiQuotaTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            match self.quota.synchronize().await {
                Ok(summary) if summary.has_operational_failures() => {
                    tracing::warn!(
                        updated = summary.updated,
                        exhausted = summary.exhausted,
                        banned = summary.banned,
                        transient = summary.transient,
                        stale = summary.stale,
                        "OpenAI quota cycle contained operational failures"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    return Err(WorkerTaskError::safe("OpenAI quota synchronization failed")
                        .with_source(error));
                }
            }
            Ok(())
        })
    }
}

impl ScheduledTask for OpenAiCatalogTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            let result = match self.catalog.refresh_catalogs().await {
                Ok(_) | Err(CodexCredentialCatalogError::NoEligibleCredential) => Ok(()),
                Err(error) => Err(WorkerTaskError::safe(
                    "OpenAI model catalog synchronization failed",
                )
                .with_source(error)),
            };
            // 成功周期尾部追加 [0, 20%) 单侧随机抖动，打破固定周期轮询特征；
            // 放在周期尾部避免延迟冷启动首轮刷新，失败路径交给宿主退避不叠加。
            // 抖动期间 leader lease 由宿主监督循环并发续租，不会超时。
            if result.is_ok() {
                let jitter = crate::jitter::catalog_refresh_jitter(
                    crate::jitter::random_u64(),
                    self.interval,
                );
                tokio::select! {
                    () = context.cancellation().cancelled() => {},
                    () = tokio::time::sleep(jitter) => {},
                }
            }
            result
        })
    }
}

impl DaemonTask for OpenAiCatalogEtagTask {
    fn run(&self, context: WorkerDaemonContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let cancellation = context.cancellation();
            loop {
                tokio::select! {
                    () = cancellation.cancelled() => return Ok(()),
                    () = self.catalog.wait_for_refresh() => {},
                };
                let result = tokio::select! {
                    ()=cancellation.cancelled()=>return Ok(()),
                    result=self.catalog.refresh_catalogs()=>result,
                };
                match result {
                    Ok(_) | Err(CodexCredentialCatalogError::NoEligibleCredential) => {}
                    Err(error) => {
                        self.catalog.schedule_refresh();
                        return Err(
                            WorkerTaskError::safe("OpenAI 模型目录刷新失败").with_source(error)
                        );
                    }
                }
                context.succeeded();
            }
        })
    }
}

struct OpenAiCliReleaseTask {
    service: Arc<CliReleaseService>,
}

impl ScheduledTask for OpenAiCliReleaseTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            tokio::select! {
                () = context.cancellation().cancelled() => {},
                result = self.service.refresh() => {
                    if let Err(error) = result { tracing::warn!(error = %error, "OpenAI CLI release check failed"); }
                }
            }
            Ok(())
        })
    }
}

struct OpenAiPlatformDesktopReleaseTask {
    service: Arc<PlatformDesktopReleaseService>,
}
impl ScheduledTask for OpenAiPlatformDesktopReleaseTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            tokio::select! {
                () = context.cancellation().cancelled() => {},
                () = self.service.refresh() => {},
            }
            Ok(())
        })
    }
}

pub(super) struct OpenAiWarmupTask {
    timezone: gateway_core::time::DeploymentTimeZone,
    quota: Arc<CodexCredentialQuotaService>,
}

impl OpenAiWarmupTask {
    pub(super) fn new(
        quota: Arc<CodexCredentialQuotaService>,
        timezone: gateway_core::time::DeploymentTimeZone,
    ) -> Self {
        Self { timezone, quota }
    }
}

impl ScheduledTask for OpenAiWarmupTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            if context.cancellation().is_cancelled() {
                return Ok(());
            }
            let policy = self
                .quota
                .runtime_policy()
                .load_warmup_policy()
                .await
                .map_err(|error| {
                    WorkerTaskError::safe("OpenAI warmup policy load failed").with_source(error)
                })?;
            if !policy.enabled() {
                return Ok(());
            }
            use chrono::Timelike as _;
            let now = chrono::Utc::now();
            let local_now = self.timezone.local(now);
            // 回拨产生的第二个相同时刻不能再次执行
            if self
                .timezone
                .resolve_local(local_now.naive_local())
                .is_none_or(|first| first != now)
            {
                return Ok(());
            }
            let slot = local_now
                .naive_local()
                .with_second(0)
                .and_then(|value| value.with_nanosecond(0))
                .ok_or_else(|| WorkerTaskError::safe("invalid warmup slot"))?;
            if !policy.matches_slot(slot) {
                return Ok(());
            }
            let Some(model) = policy.model() else {
                return Err(WorkerTaskError::safe("OpenAI warmup model is missing"));
            };
            if !self
                .quota
                .runtime_policy()
                .claim_warmup_slot(self.timezone, slot)
                .await
                .map_err(|source| {
                    WorkerTaskError::safe("warmup slot is unavailable").with_source(source)
                })?
            {
                return Ok(());
            }
            tracing::info!(%slot, model, "OpenAI account warmup cycle started");
            let outcome = tokio::select! {
                () = context.cancellation().cancelled() => return Ok(()),
                outcome = self.quota.execute_warmup(model) => outcome,
            };
            match outcome {
                Ok(summary) => {
                    tracing::info!(
                        warmed_up = summary.warmed_up,
                        skipped_active = summary.skipped_active,
                        skipped_exhausted = summary.skipped_exhausted,
                        failed = summary.failed,
                        "OpenAI account warmup cycle completed"
                    );
                }
                Err(error) => {
                    tracing::warn!(error = %error, "OpenAI account warmup cycle failed");
                }
            }
            Ok(())
        })
    }
}

struct ReleaseCheckTask {
    policy: Arc<dyn ProviderRuntimePolicyPort>,
    network: ReleaseNetwork,
    task: Box<dyn ScheduledTask>,
    last_check: Mutex<Option<(Option<OutboundProxy>, Instant)>>,
}

impl ReleaseCheckTask {
    fn new(
        policy: Arc<dyn ProviderRuntimePolicyPort>,
        network: ReleaseNetwork,
        task: Box<dyn ScheduledTask>,
    ) -> Self {
        Self {
            policy,
            network,
            task,
            last_check: Mutex::new(None),
        }
    }
}

impl ScheduledTask for ReleaseCheckTask {
    fn run_cycle(&self, context: WorkerCycleContext) -> BoxFuture<'_, Result<(), WorkerTaskError>> {
        Box::pin(async move {
            let mut last = self.last_check.lock().await;
            let proxy = tokio::select! {
                () = context.cancellation().cancelled() => return Ok(()),
                result = self.policy.load_client_release_proxy() => result,
            }
            .map_err(|error| {
                WorkerTaskError::safe("官方客户端版本检测代理不可用").with_source(error)
            })?;
            if last.as_ref().is_some_and(|(previous, checked)| {
                previous == &proxy && checked.elapsed() < APPCAST_POLL_INTERVAL
            }) {
                return Ok(());
            }
            // 同一任务的一轮检测固定出口，连接配置变更只影响下一轮
            self.network.configure(proxy.as_ref()).map_err(|error| {
                WorkerTaskError::safe("官方客户端版本检测代理配置失败").with_source(error)
            })?;
            self.task.run_cycle(context.clone()).await?;
            if !context.cancellation().is_cancelled() {
                *last = Some((proxy, Instant::now()));
            }
            Ok(())
        })
    }
}
