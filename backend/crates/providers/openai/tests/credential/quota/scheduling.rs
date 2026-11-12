//! 持久化额度的有效期与短暂并发占用共同参与选号

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
            quota_pause: None,
            expected_connection: None,
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
            inherited_concurrency: None,
            signals: AccountRuntimeSignals {
                in_flight: u32::from(account.id().as_str() == "acct_74"),
                last_started_at: None,
                quota_reset_at: None,
                quota_reset_windows: None,
                quota_remaining_rank: None,
                cooldown: None,
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
            ),
            now,
            excluded_accounts: BTreeSet::new(),
            busy_accounts: Default::default(),
            preferred_account: None,
            preferred_account_overrides_weight: true,
            round_robin_cursor: cursor,
            eligibility: AccountEligibilityPolicy::Enforce,
            account_scope: None,
            reserved_concurrency: 0,
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

#[tokio::test]
async fn weekly_window_prefers_the_first_weekly_reset_independently_of_short_resets() {
    let store = Arc::new(MemoryAccountStore::default());
    let now = UNIX_EPOCH + Duration::from_secs(timestamp(SystemTime::now()));
    let mut accounts = Vec::new();
    // 三个账号的短窗口与周窗口顺序相反，确保选择的是配置的窗口而非最早时间
    for (id, hour, day) in [("acct_one", 1, 2), ("acct_two", 2, 3), ("acct_three", 3, 1)] {
        accounts.push(persist_snapshot(&store, id, now, json!({
            "primary_window": {"used_percent":10,"limit_window_seconds":18000,"reset_at":timestamp(now) + hour * 3600u64},
            "secondary_window": {"used_percent":10,"limit_window_seconds":604800,"reset_at":timestamp(now) + day * 86400u64}
        })).await);
    }
    let service = quota_service(&store);
    service.prepare_scheduling(&accounts).await;
    let candidates = accounts
        .into_iter()
        .map(|account| AccountCandidate {
            inherited_concurrency: None,
            signals: AccountRuntimeSignals {
                in_flight: 0,
                last_started_at: None,
                quota_reset_at: None,
                quota_reset_windows: None,
                quota_remaining_rank: None,
                cooldown: None,
                failure_rate_basis_points: None,
                first_output_latency_ms: None,
            }
            .with_provider_quota(service.scheduling_signals(&account)),
            account,
        })
        .collect::<Vec<_>>();
    let mut config = json!({"loadWeight":0,"quotaWeight":0,"healthWeight":0,"latencyWeight":0,"resetWeight":1,"queueWeight":0});
    let policy = |strategy, config: &Value| {
        AccountSelectionPolicy::new(strategy, NonZeroU32::new(3).unwrap())
            .with_smart_scheduling(serde_json::from_value(config.clone()).unwrap())
    };
    let mut context = AccountSelectionContext {
        policy: policy(RotationStrategy::Smart, &config),
        now,
        excluded_accounts: Default::default(),
        busy_accounts: Default::default(),
        preferred_account: None,
        preferred_account_overrides_weight: false,
        round_robin_cursor: 0,
        eligibility: AccountEligibilityPolicy::Enforce,
        account_scope: None,
        reserved_concurrency: 0,
    };
    let selected = |pool: &[AccountCandidate], context: &AccountSelectionContext| {
        AccountSelector
            .select(pool, context)
            .unwrap()
            .candidate()
            .account
            .id()
            .as_str()
            .to_owned()
    };
    config["resetWindowWeights"] = json!({"fiveHour":1,"weekly":0,"monthly":0});
    context.policy = policy(RotationStrategy::Smart, &config);
    assert_eq!(selected(&candidates, &context), "acct_one");
    config["resetWindowWeights"] = json!({"fiveHour":0,"weekly":1,"monthly":0});
    context.policy = policy(RotationStrategy::Smart, &config);
    assert_eq!(selected(&candidates, &context), "acct_three");
    let trace = gateway_core::diagnostics::TraceContext::new("req_weekly");
    trace.account_selection(&candidates, &context, None);
    let snapshot = trace.snapshot().unwrap();
    let data = &snapshot["events"][0]["data"];
    for (candidate, expected) in
        data["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .zip([1.0 / 3.0, 0.25, 0.5])
    {
        assert!((candidate["smartScore"].as_f64().unwrap() - expected).abs() < 1e-10);
    }
    assert!(1.0 / 3.0 - 0.25 > data["smartScoreTolerance"].as_f64().unwrap());
    context.round_robin_cursor = 1;
    assert_eq!(selected(&candidates[..2], &context), "acct_one");
    context.policy = policy(RotationStrategy::QuotaResetPriority, &config);
    assert_eq!(selected(&candidates, &context), "acct_one");
}
