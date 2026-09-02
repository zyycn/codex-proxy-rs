mod selection;

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value};

use gateway_core::account::{
    AccountAttemptFeedback, AccountCandidate, AccountConcurrencyLimit, AccountEligibilityPolicy,
    AccountFeedbackStats, AccountQuotaSignals, AccountRuntimeSignals, AccountSchedulingBlocker,
    AccountSelectionContext, AccountSelectionPolicy, AccountSelector, AccountStatus, AccountWeight,
    CredentialCasUpdate, CredentialRevision, CredentialState, OpaqueProviderData,
    PlaintextCredential, PreferredAccountSelection, ProviderAccount, ProviderAccountId,
    ProviderAccountIdentity, ProviderAccountUpdate, QuotaEvidence, QuotaState, RotationStrategy,
};
use gateway_core::routing::{
    ClientRoutingScope, FrozenAccountScope, ProviderKind, RuntimeAccount, RuntimeAccountDirectory,
};

fn account(id: &str) -> ProviderAccount {
    ProviderAccount::new(
        ProviderAccountId::new(id).expect("valid account"),
        ProviderKind::new("openai").expect("valid provider"),
        id.to_owned(),
        Some(format!("user-{id}")),
        "oauth".to_owned(),
        CredentialRevision::new(1).expect("valid revision"),
        Some(SystemTime::now() + Duration::from_secs(3600)),
    )
    .with_account_facts(
        true,
        CredentialState::Ready,
        QuotaState::unknown(),
        None,
        None,
    )
}

fn candidate(id: &str, in_flight: u32, remaining: Option<u64>) -> AccountCandidate {
    AccountCandidate {
        account: account(id),
        signals: AccountRuntimeSignals {
            in_flight,
            last_started_at: None,
            quota_reset_at: None,
            quota_remaining_rank: remaining,
            rate_limited_until: None,
            failure_rate_basis_points: None,
            first_output_latency_ms: None,
        },
    }
}

fn candidate_with_concurrency(id: &str, in_flight: u32, concurrency: u32) -> AccountCandidate {
    let mut candidate = candidate(id, in_flight, Some(100));
    candidate.account = candidate.account.with_scheduling(
        Some(AccountConcurrencyLimit::new(concurrency).expect("concurrency override")),
        AccountWeight::DEFAULT,
    );
    candidate
}

fn weighted_candidate(id: &str, weight: u16, in_flight: u32) -> AccountCandidate {
    let mut candidate = candidate(id, in_flight, None);
    candidate.account = candidate.account.with_scheduling(
        None,
        AccountWeight::new(weight).expect("valid account weight"),
    );
    candidate
}

#[test]
fn account_scheduling_should_default_and_resolve_concurrency_override() {
    let default = NonZeroU32::new(12).expect("default concurrency");
    let account = account("acct_scheduling_default");
    assert_eq!(account.concurrency_limit(), None);
    assert_eq!(account.weight(), AccountWeight::DEFAULT);
    assert_eq!(account.effective_concurrency(default), default);

    let limit = AccountConcurrencyLimit::new(7).expect("concurrency override");
    let weight = AccountWeight::new(80).expect("weight");
    let account = account.with_scheduling(Some(limit), weight);
    assert_eq!(account.concurrency_limit(), Some(limit));
    assert_eq!(account.weight(), weight);
    assert_eq!(account.effective_concurrency(default).get(), 7);
    assert!(AccountConcurrencyLimit::new(0).is_none());
    assert!(AccountWeight::new(0).is_none());
    assert!(AccountWeight::new(101).is_none());
}

fn context(strategy: RotationStrategy) -> AccountSelectionContext {
    AccountSelectionContext {
        policy: AccountSelectionPolicy::new(
            strategy,
            NonZeroU32::new(3).expect("positive"),
            Duration::ZERO,
        ),
        now: SystemTime::now(),
        excluded_accounts: BTreeSet::new(),
        preferred_account: None,
        round_robin_cursor: 0,
        eligibility: AccountEligibilityPolicy::Enforce,
        account_scope: None,
    }
}

#[test]
fn rotation_strategy_parse_should_round_trip_stable_wire_values() {
    for strategy in [
        RotationStrategy::Smart,
        RotationStrategy::QuotaResetPriority,
        RotationStrategy::RoundRobin,
        RotationStrategy::Sticky,
    ] {
        assert_eq!(RotationStrategy::parse(strategy.as_str()), Some(strategy));
    }
    assert_eq!(RotationStrategy::parse("unknown"), None);
}

#[test]
fn plaintext_credential_debug_should_redact_values() {
    let mut object = Map::new();
    object.insert("access_token".to_owned(), Value::from("secret-at"));
    let credential = PlaintextCredential::new(object);

    assert!(!format!("{credential:?}").contains("secret-at"));
}

#[test]
fn opaque_provider_data_should_not_expose_quota_values_in_debug() {
    let mut object = Map::new();
    object.insert("five_hour".to_owned(), Value::from("private-window"));
    let quota = OpaqueProviderData::new(object);

    assert!(!format!("{quota:?}").contains("private-window"));
}

#[test]
fn provider_account_identity_debug_should_redact_upstream_ids() {
    let identity = ProviderAccountIdentity::new(
        "private-user-id".to_owned(),
        Some("private-account-id".to_owned()),
    );
    let debug = format!("{identity:?}");

    assert!(!debug.contains("private-user-id"));
    assert!(!debug.contains("private-account-id"));
}

#[test]
fn account_fact_values_should_round_trip() {
    assert_eq!(
        CredentialState::parse(CredentialState::Banned.as_str()),
        Some(CredentialState::Banned)
    );
    assert_eq!(
        QuotaEvidence::parse("provider_denied"),
        Some(QuotaEvidence::ProviderDenied)
    );
}

#[test]
fn exhausted_quota_refresh_is_due_at_reset_or_provider_fallback() {
    let now = SystemTime::now();
    let observed_at = now - Duration::from_secs(60);
    let before_reset = QuotaState::exhausted(
        QuotaEvidence::ProviderDenied,
        observed_at,
        Some(now + Duration::from_secs(1)),
    );
    let after_reset = QuotaState::exhausted(
        QuotaEvidence::ProviderDenied,
        observed_at,
        Some(now - Duration::from_secs(1)),
    );
    let without_reset = QuotaState::exhausted(QuotaEvidence::ProviderDenied, observed_at, None);

    assert!(!before_reset.exhaustion_refresh_due(now, Duration::from_secs(30)));
    assert!(after_reset.exhaustion_refresh_due(now, Duration::from_secs(30)));
    assert!(without_reset.exhaustion_refresh_due(now, Duration::from_secs(30)));
    assert!(!without_reset.exhaustion_refresh_due(now, Duration::from_secs(120)));
}

#[test]
fn inconclusive_quota_observation_cannot_erase_confirmed_access() {
    let now = SystemTime::now();
    let exhausted = QuotaState::exhausted(QuotaEvidence::ProviderDenied, now, None);

    assert_eq!(
        exhausted
            .merge_observation(QuotaState::observed_unknown(now + Duration::from_secs(1)))
            .access(),
        gateway_core::account::QuotaAccessState::Exhausted
    );
    assert_eq!(
        exhausted
            .merge_observation(QuotaState::allowed(now + Duration::from_secs(1)))
            .access(),
        gateway_core::account::QuotaAccessState::Allowed
    );
}

#[test]
fn diagnostic_selection_bypasses_all_local_account_eligibility() {
    let observed_at = SystemTime::now();
    let exhausted = AccountCandidate {
        account: account("acct_exhausted").with_account_facts(
            true,
            CredentialState::Ready,
            QuotaState::exhausted(QuotaEvidence::ProviderDenied, observed_at, None),
            None,
            None,
        ),
        signals: AccountRuntimeSignals {
            in_flight: 0,
            last_started_at: None,
            quota_reset_at: None,
            quota_remaining_rank: None,
            rate_limited_until: None,
            failure_rate_basis_points: None,
            first_output_latency_ms: None,
        },
    };
    let disabled = AccountCandidate {
        account: account("acct_disabled").with_account_facts(
            false,
            CredentialState::Ready,
            QuotaState::exhausted(QuotaEvidence::ProviderDenied, observed_at, None),
            None,
            None,
        ),
        signals: AccountRuntimeSignals {
            in_flight: 0,
            last_started_at: None,
            quota_reset_at: None,
            quota_remaining_rank: None,
            rate_limited_until: None,
            failure_rate_basis_points: None,
            first_output_latency_ms: None,
        },
    };
    let mut context = context(RotationStrategy::Sticky);
    context.eligibility = AccountEligibilityPolicy::BypassForDiagnostic;

    assert_eq!(
        AccountSelector
            .select(std::slice::from_ref(&exhausted), &context)
            .map(|selection| selection.candidate().account.id()),
        Some(exhausted.account.id())
    );
    assert_eq!(
        AccountSelector
            .select(std::slice::from_ref(&disabled), &context)
            .map(|selection| selection.candidate().account.id()),
        Some(disabled.account.id())
    );
}

#[test]
fn selector_should_reject_a_preferred_account_outside_the_frozen_client_scope() {
    let allowed = candidate("acct_allowed", 0, None);
    let outside = candidate("acct_outside", 0, None);
    let directory = Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
        allowed.account.id().clone(),
        RuntimeAccount::new(
            ProviderKind::new("openai").expect("provider"),
            BTreeSet::new(),
        ),
    )])));
    let mut context = context(RotationStrategy::Sticky);
    context.preferred_account = Some(outside.account.id().clone());
    context.account_scope = Some(Arc::new(FrozenAccountScope::new(
        directory,
        ClientRoutingScope::all_accounts(),
    )));

    let candidates = [outside, allowed];
    let selection = AccountSelector
        .select(&candidates, &context)
        .expect("allowed account should remain eligible");
    assert_eq!(selection.candidate().account.id().as_str(), "acct_allowed");
    assert_eq!(
        selection.preferred(),
        PreferredAccountSelection::Blocked(AccountSchedulingBlocker::OutsideClientScope)
    );
}

#[test]
fn disabled_account_should_not_be_schedulable() {
    let account = account("acct_disabled").with_account_facts(
        false,
        CredentialState::Ready,
        QuotaState::unknown(),
        None,
        None,
    );

    assert_eq!(
        account.status_projection(SystemTime::now(), None).status,
        AccountStatus::Disabled
    );
}

#[test]
fn rate_limited_projection_carries_only_the_active_cooldown_deadline() {
    let now = SystemTime::now();
    let active_until = now + Duration::from_secs(60);
    let account = account("acct_rate_limited");

    let active = account.status_projection(now, Some(active_until));
    assert_eq!(active.status, AccountStatus::RateLimited);
    assert_eq!(active.rate_limited_until, Some(active_until));

    let elapsed = account.status_projection(now, Some(now - Duration::from_secs(1)));
    assert_eq!(elapsed.status, AccountStatus::Normal);
    assert_eq!(elapsed.rate_limited_until, None);
}

#[test]
fn account_without_upstream_identity_should_stay_unknown_and_not_schedulable() {
    let account = ProviderAccount::new(
        ProviderAccountId::new("acct_pending_identity").expect("valid account"),
        ProviderKind::new("openai").expect("valid provider"),
        "pending identity".to_owned(),
        None,
        "oauth".to_owned(),
        CredentialRevision::new(1).expect("valid revision"),
        Some(SystemTime::now() + Duration::from_secs(3600)),
    )
    .with_account_facts(
        true,
        CredentialState::Ready,
        QuotaState::unknown(),
        None,
        None,
    );

    assert_eq!(
        (
            account.credential_state(),
            account.status_projection(SystemTime::now(), None).status
        ),
        (CredentialState::Unknown, AccountStatus::Error)
    );
}

#[test]
fn smart_selector_should_prefer_lower_inflight_count() {
    let candidates = vec![
        candidate("acct_busy", 2, Some(100)),
        candidate("acct_idle", 0, Some(1)),
    ];
    let selected = AccountSelector
        .select(&candidates, &context(RotationStrategy::Smart))
        .expect("candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_idle");
}

#[test]
fn smart_selector_should_prefer_lower_capacity_utilization_over_lower_inflight_count() {
    let candidates = [
        candidate_with_concurrency("acct_smaller", 1, 2),
        candidate_with_concurrency("acct_larger", 2, 10),
    ];

    let selected = AccountSelector
        .select(&candidates, &context(RotationStrategy::Smart))
        .expect("candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_larger");
}

#[test]
fn selector_should_prioritize_the_highest_weight_for_every_rotation_strategy() {
    let candidates = vec![
        weighted_candidate("acct_low_weight", 1, 0),
        weighted_candidate("acct_high_weight", 100, 2),
    ];

    for strategy in [
        RotationStrategy::Smart,
        RotationStrategy::QuotaResetPriority,
        RotationStrategy::RoundRobin,
        RotationStrategy::Sticky,
    ] {
        let selected = AccountSelector
            .select(&candidates, &context(strategy))
            .expect("highest weight account should be eligible");
        assert_eq!(
            selected.candidate().account.id().as_str(),
            "acct_high_weight",
            "strategy {strategy:?} must select inside the highest weight tier"
        );
    }
}

#[test]
fn selector_should_not_let_soft_affinity_bypass_a_higher_weight_account() {
    let preferred = weighted_candidate("acct_preferred", 1, 0);
    let preferred_id = preferred.account.id().clone();
    let higher = weighted_candidate("acct_higher", 10, 0);
    let mut selection = context(RotationStrategy::Smart);
    selection.preferred_account = Some(preferred_id);
    let candidates = [preferred, higher];

    let selected = AccountSelector
        .select(&candidates, &selection)
        .expect("higher weight account should be eligible");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_higher");
    assert_eq!(
        selected.preferred(),
        PreferredAccountSelection::Blocked(AccountSchedulingBlocker::LowerWeight)
    );
}

#[test]
fn selector_should_fall_back_when_a_higher_weight_account_reaches_its_override() {
    let mut saturated = weighted_candidate("acct_high_weight", 100, 1);
    saturated.account = saturated.account.with_scheduling(
        Some(AccountConcurrencyLimit::new(1).expect("concurrency override")),
        AccountWeight::new(100).expect("weight"),
    );
    let fallback = weighted_candidate("acct_low_weight", 1, 0);
    let candidates = [saturated, fallback];

    let selected = AccountSelector
        .select(&candidates, &context(RotationStrategy::Smart))
        .expect("lower weight account should be used after saturation");

    assert_eq!(
        selected.candidate().account.id().as_str(),
        "acct_low_weight"
    );
}

#[test]
fn smart_selector_should_balance_signals_instead_of_using_lexicographic_load() {
    let mut healthy = candidate("acct_healthy", 1, Some(100));
    healthy.signals.failure_rate_basis_points = Some(0);
    healthy.signals.first_output_latency_ms = Some(100);
    let mut unhealthy = candidate("acct_unhealthy", 0, Some(0));
    unhealthy.signals.failure_rate_basis_points = Some(10_000);
    unhealthy.signals.first_output_latency_ms = Some(10_000);
    let candidates = vec![healthy, unhealthy];

    let selected = AccountSelector
        .select(&candidates, &context(RotationStrategy::Smart))
        .expect("candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_healthy");
}

#[test]
fn smart_selector_should_report_a_schedulable_preferred_account_hit() {
    let fallback = candidate("acct_fallback", 0, Some(100));
    let preferred = candidate("acct_preferred", 0, Some(1));
    let preferred_id = preferred.account.id().clone();
    let mut selection = context(RotationStrategy::Smart);
    selection.preferred_account = Some(preferred_id);
    let candidates = vec![fallback, preferred];

    let selected = AccountSelector
        .select(&candidates, &selection)
        .expect("candidate available");

    assert_eq!(
        (
            selected.candidate().account.id().as_str(),
            selected.preferred(),
        ),
        ("acct_preferred", PreferredAccountSelection::Hit)
    );
}

#[test]
fn smart_selector_should_keep_preferred_account_despite_failure_signals() {
    let mut fallback = candidate("acct_fallback", 0, Some(100));
    fallback.signals.failure_rate_basis_points = Some(0);
    fallback.signals.first_output_latency_ms = Some(100);
    let mut preferred = candidate("acct_preferred", 0, Some(100));
    preferred.signals.failure_rate_basis_points = Some(10_000);
    preferred.signals.first_output_latency_ms = Some(100);
    let preferred_id = preferred.account.id().clone();
    let mut selection = context(RotationStrategy::Smart);
    selection.preferred_account = Some(preferred_id);
    let candidates = vec![fallback, preferred];

    let selected = AccountSelector
        .select(&candidates, &selection)
        .expect("preferred candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_preferred");
}

#[test]
fn smart_selector_should_keep_preferred_account_despite_latency_signals() {
    let mut fallback = candidate("acct_fallback", 0, Some(100));
    fallback.signals.failure_rate_basis_points = Some(0);
    fallback.signals.first_output_latency_ms = Some(100);
    let mut preferred = candidate("acct_preferred", 0, Some(100));
    preferred.signals.failure_rate_basis_points = Some(0);
    preferred.signals.first_output_latency_ms = Some(120_000);
    let preferred_id = preferred.account.id().clone();
    let mut selection = context(RotationStrategy::Smart);
    selection.preferred_account = Some(preferred_id);
    let candidates = vec![fallback, preferred];

    let selected = AccountSelector
        .select(&candidates, &selection)
        .expect("preferred candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_preferred");
}

#[test]
fn smart_selector_should_escape_preferred_account_at_concurrency_limit() {
    let preferred = candidate("acct_preferred", 3, Some(100));
    let fallback = candidate("acct_fallback", 0, Some(1));
    let preferred_id = preferred.account.id().clone();
    let mut selection = context(RotationStrategy::Smart);
    selection.preferred_account = Some(preferred_id);
    let candidates = vec![preferred, fallback];

    let selected = AccountSelector
        .select(&candidates, &selection)
        .expect("fallback candidate available");

    assert_eq!(
        (
            selected.candidate().account.id().as_str(),
            selected.preferred(),
        ),
        (
            "acct_fallback",
            PreferredAccountSelection::Blocked(AccountSchedulingBlocker::ConcurrencyLimit),
        )
    );
}

#[test]
fn provider_quota_overlay_should_preserve_store_concurrency_facts() {
    let reset_at = SystemTime::now() + Duration::from_secs(60);
    let last_started_at = SystemTime::now();
    let signals = AccountRuntimeSignals {
        in_flight: 2,
        last_started_at: Some(last_started_at),
        quota_reset_at: None,
        quota_remaining_rank: None,
        rate_limited_until: None,
        failure_rate_basis_points: None,
        first_output_latency_ms: None,
    }
    .with_provider_quota(Some(AccountQuotaSignals::new(Some(reset_at), Some(75))));

    assert_eq!(
        (
            signals.in_flight,
            signals.last_started_at,
            signals.quota_reset_at,
            signals.quota_remaining_rank,
        ),
        (2, Some(last_started_at), Some(reset_at), Some(75))
    );
}

#[test]
fn account_feedback_should_be_shared_by_strategy_but_isolated_by_provider() {
    let feedback = AccountFeedbackStats::default();
    let openai = ProviderKind::new("openai").expect("provider");
    let xai = ProviderKind::new("xai").expect("provider");
    let account = ProviderAccountId::new("acct_shared_id").expect("account");

    feedback.report(
        &openai,
        &account,
        AccountAttemptFeedback::Failed {
            first_output_ms: Some(1_200),
        },
    );

    assert_eq!(
        feedback.scheduling_signals(&openai, &account),
        (Some(2_000), Some(1_200))
    );
    assert_eq!(feedback.scheduling_signals(&xai, &account), (None, None));
}

#[test]
fn account_feedback_should_decay_failure_rate_after_success() {
    let feedback = AccountFeedbackStats::default();
    let provider = ProviderKind::new("openai").expect("provider");
    let account = ProviderAccountId::new("acct_feedback").expect("account");
    feedback.report(
        &provider,
        &account,
        AccountAttemptFeedback::Failed {
            first_output_ms: None,
        },
    );
    feedback.report(
        &provider,
        &account,
        AccountAttemptFeedback::Succeeded {
            first_output_ms: Some(800),
        },
    );

    assert_eq!(
        feedback.scheduling_signals(&provider, &account),
        (Some(1_600), Some(800))
    );
}

#[test]
fn smart_selector_should_use_provider_quota_rank_after_load_is_equal() {
    let candidates = vec![
        candidate("acct_low_quota", 0, Some(20)),
        candidate("acct_high_quota", 0, Some(80)),
    ];
    let selected = AccountSelector
        .select(&candidates, &context(RotationStrategy::Smart))
        .expect("candidate available");

    assert_eq!(
        selected.candidate().account.id().as_str(),
        "acct_high_quota"
    );
}

#[test]
fn quota_reset_selector_should_prefer_known_earliest_window() {
    let now = SystemTime::now();
    let unknown = candidate("acct_unknown", 0, None);
    let mut later = candidate("acct_later", 0, None);
    later.signals.quota_reset_at = Some(now + Duration::from_secs(120));
    let mut earlier = candidate("acct_earlier", 0, None);
    earlier.signals.quota_reset_at = Some(now + Duration::from_secs(60));
    let candidates = vec![unknown, later, earlier];
    let selected = AccountSelector
        .select(&candidates, &context(RotationStrategy::QuotaResetPriority))
        .expect("candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_earlier");
}

#[test]
fn quota_reset_selector_should_use_capacity_utilization_as_load_tiebreaker() {
    let reset_at = SystemTime::now() + Duration::from_secs(60);
    let mut smaller = candidate_with_concurrency("acct_smaller", 1, 2);
    smaller.signals.quota_reset_at = Some(reset_at);
    let mut larger = candidate_with_concurrency("acct_larger", 2, 10);
    larger.signals.quota_reset_at = Some(reset_at);
    let candidates = [smaller, larger];

    let selected = AccountSelector
        .select(&candidates, &context(RotationStrategy::QuotaResetPriority))
        .expect("candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_larger");
}

#[test]
fn smart_selector_should_prefer_known_quota_over_unknown_after_load_is_equal() {
    let candidates = vec![
        candidate("acct_unknown", 0, None),
        candidate("acct_known", 0, Some(1)),
    ];
    let selected = AccountSelector
        .select(&candidates, &context(RotationStrategy::Smart))
        .expect("candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_known");
}

#[test]
fn selector_should_reject_account_at_concurrency_limit() {
    let candidates = vec![
        candidate("acct_full", 3, Some(100)),
        candidate("acct_available", 2, Some(1)),
    ];
    let selected = AccountSelector
        .select(&candidates, &context(RotationStrategy::Smart))
        .expect("candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_available");
}

#[test]
fn selector_should_enforce_request_interval_until_boundary() {
    let now = SystemTime::now();
    let mut cooling = candidate("acct_cooling", 0, Some(100));
    cooling.signals.last_started_at = Some(now - Duration::from_millis(9));
    let mut ready = candidate("acct_ready", 0, Some(1));
    ready.signals.last_started_at = Some(now - Duration::from_millis(10));
    let candidates = vec![cooling, ready];
    let mut context = context(RotationStrategy::Smart);
    context.now = now;
    context.policy = AccountSelectionPolicy::new(
        RotationStrategy::Smart,
        NonZeroU32::new(3).expect("positive"),
        Duration::from_millis(10),
    );
    let selected = AccountSelector
        .select(&candidates, &context)
        .expect("candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_ready");
}

#[test]
fn sticky_selector_should_fall_back_when_requested_account_is_excluded() {
    let candidates = vec![
        candidate("acct_sticky", 0, None),
        candidate("acct_fallback", 0, None),
    ];
    let mut context = context(RotationStrategy::Sticky);
    let sticky = ProviderAccountId::new("acct_sticky").expect("valid account");
    context.preferred_account = Some(sticky.clone());
    context.excluded_accounts.insert(sticky);
    let selected = AccountSelector
        .select(&candidates, &context)
        .expect("fallback candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_fallback");
}

#[test]
fn round_robin_selector_should_be_stable_for_unsorted_candidates() {
    let candidates = vec![
        candidate("acct_second", 0, None),
        candidate("acct_first", 0, None),
    ];
    let mut context = context(RotationStrategy::RoundRobin);
    context.round_robin_cursor = 0;
    let first = AccountSelector
        .select(&candidates, &context)
        .expect("first candidate available");
    context.round_robin_cursor = 1;
    let second = AccountSelector
        .select(&candidates, &context)
        .expect("second candidate available");

    assert_eq!(
        (
            first.candidate().account.id().as_str(),
            second.candidate().account.id().as_str(),
        ),
        ("acct_first", "acct_second")
    );
}

#[test]
fn round_robin_selector_should_honor_a_schedulable_preferred_account_before_the_cursor() {
    let candidates = vec![
        candidate("acct_first", 0, None),
        candidate("acct_second", 0, None),
    ];
    let mut context = context(RotationStrategy::RoundRobin);
    context.round_robin_cursor = 0;
    context.preferred_account =
        Some(ProviderAccountId::new("acct_second").expect("preferred account"));

    let selected = AccountSelector
        .select(&candidates, &context)
        .expect("preferred account available");

    assert_eq!(
        (
            selected.candidate().account.id().as_str(),
            selected.preferred(),
        ),
        ("acct_second", PreferredAccountSelection::Hit)
    );
}

#[test]
fn selector_should_honor_request_local_exclusion() {
    let candidates = vec![
        candidate("acct_first", 0, None),
        candidate("acct_second", 0, None),
    ];
    let mut context = context(RotationStrategy::Smart);
    context
        .excluded_accounts
        .insert(ProviderAccountId::new("acct_first").expect("valid account"));
    let selected = AccountSelector
        .select(&candidates, &context)
        .expect("second account available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_second");
}

#[test]
fn sticky_selector_should_prefer_requested_account() {
    let candidates = vec![
        candidate("acct_first", 0, None),
        candidate("acct_second", 0, None),
    ];
    let mut context = context(RotationStrategy::Sticky);
    context.preferred_account =
        Some(ProviderAccountId::new("acct_second").expect("valid sticky account"));
    let selected = AccountSelector
        .select(&candidates, &context)
        .expect("sticky account available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_second");
}

#[test]
fn sticky_selector_without_request_binding_should_reuse_latest_account() {
    let now = SystemTime::now();
    let mut older = candidate("acct_older", 0, Some(100));
    older.signals.last_started_at = Some(now - Duration::from_secs(20));
    let mut latest = candidate("acct_latest", 1, Some(1));
    latest.signals.last_started_at = Some(now - Duration::from_secs(10));
    let candidates = vec![older, latest];
    let selected = AccountSelector
        .select(&candidates, &context(RotationStrategy::Sticky))
        .expect("sticky candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_latest");
}

#[test]
fn round_robin_selector_should_use_frozen_cursor() {
    let candidates = vec![
        candidate("acct_first", 0, None),
        candidate("acct_second", 0, None),
    ];
    let mut context = context(RotationStrategy::RoundRobin);
    context.round_robin_cursor = 1;
    let selected = AccountSelector
        .select(&candidates, &context)
        .expect("candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_second");
}

#[test]
fn credential_cas_should_reject_profile_for_another_account() {
    let account_id = ProviderAccountId::new("acct_primary").expect("account id");
    let profile = ProviderAccountUpdate {
        account_id: ProviderAccountId::new("acct_other").expect("profile account id"),
        name: "other".to_owned(),
        email: None,
        plan_type: None,
    };
    assert!(
        CredentialCasUpdate::new(
            account_id,
            CredentialRevision::new(1).expect("revision"),
            profile,
            PlaintextCredential::new(Map::new()),
            false,
            Some(SystemTime::now() + Duration::from_secs(60)),
            None,
        )
        .is_err()
    );
}

#[test]
fn credential_cas_should_reject_refresh_schedule_without_refresh_token() {
    let account_id = ProviderAccountId::new("acct_primary").expect("account id");
    let profile = ProviderAccountUpdate {
        account_id: account_id.clone(),
        name: "primary".to_owned(),
        email: None,
        plan_type: None,
    };
    assert!(
        CredentialCasUpdate::new(
            account_id,
            CredentialRevision::new(1).expect("revision"),
            profile,
            PlaintextCredential::new(Map::new()),
            false,
            Some(SystemTime::now() + Duration::from_secs(60)),
            Some(SystemTime::now() + Duration::from_secs(30)),
        )
        .is_err()
    );
}

#[test]
fn enforced_eligibility_excludes_credential_errors_despite_quota_signals() {
    let mut banned = candidate("acct_banned", 0, Some(100));
    banned.account = banned.account.with_account_facts(
        true,
        CredentialState::Banned,
        QuotaState::unknown(),
        None,
        None,
    );
    let mut expired = candidate("acct_expired", 0, Some(100));
    expired.account = expired.account.with_account_facts(
        true,
        CredentialState::Expired,
        QuotaState::unknown(),
        None,
        None,
    );
    let mut invalid = candidate("acct_invalid", 0, Some(100));
    invalid.account = invalid.account.with_account_facts(
        true,
        CredentialState::Invalid,
        QuotaState::unknown(),
        None,
        None,
    );
    let healthy = candidate("acct_healthy", 0, Some(100));
    let candidates = vec![banned, expired, invalid, healthy];

    let selected = AccountSelector
        .select(&candidates, &context(RotationStrategy::Smart))
        .expect("healthy candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_healthy");
}

#[test]
fn enforced_eligibility_excludes_authoritative_quota_exhaustion() {
    let mut exhausted = candidate("acct_exhausted", 0, Some(100));
    exhausted.account = exhausted.account.with_account_facts(
        true,
        CredentialState::Ready,
        QuotaState::exhausted(QuotaEvidence::ProviderDenied, SystemTime::now(), None),
        None,
        None,
    );
    let healthy = candidate("acct_healthy", 0, Some(100));
    let candidates = vec![exhausted, healthy];

    let selected = AccountSelector
        .select(&candidates, &context(RotationStrategy::Smart))
        .expect("healthy candidate available");

    assert_eq!(selected.candidate().account.id().as_str(), "acct_healthy");
}

#[test]
fn elapsed_quota_reset_does_not_fabricate_recovery() {
    let now = SystemTime::now();
    let mut exhausted = candidate("acct_exhausted", 0, Some(100));
    exhausted.account = exhausted.account.with_account_facts(
        true,
        CredentialState::Ready,
        QuotaState::exhausted(
            QuotaEvidence::ProviderDenied,
            now - Duration::from_secs(2),
            Some(now - Duration::from_secs(1)),
        ),
        None,
        None,
    );
    let healthy = candidate("acct_healthy", 0, Some(100));

    assert_eq!(
        exhausted.account.status_projection(now, None).status,
        AccountStatus::QuotaExhausted
    );
    let candidates = [exhausted, healthy];
    let selected = AccountSelector
        .select(&candidates, &context(RotationStrategy::Smart))
        .expect("healthy candidate available");
    assert_eq!(selected.candidate().account.id().as_str(), "acct_healthy");
}
