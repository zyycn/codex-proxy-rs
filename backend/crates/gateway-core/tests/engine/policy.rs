use std::{
    collections::BTreeSet,
    num::NonZeroU32,
    sync::Arc,
    time::{Duration, SystemTime},
};

use futures::{executor::block_on, future::BoxFuture};
use gateway_core::{
    account::{
        AccountCandidate, AccountEligibilityPolicy, AccountRuntimeSignals, AccountSelectionContext,
        AccountSelectionPolicy, AccountWeight, CredentialRevision, CredentialState,
        PreferredAccountSelection, ProviderAccount, ProviderAccountId, QuotaState,
        RotationStrategy,
    },
    engine::{
        ModelRequestId,
        policy::{
            AccountPolicyError, AccountScheduleDecision, AccountScheduleInput, ModelRouteDecision,
            ModelRouteInput, RequestPolicyContext, RequestPolicyFault, RequestPolicyPlan,
        },
    },
    policy::ClientApiKeyId,
    routing::ProviderKind,
    runtime::extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference},
};

#[derive(Debug)]
struct FixedScheduler(Option<&'static str>);

impl RequestPolicyPlan for FixedScheduler {
    fn route_model(
        &self,
        _: ModelRouteInput,
    ) -> BoxFuture<'static, Result<ModelRouteDecision, RequestPolicyFault>> {
        Box::pin(async { Ok(ModelRouteDecision::Unhandled) })
    }

    fn schedule_account(
        &self,
        _: AccountScheduleInput,
    ) -> BoxFuture<'static, Result<AccountScheduleDecision, RequestPolicyFault>> {
        let decision = self.0.map_or(AccountScheduleDecision::Delegate, |id| {
            AccountScheduleDecision::Pick(account_id(id))
        });
        Box::pin(async move { Ok(decision) })
    }
}

struct Lease;
impl ExtensionSetLease for Lease {
    fn is_ready(&self) -> bool {
        true
    }
}

fn account_id(id: &str) -> ProviderAccountId {
    ProviderAccountId::new(id.to_owned()).unwrap()
}

fn candidate(id: &str) -> AccountCandidate {
    AccountCandidate {
        account: ProviderAccount::new(
            account_id(id),
            ProviderKind::new("openai").unwrap(),
            id.into(),
            None,
            "test".into(),
            CredentialRevision::new(1).unwrap(),
            None,
        )
        .with_account_facts(
            true,
            CredentialState::Ready,
            QuotaState::unknown(),
            None,
            None,
        )
        .with_scheduling(None, AccountWeight::new(100).unwrap()),
        signals: AccountRuntimeSignals {
            in_flight: 0,
            last_started_at: None,
            quota_reset_at: None,
            quota_remaining_rank: None,
            cooldown: None,
            failure_rate_basis_points: None,
            first_output_latency_ms: None,
        },
    }
}

fn policy(choice: Option<&'static str>) -> RequestPolicyContext {
    RequestPolicyContext::new(
        Arc::new(FixedScheduler(choice)),
        ExtensionSetReference::new(
            ExtensionSetId::new("scheduler-test".into()).unwrap(),
            Arc::new(Lease),
        ),
        ModelRequestId::new("req_scheduler").unwrap(),
        ClientApiKeyId::new("scheduler-key").unwrap(),
        vec![],
    )
}

fn context() -> AccountSelectionContext {
    AccountSelectionContext {
        policy: AccountSelectionPolicy::new(
            RotationStrategy::Sticky,
            NonZeroU32::new(2).unwrap(),
            Duration::ZERO,
        ),
        now: SystemTime::now(),
        excluded_accounts: BTreeSet::new(),
        preferred_account: Some(account_id("acct_a")),
        preferred_account_overrides_weight: true,
        round_robin_cursor: 0,
        eligibility: AccountEligibilityPolicy::Enforce,
        account_scope: None,
    }
}

#[test]
fn scheduler_can_override_soft_affinity_and_reports_the_override() {
    block_on(async {
        let candidates = [candidate("acct_a"), candidate("acct_b")];
        let selected = policy(Some("acct_b"))
            .select_account(
                NonZeroU32::new(1).unwrap(),
                &ProviderKind::new("openai").unwrap(),
                Some("demo-model"),
                &candidates,
                &context(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                selected.candidate().account.id().as_str(),
                selected.preferred()
            ),
            ("acct_b", PreferredAccountSelection::OverriddenByPolicy)
        );
    });
}

#[test]
fn delegated_scheduler_keeps_builtin_affinity() {
    block_on(async {
        let candidates = [candidate("acct_a"), candidate("acct_b")];
        let selected = policy(None)
            .select_account(
                NonZeroU32::new(1).unwrap(),
                &ProviderKind::new("openai").unwrap(),
                None,
                &candidates,
                &context(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                selected.candidate().account.id().as_str(),
                selected.preferred()
            ),
            ("acct_a", PreferredAccountSelection::Hit)
        );
    });
}

#[test]
fn smart_switchback_does_not_override_explicit_plugin_choices_or_their_telemetry() {
    block_on(async {
        let mut candidates = [candidate("acct_a"), candidate("acct_b")];
        candidates[0].account = candidates[0]
            .account
            .clone()
            .with_scheduling(None, AccountWeight::new(10).unwrap());
        let mut selection_context = context();
        selection_context.policy = AccountSelectionPolicy::new(
            RotationStrategy::Smart,
            NonZeroU32::new(2).unwrap(),
            Duration::ZERO,
        )
        .with_smart_scheduling(
            gateway_core::account::SmartSchedulingConfig::new([1.0, 0.8, 1.0, 0.5, 0.0, 0.0], true)
                .unwrap(),
        );
        for chosen in [Some("acct_a"), Some("acct_b"), None] {
            let selected = policy(chosen)
                .select_account(
                    NonZeroU32::new(1).unwrap(),
                    &ProviderKind::new("openai").unwrap(),
                    None,
                    &candidates,
                    &selection_context,
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                selected.candidate().account.id().as_str(),
                chosen.unwrap_or("acct_b")
            );
            let expected = match chosen {
                Some("acct_a") => PreferredAccountSelection::Hit,
                Some(_) => PreferredAccountSelection::OverriddenByPolicy,
                None => PreferredAccountSelection::Blocked(
                    gateway_core::account::AccountSchedulingBlocker::LowerWeight,
                ),
            };
            assert_eq!(selected.preferred(), expected);
        }
    });
}

#[test]
fn scheduler_cannot_select_outside_the_hard_bound_candidate_pool() {
    block_on(async {
        let candidates = [candidate("acct_a"), candidate("acct_b")];
        let mut context = context();
        context.excluded_accounts.insert(account_id("acct_b"));
        let result = policy(Some("acct_b"))
            .select_account(
                NonZeroU32::new(1).unwrap(),
                &ProviderKind::new("openai").unwrap(),
                None,
                &candidates,
                &context,
            )
            .await;
        assert!(matches!(result, Err(AccountPolicyError::Fault)));
    });
}
