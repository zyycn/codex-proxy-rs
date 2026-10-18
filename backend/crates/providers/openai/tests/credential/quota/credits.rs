//! 点数独立可用性、耗尽恢复和过期查询隔离

use super::*;

fn exhausted_usage() -> serde_json::Value {
    json!({"rate_limit": {
        "allowed": false,
        "limit_reached": true,
        "primary_window": {"used_percent": 100, "reset_at": 1_900_000_000},
        "secondary_window": {"used_percent": 100, "reset_at": 1_900_604_800}
    }})
}

async fn mount_usage(server: &MockServer, usage: serde_json::Value) {
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(usage))
        .mount(server)
        .await;
}

#[tokio::test]
async fn credits_allow_exhausted_subscription_without_bypassing_spend_restrictions() {
    for (metadata, allowed) in [
        (
            json!({"credits": {"has_credits": true, "unlimited": false}}),
            true,
        ),
        (
            json!({"credits": {"has_credits": false, "unlimited": true}}),
            true,
        ),
        (json!({"credits": {"has_credits": true}}), true),
        (json!({"credits": {"unlimited": true}}), true),
        (
            json!({"credits": {"has_credits": false, "unlimited": false, "balance": "500"}}),
            false,
        ),
        (json!({"credits": null}), false),
        (
            json!({"credits": {"has_credits": true}, "spend_control": {"reached": true}}),
            false,
        ),
        (
            json!({"credits": {"has_credits": true}, "rate_limit_reached_type": {"type": "spend_limit_reached"}}),
            false,
        ),
        (
            json!({"credits": {"has_credits": true}, "rate_limit_reached_type": {"type": "rate_limit_reached"}}),
            true,
        ),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account_with_enabled(&store, "acct_credits", false).await;
        let account = store.account("acct_credits").expect("account");
        persist_credential_state(&store, &account, CredentialState::Expired).await;
        let server = MockServer::start().await;
        let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
        let mut usage = exhausted_usage();
        usage
            .as_object_mut()
            .expect("usage")
            .extend(metadata.as_object().expect("metadata").clone());
        mount_usage(&server, usage).await;

        let snapshot = service
            .refresh_account(account.id())
            .await
            .expect("refresh credits");
        assert_eq!(
            snapshot.quota().access() == QuotaAccessState::Allowed,
            allowed,
            "{metadata}"
        );
        assert_eq!(snapshot.fact().remaining_percent(), Some(0));
        let current = store.account("acct_credits").expect("current account");
        assert_eq!(current.quota().is_exhausted(), !allowed, "{metadata}");
        assert!(!current.enabled());
        assert_eq!(current.credential_state(), CredentialState::Expired);
    }
}

#[tokio::test]
async fn credits_refresh_clears_old_recovery_and_locks_again_only_when_spent() {
    for periodic in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_credits_recovery").await;
        let account = store.account("acct_credits_recovery").expect("account");
        let server = MockServer::start().await;
        let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
        mount_usage(&server, exhausted_usage()).await;
        service
            .refresh_account(account.id())
            .await
            .expect("seed exhaustion");
        assert!(
            store
                .quota_json("acct_credits_recovery")
                .expect("quota")
                .get("_quota_recovery")
                .is_some()
        );

        let mut usage = exhausted_usage();
        usage["credits"] =
            json!({"has_credits": !periodic, "unlimited": periodic, "balance": "20"});
        mount_usage(&server, usage.clone()).await;
        if periodic {
            assert_eq!(
                service
                    .synchronize()
                    .await
                    .expect("periodic recovery")
                    .updated,
                1
            );
        } else {
            service
                .refresh_account(account.id())
                .await
                .expect("manual recovery");
        }
        let snapshot = service
            .read_account(account.id())
            .await
            .expect("persisted snapshot")
            .expect("snapshot");
        assert_eq!(snapshot.quota().access(), QuotaAccessState::Allowed);
        assert_eq!(snapshot.fact().remaining_percent(), Some(0));
        assert_eq!(
            snapshot.credits().expect("credits").balance.as_deref(),
            Some("20")
        );
        assert!(
            store
                .quota_json("acct_credits_recovery")
                .expect("quota")
                .get("_quota_recovery")
                .is_none()
        );

        usage["credits"] = json!({"has_credits": false, "unlimited": false, "balance": "0"});
        mount_usage(&server, usage).await;
        let snapshot = service
            .refresh_account(account.id())
            .await
            .expect("spent credits");
        assert!(snapshot.quota().is_exhausted());
        assert!(
            store
                .account("acct_credits_recovery")
                .expect("account")
                .quota()
                .is_exhausted()
        );
    }
}

#[tokio::test]
async fn credits_query_started_before_new_exhaustion_cannot_restore_access() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_stale_credits").await;
    let account = store.account("acct_stale_credits").expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    let mut usage = exhausted_usage();
    usage["credits"] = json!({"has_credits": true, "unlimited": false});
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(200))
                .set_body_json(usage),
        )
        .mount(&server)
        .await;
    let (result, ()) = tokio::join!(service.refresh_account(account.id()), async {
        tokio::time::timeout(Duration::from_secs(2), async {
            while server
                .received_requests()
                .await
                .expect("requests")
                .is_empty()
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("query started");
        persist_quota_state(
            &store,
            &account,
            QuotaState::exhausted(QuotaEvidence::UsageLimitReached, SystemTime::now(), None),
        )
        .await;
    });
    result.expect("old display observation can persist independently of newer access state");
    assert!(
        store
            .account("acct_stale_credits")
            .expect("account")
            .quota()
            .is_exhausted()
    );
    assert!(
        service
            .read_account(account.id())
            .await
            .expect("persisted quota")
            .expect("snapshot")
            .quota()
            .is_exhausted()
    );
}
