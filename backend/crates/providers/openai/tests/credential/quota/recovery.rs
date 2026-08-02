//! 402 恢复回归。
//!
//! 覆盖 `QuotaExhausted -> Ready` 的证据规则：旧 reset 已到期 + 新快照未耗尽。

use super::*;

#[tokio::test]
async fn manual_quota_refresh_keeps_confirmed_exhaustion_until_the_recorded_reset() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_manual_quota_state";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    store
        .apply_state_change(AccountStateChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::Expired,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("seed stale expired state");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header(
            "authorization",
            format!("Bearer token-{account_id}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": false,
                "limit_reached": true,
                "primary_window": {"used_percent": 100, "reset_at": 1_900_000_000}
            }
        })))
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    let exhausted = service
        .refresh_account(account.id())
        .await
        .expect("refresh exhausted quota");

    assert!(exhausted.fact().exhausted());
    assert_eq!(
        store
            .account(account_id)
            .expect("exhausted account")
            .availability(),
        AccountAvailability::Expired,
        "manual refresh never changes non-QuotaExhausted availability"
    );

    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header(
            "authorization",
            format!("Bearer token-{account_id}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {"used_percent": 100, "reset_at": 1_900_000_000}
            }
        })))
        .mount(&server)
        .await;

    let stale = service
        .refresh_account(account.id())
        .await
        .expect("refresh contradictory full quota");

    // used=100 是快照级 exhaustion。
    assert!(stale.fact().exhausted());
    assert_eq!(
        store
            .account(account_id)
            .expect("account after contradictory quota")
            .availability(),
        AccountAvailability::Expired,
        "an unchanged usage window cannot override an expired credential state"
    );

    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header(
            "authorization",
            format!("Bearer token-{account_id}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {"used_percent": 99, "reset_at": 1_900_000_000}
            }
        })))
        .mount(&server)
        .await;

    let unchanged_reset = service
        .refresh_account(account.id())
        .await
        .expect("refresh lower usage quota");

    assert!(!unchanged_reset.fact().exhausted());
    assert_eq!(
        store
            .account(account_id)
            .expect("account after lower usage")
            .availability(),
        AccountAvailability::Expired,
        "a lower used_percent before the recorded reset is not quota recovery for an expired credential"
    );
}

#[tokio::test]
async fn expired_old_reset_with_fresh_allowed_snapshot_recovers_ready() {
    // 旧快照 reset_at < now，新快照 allowed=true、
    // limit_reached=false、reset_at > now；最终必须恢复 Ready。
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_cross_window_recovery";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    let old_reset = Utc::now().timestamp() - 60;
    store
        .compare_and_swap_quota(QuotaObservation {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: Some(OpaqueProviderData::new(
                json!({
                    "rate_limit": {
                        "allowed": false,
                        "limit_reached": true,
                        "primary_window": {
                            "used_percent": 100,
                            "reset_at": old_reset,
                            "limit_window_seconds": 3600
                        }
                    }
                })
                .as_object()
                .expect("quota object")
                .clone(),
            )),
            observed_at: Some(SystemTime::now()),
            limit_reached: None,
        })
        .await
        .expect("seed old exhausted snapshot");
    store
        .apply_state_change(AccountStateChange {
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            availability: AccountAvailability::QuotaExhausted,
            observed_at: SystemTime::now(),
        })
        .await
        .expect("mark account exhausted");
    let server = MockServer::start().await;
    let future_reset = Utc::now().timestamp() + 3600;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header(
            "authorization",
            format!("Bearer token-{account_id}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {"used_percent": 5, "reset_at": future_reset}
            }
        })))
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client"),
        server.uri(),
    );

    let recovered = service
        .refresh_account(account.id())
        .await
        .expect("refresh new window quota");

    assert!(!recovered.fact().exhausted());
    assert_eq!(
        store
            .account(account_id)
            .expect("account after cross-window recovery")
            .availability(),
        AccountAvailability::Ready,
        "old reset elapsed + fresh allowed snapshot must recover QuotaExhausted"
    );
}
