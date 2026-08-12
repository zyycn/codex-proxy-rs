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
                "primary_window": {"used_percent": 100, "reset_at": 1_900_000_000}
            }
        })))
        .mount(&server)
        .await;

    let snapshot = service
        .refresh_account(account.id())
        .await
        .expect("refresh allowed quota");
    let current = store.account(account_id).expect("recovered quota account");

    assert_eq!(snapshot.fact().remaining_percent(), Some(0));
    assert_eq!(snapshot.quota().access(), QuotaAccessState::Allowed);
    assert_eq!(current.quota().access(), QuotaAccessState::Allowed);
    assert_eq!(current.credential_state(), CredentialState::Expired);
}

#[tokio::test]
async fn explicit_allowance_replaces_confirmed_exhaustion_without_usage_heuristics() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_authoritative_quota_recovery";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    persist_quota_state(
        &store,
        &account,
        exhausted_quota(Some(SystemTime::now() + Duration::from_secs(3_600))),
    )
    .await;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
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
        .expect("refresh authoritative allowance");

    assert_eq!(snapshot.fact().remaining_percent(), Some(0));
    assert_eq!(snapshot.quota().access(), QuotaAccessState::Allowed);
    assert_eq!(
        store
            .account(account_id)
            .expect("updated account")
            .quota()
            .access(),
        QuotaAccessState::Allowed
    );
}

#[tokio::test]
async fn inconclusive_quota_refresh_preserves_confirmed_exhaustion() {
    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_inconclusive_quota_refresh";
    create_account(&store, account_id).await;
    let account = store.account(account_id).expect("created account");
    persist_quota_state(&store, &account, exhausted_quota(None)).await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "primary_window": {"used_percent": 12, "reset_at": 1_900_000_000}
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
        .expect("refresh inconclusive quota");

    assert_eq!(snapshot.quota().access(), QuotaAccessState::Exhausted);
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
