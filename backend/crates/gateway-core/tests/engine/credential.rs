use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value};

use gateway_core::engine::credential::{
    AccountAvailability, AccountCandidate, AccountRuntimeSignals, AccountSelectionContext,
    AccountSelectionPolicy, AccountSelector, CredentialCasUpdate, CredentialRevision,
    OpaqueProviderData, PlaintextCredential, ProviderAccount, ProviderAccountId,
    ProviderAccountUpdate, RotationStrategy,
};
use gateway_core::routing::{ProviderInstanceId, ProviderKind};

fn account(id: &str) -> ProviderAccount {
    ProviderAccount::new(
        ProviderAccountId::new(id).expect("valid account"),
        ProviderInstanceId::new("inst_test").expect("valid instance"),
        ProviderKind::new("openai").expect("valid provider"),
        id.to_owned(),
        format!("user-{id}"),
        CredentialRevision::new(1).expect("valid revision"),
        SystemTime::now() + Duration::from_secs(3600),
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
        sticky_account: None,
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
    context.sticky_account =
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
            SystemTime::now() + Duration::from_secs(60),
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
            SystemTime::now() + Duration::from_secs(60),
            Some(SystemTime::now() + Duration::from_secs(30)),
        )
        .is_err()
    );
}
