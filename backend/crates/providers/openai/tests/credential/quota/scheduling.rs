//! 持久化额度的有效期与短暂并发占用共同参与选号。

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gateway_core::account::{
    AccountCandidate, AccountConcurrencyLimit, AccountEligibilityPolicy, AccountRuntimeSignals,
    AccountSelectionContext, AccountSelectionPolicy, AccountSelector, AccountWeight,
    OpaqueProviderData, ProviderAccount, ProviderAccountStore as _, QuotaAccessState,
    QuotaEvidence, QuotaObservation, QuotaState, RotationStrategy,
};
use serde_json::{Value, json};

use super::{MemoryAccountStore, create_account, quota_service};

async fn persist_snapshot(
    store: &Arc<MemoryAccountStore>,
    id: &str,
    observed_at: SystemTime,
    rate_limit: Value,
) -> ProviderAccount {
    create_account(store, id).await;
    let account = store.account(id).expect("created account");
    store
        .compare_and_swap_quota(QuotaObservation {
            plan_type: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: OpaqueProviderData::new(
                json!({"rate_limit": rate_limit})
                    .as_object()
                    .expect("quota object")
                    .clone(),
            ),
            observed_at,
            state: QuotaState::allowed(observed_at),
        })
        .await
        .expect("persist quota");
    store.account(id).expect("persisted account")
}

fn timestamp(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).expect("epoch").as_secs()
}

#[tokio::test]
async fn old_weekly_quota_should_outweigh_one_in_flight_title_request() {
    let store = Arc::new(MemoryAccountStore::default());
    let now = SystemTime::now();
    let reset_at = timestamp(now + Duration::from_secs(6 * 3600));
    let mut accounts = Vec::new();
    for (id, used_percent, age_minutes) in [("acct_74", 74, 41), ("acct_96", 96, 16)] {
        accounts.push(
            persist_snapshot(
                &store,
                id,
                now - Duration::from_secs(age_minutes * 60),
                json!({
                    "allowed": true,
                    "primary_window": {
                        "used_percent": used_percent,
                        "reset_at": reset_at,
                        "limit_window_seconds": 604_800
                    }
                }),
            )
            .await
            .with_scheduling(
                Some(AccountConcurrencyLimit::new(10).expect("concurrency")),
                AccountWeight::DEFAULT,
            ),
        );
    }
    let service = quota_service(&store);
    service.prepare_scheduling(&accounts).await;
    let candidates = accounts
        .into_iter()
        .map(|account| AccountCandidate {
            signals: AccountRuntimeSignals {
                in_flight: u32::from(account.id().as_str() == "acct_74"),
                last_started_at: None,
                quota_reset_at: None,
                quota_remaining_rank: None,
                rate_limited_until: None,
                failure_rate_basis_points: None,
                first_output_latency_ms: None,
            }
            .with_provider_quota(service.scheduling_signals(&account)),
            account,
        })
        .collect::<Vec<_>>();
    for cursor in 0..20 {
        let context = AccountSelectionContext {
            policy: AccountSelectionPolicy::new(
                RotationStrategy::Smart,
                NonZeroU32::new(3).expect("default concurrency"),
                Duration::ZERO,
            ),
            now,
            excluded_accounts: BTreeSet::new(),
            preferred_account: None,
            preferred_account_overrides_weight: true,
            round_robin_cursor: cursor,
            eligibility: AccountEligibilityPolicy::Enforce,
            account_scope: None,
        };
        let selected = AccountSelector
            .select(&candidates, &context)
            .expect("available account");
        assert_eq!(selected.candidate().account.id().as_str(), "acct_74");
    }
}

#[tokio::test]
async fn old_quota_should_drop_reset_short_window_and_keep_weekly_window() {
    let store = Arc::new(MemoryAccountStore::default());
    let now = SystemTime::now();
    let weekly_reset = now + Duration::from_secs(6 * 3600);
    let account = persist_snapshot(
        &store,
        "acct_mixed_resets",
        now - Duration::from_secs(41 * 60),
        json!({
            "primary_window": {
                "used_percent": 99,
                "reset_at": timestamp(now - Duration::from_secs(60)),
                "limit_window_seconds": 18_000
            },
            "secondary_window": {
                "used_percent": 74,
                "reset_at": timestamp(weekly_reset),
                "limit_window_seconds": 604_800
            }
        }),
    )
    .await;
    let service = quota_service(&store);
    service
        .read_account(account.id())
        .await
        .expect("read quota");
    let signals = service.scheduling_signals(&account).expect("weekly quota");
    assert_eq!(
        (signals.remaining_rank(), signals.reset_at().map(timestamp)),
        (Some(2600), Some(timestamp(weekly_reset)))
    );
}

#[tokio::test]
async fn undated_quota_should_expire_independently_of_dated_weekly_quota() {
    for (age_minutes, expected_remaining) in [(1, 100), (41, 2600)] {
        let store = Arc::new(MemoryAccountStore::default());
        let now = SystemTime::now();
        let account = persist_snapshot(
            &store,
            "acct_undated_window",
            now - Duration::from_secs(age_minutes * 60),
            json!({
                "primary_window": {"used_percent": 99},
                "secondary_window": {
                    "used_percent": 74,
                    "reset_at": timestamp(now + Duration::from_secs(6 * 3600)),
                    "limit_window_seconds": 604_800
                }
            }),
        )
        .await;
        let service = quota_service(&store);
        service
            .prepare_scheduling(std::slice::from_ref(&account))
            .await;
        assert_eq!(
            service
                .scheduling_signals(&account)
                .and_then(|s| s.remaining_rank()),
            Some(expected_remaining),
            "observation age: {age_minutes} minutes"
        );
    }
}

#[tokio::test]
async fn old_undated_quota_should_remain_unknown_after_cache_hydration() {
    let store = Arc::new(MemoryAccountStore::default());
    let account = persist_snapshot(
        &store,
        "acct_undated_only",
        SystemTime::now() - Duration::from_secs(41 * 60),
        json!({"primary_window": {"used_percent": 74}}),
    )
    .await;
    let service = quota_service(&store);
    for _ in 0..2 {
        service
            .prepare_scheduling(std::slice::from_ref(&account))
            .await;
        assert_eq!(service.scheduling_signals(&account), None);
    }
    assert_eq!(store.quota_reads(), 1);
}

#[tokio::test]
async fn fresh_reset_observation_should_clear_previous_scheduling_signals() {
    let store = Arc::new(MemoryAccountStore::default());
    let now = SystemTime::now();
    let account = persist_snapshot(
        &store,
        "acct_fresh_reset",
        now,
        json!({"primary_window": {
            "used_percent": 74,
            "reset_at": timestamp(now + Duration::from_secs(3600))
        }}),
    )
    .await;
    let service = quota_service(&store);
    service
        .prepare_scheduling(std::slice::from_ref(&account))
        .await;
    assert_eq!(
        service
            .scheduling_signals(&account)
            .and_then(|s| s.remaining_rank()),
        Some(2600)
    );

    let headers = vec![
        ("x-codex-active-limit".to_owned(), "codex".to_owned()),
        ("x-codex-primary-used-percent".to_owned(), "74".to_owned()),
        (
            "x-codex-primary-reset-at".to_owned(),
            timestamp(now - Duration::from_secs(60)).to_string(),
        ),
    ];
    assert!(
        service
            .synchronize_passive_headers(&account, &headers)
            .await
            .expect("reset observation")
    );
    assert_eq!(service.scheduling_signals(&account), None);
}

#[tokio::test]
async fn reset_window_should_not_clear_confirmed_exhaustion() {
    let store = Arc::new(MemoryAccountStore::default());
    let now = SystemTime::now();
    let reset_at = now - Duration::from_secs(60);
    let account = persist_snapshot(
        &store,
        "acct_exhausted_reset",
        now - Duration::from_secs(41 * 60),
        json!({"primary_window": {"used_percent": 100, "reset_at": timestamp(reset_at)}}),
    )
    .await;
    super::persist_quota_state(
        &store,
        &account,
        QuotaState::exhausted(QuotaEvidence::ProviderDenied, now, Some(reset_at)),
    )
    .await;
    let service = quota_service(&store);
    service
        .prepare_scheduling(std::slice::from_ref(&account))
        .await;
    assert_eq!(service.scheduling_signals(&account), None);
    assert_eq!(
        store
            .account("acct_exhausted_reset")
            .expect("account")
            .quota()
            .access(),
        QuotaAccessState::Exhausted
    );
}
