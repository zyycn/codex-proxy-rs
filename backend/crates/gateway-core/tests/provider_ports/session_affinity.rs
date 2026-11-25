//! 通用亲和策略只影响选号和绑定，不覆盖资格或固定续接账号

use gateway_core::{
    account::{
        AccountAffinity, AccountCandidate, AccountEligibilityPolicy, AccountRuntimeSignals,
        AccountSelectionContext, AccountSelectionPolicy, AccountSelector, CredentialRevision,
        CredentialState, ProviderAccount, ProviderAccountId, QuotaState, RotationStrategy,
    },
    provider_ports::{
        ProviderSessionAffinityKey, ProviderSessionAffinityState, ProviderSessionBinding,
    },
    routing::ProviderKind,
};
use std::{
    num::NonZeroU32,
    time::{Duration, SystemTime},
};

fn candidate(id: &str, busy: bool) -> AccountCandidate {
    AccountCandidate {
        account: ProviderAccount::new(
            ProviderAccountId::new(id).unwrap(),
            ProviderKind::new("antigravity").unwrap(),
            id.into(),
            None,
            "oauth".into(),
            CredentialRevision::new(1).unwrap(),
            None,
        )
        .with_account_facts(
            true,
            CredentialState::Ready,
            QuotaState::unknown(),
            None,
            None,
        ),
        inherited_concurrency: None,
        signals: AccountRuntimeSignals {
            in_flight: u32::from(busy),
            last_started_at: None,
            quota_reset_at: None,
            quota_reset_windows: None,
            quota_remaining_rank: None,
            cooldown: None,
            failure_rate_basis_points: None,
            first_output_latency_ms: None,
        },
    }
}

fn context(mode: AccountAffinity) -> AccountSelectionContext {
    AccountSelectionContext {
        policy: AccountSelectionPolicy::new(RotationStrategy::RoundRobin, NonZeroU32::MIN)
            .with_account_affinity(mode)
            .with_session_affinity_ttl(Duration::from_secs(7200)),
        now: SystemTime::now(),
        excluded_accounts: Default::default(),
        busy_accounts: Default::default(),
        preferred_account: None,
        preferred_account_overrides_weight: false,
        round_robin_cursor: 0,
        eligibility: AccountEligibilityPolicy::Enforce,
        account_scope: None,
        reserved_concurrency: 0,
    }
}

fn binding() -> ProviderSessionBinding {
    ProviderSessionBinding::new(
        ProviderAccountId::new("acct_primary").unwrap(),
        "a".repeat(32),
    )
    .unwrap()
}

#[test]
fn strict_waits_only_while_primary_remains_eligible_and_queuing_is_enabled() {
    let key = ProviderSessionAffinityKey::try_new("session").unwrap();
    let mut context = context(AccountAffinity::Strict);
    let state = ProviderSessionAffinityState::new(&key, Some(binding()), context.policy);
    let candidates = [
        candidate("acct_primary", true),
        candidate("acct_backup", false),
    ];
    state.apply_preference(&mut context);
    assert_eq!(
        state.wait_account(&candidates, &context, true),
        state.bound_account()
    );
    assert!(state.wait_account(&candidates, &context, false).is_none());
    context
        .excluded_accounts
        .insert(ProviderAccountId::new("acct_primary").unwrap());
    assert!(state.wait_account(&candidates, &context, true).is_none());
}

#[test]
fn preferred_spillover_renews_primary_but_ineligible_binding_can_migrate() {
    let key = ProviderSessionAffinityKey::try_new("session").unwrap();
    let mut context = context(AccountAffinity::Preferred);
    let state = ProviderSessionAffinityState::new(&key, Some(binding()), context.policy);
    let candidates = [
        candidate("acct_primary", true),
        candidate("acct_backup", false),
    ];
    state.apply_preference(&mut context);
    let selected = AccountSelector
        .select(&candidates, &context)
        .unwrap()
        .candidate()
        .account
        .id();
    let claim = state.claim(
        selected,
        state.binding_eligible(&candidates, &context),
        false,
    );
    assert_eq!(claim.account, state.bound_account().unwrap());
    assert_eq!(claim.ttl, Duration::from_secs(7200));
    context
        .excluded_accounts
        .insert(ProviderAccountId::new("acct_primary").unwrap());
    assert_eq!(
        state
            .claim(
                selected,
                state.binding_eligible(&candidates, &context),
                false
            )
            .account,
        selected
    );
}

#[test]
fn relaxed_does_not_prefer_primary_and_admits_the_scheduled_account() {
    let key = ProviderSessionAffinityKey::try_new("session").unwrap();
    let mut context = context(AccountAffinity::Relaxed);
    let state = ProviderSessionAffinityState::new(&key, Some(binding()), context.policy);
    state.apply_preference(&mut context);
    assert!(context.preferred_account.is_none());
    let selected = ProviderAccountId::new("acct_backup").unwrap();
    assert_eq!(state.claim(&selected, true, false).account, &selected);
}

#[test]
fn native_pinned_account_keeps_its_preference_and_does_not_replace_primary() {
    let key = ProviderSessionAffinityKey::try_new("session").unwrap();
    let mut context = context(AccountAffinity::Strict);
    let state = ProviderSessionAffinityState::new(&key, Some(binding()), context.policy);
    let pinned = ProviderAccountId::new("acct_native_owner").unwrap();
    context.preferred_account = Some(pinned.clone());
    state.apply_preference(&mut context);
    assert_eq!(context.preferred_account.as_ref(), Some(&pinned));
    assert_eq!(
        state.claim(&pinned, false, true).account,
        state.bound_account().unwrap()
    );
}
