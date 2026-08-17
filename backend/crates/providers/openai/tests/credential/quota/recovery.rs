//! 额度刷新与凭证事实相互独立的回归测试。

use super::*;

#[tokio::test]
async fn quota_refresh_updates_access_fact_without_recovering_credential_error() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_independent_quota_and_credential";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    persist_credential_state(&store, &account, CredentialState::Expired).await;

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

    let snapshot = service
        .refresh_account(account.id())
        .await
        .expect("refresh exhausted quota");
    let current = store.account(account_id).expect("updated account");

    assert_eq!(snapshot.quota().access(), QuotaAccessState::Exhausted);
    assert_eq!(current.quota().access(), QuotaAccessState::Exhausted);
    assert_eq!(current.credential_state(), CredentialState::Expired);

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
                "primary_window": {"used_percent": 9, "reset_at": 1_900_003_600}
            }
        })))
        .mount(&server)
        .await;

    let snapshot = service
        .refresh_account(account.id())
        .await
        .expect("refresh allowed quota");
    let current = store.account(account_id).expect("recovered quota account");

    assert_eq!(snapshot.fact().remaining_percent(), Some(91));
    assert_eq!(snapshot.quota().access(), QuotaAccessState::Allowed);
    assert_eq!(current.quota().access(), QuotaAccessState::Allowed);
    assert_eq!(current.credential_state(), CredentialState::Expired);
}

#[tokio::test]
async fn exhausted_quota_worker_only_touches_refresh_time_when_recovery_is_rejected() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_worker_quota_recovery_gate";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": false,
                "limit_reached": true,
                "primary_window": {"used_percent": 100, "reset_at": 1_700_000_000}
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

    let old_snapshot = service
        .refresh_account(account.id())
        .await
        .expect("seed exhausted quota");
    let old_quota = store.quota_json(account_id).expect("old quota document");

    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {"used_percent": 98, "reset_at": 1_900_000_000}
            }
        })))
        .mount(&server)
        .await;

    let summary = service.synchronize().await.expect("worker quota refresh");

    assert_eq!(summary.exhausted, 1);
    assert_eq!(summary.updated, 0);
    assert_eq!(store.quota_json(account_id), Some(old_quota));
    let refreshed = service
        .read_account(account.id())
        .await
        .expect("read refreshed quota")
        .expect("refreshed quota snapshot");
    assert!(refreshed.observed_at() > old_snapshot.observed_at());
    assert_eq!(refreshed.fact(), old_snapshot.fact());
    assert_eq!(refreshed.quota().access(), QuotaAccessState::Exhausted);
    assert_eq!(
        store
            .account(account_id)
            .expect("preserved account")
            .quota()
            .access(),
        QuotaAccessState::Exhausted
    );
}

#[tokio::test]
async fn manual_quota_refresh_only_recovers_after_reset_advances_below_ten_percent() {
    for (suffix, allowed, limit_reached, reset_at, used_percent, recovered) in [
        (
            "still_exhausted",
            false,
            true,
            1_900_003_600_i64,
            0_u8,
            false,
        ),
        ("same_reset", true, false, 1_900_000_000_i64, 0_u8, false),
        ("exactly_ten", true, false, 1_900_003_600_i64, 10_u8, false),
        ("below_ten", true, false, 1_900_003_600_i64, 9_u8, true),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        let account_id = format!("acct_manual_quota_recovery_{suffix}");
        create_account(&store, &account_id).await;
        let account = store.account(&account_id).expect("created account");
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
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
        let old_snapshot = service
            .refresh_account(account.id())
            .await
            .expect("seed exhausted quota");
        let old_quota = store.quota_json(&account_id).expect("old quota document");

        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "rate_limit": {
                    "allowed": allowed,
                    "limit_reached": limit_reached,
                    "primary_window": {"used_percent": used_percent, "reset_at": reset_at}
                }
            })))
            .mount(&server)
            .await;

        let snapshot = service
            .refresh_account(account.id())
            .await
            .expect("refresh exhausted quota");
        let current = store.account(&account_id).expect("refreshed account");

        if recovered {
            assert_eq!(snapshot.fact().remaining_percent(), Some(91));
            assert_eq!(snapshot.quota().access(), QuotaAccessState::Allowed);
            assert_ne!(store.quota_json(&account_id), Some(old_quota));
            assert_eq!(current.quota().access(), QuotaAccessState::Allowed);
        } else {
            assert_eq!(snapshot.fact(), old_snapshot.fact());
            assert_eq!(snapshot.quota().access(), QuotaAccessState::Exhausted);
            assert_eq!(store.quota_json(&account_id), Some(old_quota));
            assert_eq!(current.quota().access(), QuotaAccessState::Exhausted);
        }
        assert!(snapshot.observed_at() > old_snapshot.observed_at());
    }
}

#[tokio::test]
async fn deactivated_workspace_quota_response_persists_credential_error_reason() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_deactivated_workspace";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "detail": {
                "code": "deactivated_workspace",
                "message": "workspace disabled"
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

    assert!(service.refresh_account(account.id()).await.is_err());
    let current = store
        .account(account_id)
        .expect("account after deactivation");
    assert_eq!(current.credential_state(), CredentialState::Banned);
    assert_eq!(
        current.last_error_reason(),
        Some(gateway_core::engine::credential::AccountErrorReason::AccountBanned)
    );
}
