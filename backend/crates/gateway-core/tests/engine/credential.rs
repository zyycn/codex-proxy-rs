use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value};

use gateway_core::engine::credential::{
    AccountAttemptFeedback, AccountAvailability, AccountCandidate, AccountFeedbackStats,
    AccountQuotaSignals, AccountRuntimeSignals, AccountSelectionContext, AccountSelectionPolicy,
    AccountSelector, CredentialCasUpdate, CredentialRevision, OpaqueProviderData,
    PlaintextCredential, ProviderAccount, ProviderAccountId, ProviderAccountUpdate,
    RotationStrategy,
};
use gateway_core::routing::ProviderKind;

fn account(id: &str) -> ProviderAccount {
    ProviderAccount::new(
        ProviderAccountId::new(id).expect("valid account"),
        ProviderKind::new("openai").expect("valid provider"),
        id.to_owned(),
        format!("user-{id}"),
        "oauth".to_owned(),
        CredentialRevision::new(1).expect("valid revision"),
        Some(SystemTime::now() + Duration::from_secs(3600)),
    )
    .with_runtime_state(true, AccountAvailability::Ready, None)
}

fn candidate(id: &str, in_flight: u32, remaining: Option<u64>) -> AccountCandidate {
    AccountCandidate {
        account: account(id),
        signals: AccountRuntimeSignals {
            in_flight,
            last_started_at: None,
            quota_reset_at: None,
            quota_remaining_rank: remaining,
            failure_rate_basis_points: None,
            first_output_latency_ms: None,
        },
    }
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
    }
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
fn availability_should_round_trip_all_database_values() {
    assert_eq!(
        AccountAvailability::parse(AccountAvailability::QuotaExhausted.as_str()),
        Some(AccountAvailability::QuotaExhausted)
    );
}

#[test]
fn disabled_account_should_not_be_schedulable() {
    let account =
        account("acct_disabled").with_runtime_state(false, AccountAvailability::Ready, None);

    assert!(!account.is_schedulable(SystemTime::now()));
}

#[test]
fn cooldown_account_should_become_schedulable_at_its_deadline() {
    let now = SystemTime::now();
    let account =
        account("acct_cooldown").with_runtime_state(true, AccountAvailability::Cooldown, Some(now));

    assert!(account.is_schedulable(now));
}

#[test]
fn cooldown_account_should_remain_isolated_before_its_deadline() {
    let now = SystemTime::now();
    let account = account("acct_cooldown").with_runtime_state(
        true,
        AccountAvailability::Cooldown,
        Some(now + Duration::from_secs(1)),
    );

    assert!(!account.is_schedulable(now));
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

    assert_eq!(selected.account.id().as_str(), "acct_idle");
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

    assert_eq!(selected.account.id().as_str(), "acct_healthy");
}

#[test]
fn smart_selector_should_keep_healthy_preferred_account_at_escape_boundary() {
    let fallback = candidate("acct_fallback", 0, Some(100));
    let mut preferred = candidate("acct_preferred", 0, Some(1));
    preferred.signals.failure_rate_basis_points = Some(5_000);
    preferred.signals.first_output_latency_ms = Some(15_000);
    let preferred_id = preferred.account.id().clone();
    let mut selection = context(RotationStrategy::Smart);
    selection.preferred_account = Some(preferred_id);
    let candidates = vec![fallback, preferred];

    let selected = AccountSelector
        .select(&candidates, &selection)
        .expect("candidate available");

    assert_eq!(selected.account.id().as_str(), "acct_preferred");
}

#[test]
fn smart_selector_should_escape_preferred_account_after_failure_threshold() {
    let mut fallback = candidate("acct_fallback", 0, Some(100));
    fallback.signals.failure_rate_basis_points = Some(0);
    fallback.signals.first_output_latency_ms = Some(100);
    let mut preferred = candidate("acct_preferred", 0, Some(100));
    preferred.signals.failure_rate_basis_points = Some(5_001);
    preferred.signals.first_output_latency_ms = Some(100);
    let preferred_id = preferred.account.id().clone();
    let mut selection = context(RotationStrategy::Smart);
    selection.preferred_account = Some(preferred_id);
    let candidates = vec![fallback, preferred];

    let selected = AccountSelector
        .select(&candidates, &selection)
        .expect("fallback candidate available");

    assert_eq!(selected.account.id().as_str(), "acct_fallback");
}

#[test]
fn smart_selector_should_escape_preferred_account_after_latency_threshold() {
    let mut fallback = candidate("acct_fallback", 0, Some(100));
    fallback.signals.failure_rate_basis_points = Some(0);
    fallback.signals.first_output_latency_ms = Some(100);
    let mut preferred = candidate("acct_preferred", 0, Some(100));
    preferred.signals.failure_rate_basis_points = Some(0);
    preferred.signals.first_output_latency_ms = Some(15_001);
    let preferred_id = preferred.account.id().clone();
    let mut selection = context(RotationStrategy::Smart);
    selection.preferred_account = Some(preferred_id);
    let candidates = vec![fallback, preferred];

    let selected = AccountSelector
        .select(&candidates, &selection)
        .expect("fallback candidate available");

    assert_eq!(selected.account.id().as_str(), "acct_fallback");
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

    assert_eq!(selected.account.id().as_str(), "acct_fallback");
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

    assert_eq!(selected.account.id().as_str(), "acct_high_quota");
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

    assert_eq!(selected.account.id().as_str(), "acct_earlier");
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

    assert_eq!(selected.account.id().as_str(), "acct_known");
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

    assert_eq!(selected.account.id().as_str(), "acct_available");
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

    assert_eq!(selected.account.id().as_str(), "acct_ready");
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

    assert_eq!(selected.account.id().as_str(), "acct_fallback");
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
        (first.account.id().as_str(), second.account.id().as_str()),
        ("acct_first", "acct_second")
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

    assert_eq!(selected.account.id().as_str(), "acct_second");
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

    assert_eq!(selected.account.id().as_str(), "acct_second");
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

    assert_eq!(selected.account.id().as_str(), "acct_latest");
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

    assert_eq!(selected.account.id().as_str(), "acct_second");
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
