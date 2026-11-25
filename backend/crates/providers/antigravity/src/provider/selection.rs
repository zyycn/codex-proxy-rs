//! 协调 Antigravity 账号选择、并发排队和租约，取得租约后复核凭据

use super::{
    AntigravityProvider,
    candidates::{CandidatePool, Project},
};
use crate::{credential::Material, transport::unsent};
use gateway_core::{
    account::{
        AccountCapacitySnapshot, AccountSelector, PreferredAccountSelection, ProviderAccount,
        RotationStrategy,
    },
    concurrency::{CapacityWait, ConcurrencyQueuePolicy, QueueRejection},
    engine::{
        AttemptContext,
        policy::{AccountPolicyError, AccountScheduleOwnership},
    },
    error::{ProviderDiagnostic, ProviderError, ProviderErrorKind as Kind},
    provider_ports::{
        ProviderAccountAdmission, ProviderLeaseAcquisition, ProviderLeaseGuard,
        ProviderLeaseRequest, ProviderSchedulingLeaseRequest, ProviderSessionAffinityKey,
        ProviderSessionAffinityState,
    },
    routing::UpstreamModelId,
};
use std::time::Duration;

pub(super) struct SelectedAccount {
    pub(super) account: ProviderAccount,
    pub(super) capacity: Option<AccountCapacitySnapshot>,
    pub(super) material: Material,
    pub(super) project: Project,
    pub(super) lease: Box<dyn ProviderLeaseGuard>,
    pub(super) account_switch: bool,
}

#[derive(Debug, thiserror::Error)]
enum SelectionError {
    #[error(transparent)]
    Queue(#[from] QueueRejection),
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

impl AntigravityProvider {
    pub(super) async fn select_account(
        &self,
        model: &UpstreamModelId,
        ctx: &AttemptContext,
        affinity_key: Option<&ProviderSessionAffinityKey>,
    ) -> Result<SelectedAccount, ProviderError> {
        let policy = ctx.account_selection_policy();
        let select = |queue| self.select_with_queue(model, ctx, queue, affinity_key);
        let result = match select(policy.queue_policy()).await {
            Err(SelectionError::Queue(reason)) if ctx.required_account().is_none() => {
                policy
                    .reselect_after_queue_rejection(
                        reason,
                        ctx.deadline().remaining(),
                        select,
                        |error| match error {
                            SelectionError::Provider(error) => matches!(
                                error.kind(),
                                Kind::NoEligibleAccount | Kind::AccountCapacityUnavailable
                            ),
                            SelectionError::Queue(_) => false,
                        },
                    )
                    .await
            }
            result => result,
        };
        result.map_err(|error| match error {
            SelectionError::Queue(reason) => unsent(reason.provider_kind()),
            SelectionError::Provider(error) => error,
        })
    }

    async fn select_with_queue(
        &self,
        model: &UpstreamModelId,
        ctx: &AttemptContext,
        queue: ConcurrencyQueuePolicy,
        affinity_key: Option<&ProviderSessionAffinityKey>,
    ) -> Result<SelectedAccount, SelectionError> {
        let mut waiting = CapacityWait::new(
            &self.waiting,
            queue,
            ctx.deadline().at(),
            ctx.concurrency_wait_budget(),
        )
        .with_periodic_recheck(ctx.required_account().is_none());
        'refresh: loop {
            if ctx.cancellation().is_cancelled() {
                return Err(unsent(Kind::Cancelled).into());
            }
            if ctx.deadline().is_elapsed() {
                return Err(unsent(Kind::Timeout).into());
            }
            let CandidatePool {
                accounts,
                mut projects,
                mut selection,
            } = self.load_candidates(model, ctx).await?;
            let affinity = match affinity_key.filter(|_| !ctx.is_diagnostic_required_account()) {
                Some(key) => Some(ProviderSessionAffinityState::new(
                    key,
                    self.session_affinity
                        .load(&self.kind, key)
                        .await
                        .map_err(super::unavailable)?,
                    ctx.account_selection_policy(),
                )),
                None => None,
            };
            if let Some(affinity) = &affinity {
                affinity.apply_preference(&mut selection);
            }
            for candidate in &accounts {
                if !waiting.can_try(candidate.account.id()) {
                    selection
                        .busy_accounts
                        .insert(candidate.account.id().clone());
                }
            }
            let binding_eligible = affinity
                .as_ref()
                .is_some_and(|affinity| affinity.binding_eligible(&accounts, &selection));
            // 分配游标只读，排队重查和亲和命中不消耗新分配的位置
            let assignment_cursor = if selection.policy.strategy() == RotationStrategy::RoundRobin
                && ctx.required_account().is_none()
            {
                let cursor = self
                    .session_affinity
                    .round_robin_cursor(&self.kind, ctx.client_api_key_ref())
                    .await
                    .map_err(super::unavailable)?;
                selection.round_robin_cursor = cursor;
                Some(cursor)
            } else {
                None
            };
            let excluded = std::mem::take(&mut selection.excluded_accounts);
            let mut retry_after: Option<Duration> = None;
            loop {
                // 每次策略求值都从相同的授权、冷却和重试排除集合开始
                selection.excluded_accounts.clone_from(&excluded);
                let selected = ctx
                    .select_account(
                        &self.kind,
                        Some(model.as_str()),
                        &accounts,
                        &mut selection,
                        AccountScheduleOwnership {
                            bound_account: affinity
                                .as_ref()
                                .and_then(ProviderSessionAffinityState::bound_account),
                            pinned_account: ctx.required_account(),
                            ..Default::default()
                        },
                    )
                    .await;
                let choice = match selected {
                    Ok(Some(selected)) => {
                        if !selected.is_policy_choice()
                            && ctx.required_account().is_none()
                            && affinity
                                .as_ref()
                                .and_then(|affinity| {
                                    affinity.wait_account(
                                        &accounts,
                                        &selection,
                                        queue.max_waiting > 0,
                                    )
                                })
                                .is_some_and(|bound| bound != selected.candidate().account.id())
                        {
                            break;
                        }
                        selected
                    }
                    Ok(None) => break,
                    Err(AccountPolicyError::StaleCandidate) => continue 'refresh,
                    Err(AccountPolicyError::Filtered) => {
                        return Err(unsent(Kind::RequestPolicyDenied)
                            .with_diagnostic(
                                ProviderDiagnostic::new(AccountPolicyError::Filtered.to_string())
                                    .with_classification(
                                        "account_selection",
                                        "account_policy_filtered",
                                    ),
                            )
                            .into());
                    }
                    Err(AccountPolicyError::Rejected) => {
                        return Err(unsent(Kind::RequestPolicyDenied).into());
                    }
                    Err(AccountPolicyError::Fault) => {
                        return Err(unsent(Kind::ProviderInfrastructureUnavailable).into());
                    }
                };
                let candidate = choice.candidate();
                let account = &candidate.account;
                let acquire = ProviderSchedulingLeaseRequest::new(
                    self.kind.clone(),
                    account.id().clone(),
                    account.revision(),
                    selection.concurrency_limit(candidate),
                    ctx.deadline(),
                )
                .with_metadata(ctx.lease_metadata())
                .with_cancellation(ctx.cancellation().clone());
                match self
                    .leases
                    .try_acquire(ProviderLeaseRequest::Scheduling(acquire))
                    .await
                    .map_err(super::unavailable)?
                {
                    ProviderLeaseAcquisition::Acquired(lease) => {
                        let (_, project) = projects
                            .remove(account.id())
                            .ok_or_else(|| unsent(Kind::NoEligibleAccount))?;
                        let capacity = AccountSelector
                            .capacity_snapshot(&accounts, &selection)
                            .map(AccountCapacitySnapshot::with_acquired_request);
                        // 在认领迁移前保留原绑定事实，临时分流也按实际出站账号判断
                        let account_switch = affinity
                            .as_ref()
                            .and_then(ProviderSessionAffinityState::bound_account)
                            .is_some_and(|bound| bound != account.id())
                            || ctx
                                .account_state_owner()
                                .is_some_and(|owner| !owner.matches(&self.kind, account.id()));
                        let selected = self
                            .load_selected(account, project, lease, capacity, account_switch, ctx)
                            .await?;
                        let claim = affinity.as_ref().map(|affinity| {
                            affinity.claim(
                                account.id(),
                                binding_eligible,
                                ctx.required_account().is_some(),
                            )
                        });
                        // 保留主绑定的临时分流、亲和命中和插件显式选号均不占轮询位置
                        let admission = match assignment_cursor {
                            Some(cursor)
                                if !choice.is_policy_choice()
                                    && choice.preferred() != PreferredAccountSelection::Hit
                                    && claim.is_none_or(|claim| claim.account == account.id()) =>
                            {
                                Some(ProviderAccountAdmission::RoundRobin {
                                    client_key: ctx.client_api_key_ref(),
                                    cursor,
                                    session: claim,
                                })
                            }
                            _ => claim.map(ProviderAccountAdmission::Session),
                        };
                        if let Some(admission) = admission
                            && !self
                                .session_affinity
                                .admit_account(&self.kind, admission)
                                .await
                                .map_err(super::unavailable)?
                        {
                            selected.lease.release().await.map_err(super::unavailable)?;
                            tokio::task::yield_now().await;
                            continue 'refresh;
                        }
                        return Ok(selected);
                    }
                    ProviderLeaseAcquisition::Busy { retry_after: delay } => {
                        if let Some(delay) = delay {
                            retry_after =
                                Some(retry_after.map_or(delay, |current| current.min(delay)));
                        }
                        selection.busy_accounts.insert(account.id().clone());
                    }
                }
            }
            let mut keys = AccountSelector.wait_candidates(&accounts, &selection);
            if ctx.required_account().is_none()
                && let Some(bound) = affinity.as_ref().and_then(|affinity| {
                    affinity.wait_account(&accounts, &selection, queue.max_waiting > 0)
                })
            {
                keys.retain(|account| account == bound);
            }
            if keys.is_empty() {
                return Err(unsent(Kind::NoEligibleAccount).into());
            }
            if queue.max_waiting == 0 || ctx.is_diagnostic_required_account() {
                let mut error = unsent(Kind::AccountCapacityUnavailable);
                if let Some(delay) = retry_after {
                    error = error.with_retry_after(delay);
                }
                return Err(error.into());
            }
            tokio::select! {
                biased;
                () = ctx.cancellation().cancelled() => return Err(unsent(Kind::Cancelled).into()),
                result = AccountSelector.wait_for_capacity(&mut waiting, &keys, &accounts, &selection) => result?,
            }
        }
    }

    async fn load_selected(
        &self,
        account: &ProviderAccount,
        project: Project,
        lease: Box<dyn ProviderLeaseGuard>,
        capacity: Option<AccountCapacitySnapshot>,
        account_switch: bool,
        ctx: &AttemptContext,
    ) -> Result<SelectedAccount, ProviderError> {
        let current = self
            .credentials
            .current(account.id())
            .await
            .map_err(super::unavailable)?;
        if current.account.revision() != account.revision()
            || ctx.excludes_proxy(current.account.outbound_proxy())
        {
            return Err(unsent(Kind::NoEligibleAccount));
        }
        let material = self.credentials.material(&current)?;
        Ok(SelectedAccount {
            account: current.account,
            capacity,
            material,
            project,
            lease,
            account_switch,
        })
    }
}
