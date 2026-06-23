use std::{sync::Arc, time::Duration as StdDuration};

use chrono::{Duration, Utc};
use codex_proxy_rs::infra::crypto::SecretBox;
use codex_proxy_rs::infra::database::connect_sqlite;
use codex_proxy_rs::upstream::accounts::model::AccountStatus;
use codex_proxy_rs::upstream::accounts::quota::RuntimeQuotaRefreshService;
use codex_proxy_rs::upstream::accounts::store::{NewAccount, SqliteAccountStore};
use codex_proxy_rs::upstream::fingerprint::Fingerprint;
use codex_proxy_rs::upstream::transport::CodexBackendClient;
use secrecy::SecretString;
use serde_json::{json, Value};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[test]
fn quota_refresh_service_should_expose_default_request_spacing() {
    assert_eq!(
        RuntimeQuotaRefreshService::default_request_spacing(),
        StdDuration::from_secs(3)
    );
}

#[tokio::test]
async fn quota_refresh_service_should_fetch_usage_for_quota_locked_accounts_and_store_quota() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 72.4,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 18_000
                },
                "secondary_window": {
                    "used_percent": 88.2,
                    "reset_at": 1_800_000_100,
                    "limit_window_seconds": 3_600
                }
            }
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("quota-refresh.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool.clone(), SecretBox::new([12u8; 32]));
    store
        .insert(NewAccount {
            id: "acct-quota".to_string(),
            email: Some("user@example.com".to_string()),
            account_id: Some("chatgpt-account".to_string()),
            user_id: None,
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new("access-token".to_string().into()),
            refresh_token: None,
            added_at: None,
            access_token_expires_at: Some(Utc::now() + Duration::hours(1)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    sqlx::query("update accounts set quota_limit_reached = 1 where id = ?")
        .bind("acct-quota")
        .execute(&pool)
        .await
        .expect("quota lock should be set");
    let codex = CodexBackendClient::new(
        reqwest::Client::new(),
        server.uri(),
        Fingerprint::default_for_tests(),
    );
    let service = RuntimeQuotaRefreshService::new(store.clone(), Arc::new(codex));

    let summary = service
        .refresh_locked_accounts_once()
        .await
        .expect("quota refresh should succeed");
    let stored = store
        .get_quota_json("acct-quota")
        .await
        .expect("quota json should load")
        .expect("quota json should be present");
    let quota: Value = serde_json::from_str(&stored).expect("quota json should parse");

    assert_eq!(
        (
            summary.refreshed,
            quota
                .pointer("/rate_limit/limit_reached")
                .and_then(Value::as_bool),
            quota
                .pointer("/rate_limit/remaining_percent")
                .and_then(Value::as_i64),
            quota
                .pointer("/secondary_rate_limit/limit_reached")
                .and_then(Value::as_bool),
        ),
        (1, Some(false), Some(28), Some(false))
    );
}

#[tokio::test]
async fn quota_refresh_service_should_stagger_multiple_locked_account_requests() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 42.0,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 18_000
                }
            }
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("quota-refresh-spacing.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let store = SqliteAccountStore::new(pool.clone(), SecretBox::new([13u8; 32]));
    insert_quota_locked_account(&store, &pool, "acct-quota-1", "access-token-1").await;
    insert_quota_locked_account(&store, &pool, "acct-quota-2", "access-token-2").await;
    let codex = CodexBackendClient::new(
        reqwest::Client::new(),
        server.uri(),
        Fingerprint::default_for_tests(),
    );
    let service = RuntimeQuotaRefreshService::new(store, Arc::new(codex))
        .with_request_spacing(StdDuration::from_millis(200));

    let refresh = tokio::spawn(async move { service.refresh_locked_accounts_once().await });
    wait_for_usage_requests(&server, 1).await;
    tokio::time::sleep(StdDuration::from_millis(50)).await;

    assert_eq!(usage_request_count(&server).await, 1);

    let summary = refresh
        .await
        .expect("quota refresh service should join")
        .expect("quota refresh should succeed");

    assert_eq!(
        (summary.refreshed, usage_request_count(&server).await),
        (2, 2)
    );
}

async fn insert_quota_locked_account(
    store: &SqliteAccountStore,
    pool: &sqlx::SqlitePool,
    id: &str,
    access_token: &str,
) {
    store
        .insert(NewAccount {
            id: id.to_string(),
            email: Some(format!("{id}@example.com")),
            account_id: Some(format!("chatgpt-{id}")),
            user_id: None,
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new(access_token.to_string().into()),
            refresh_token: None,
            added_at: None,
            access_token_expires_at: Some(Utc::now() + Duration::hours(1)),
            status: AccountStatus::Active,
        })
        .await
        .expect("account should be inserted");
    sqlx::query("update accounts set quota_limit_reached = 1 where id = ?")
        .bind(id)
        .execute(pool)
        .await
        .expect("quota lock should be set");
}

async fn wait_for_usage_requests(server: &MockServer, expected_count: usize) {
    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(2);
    loop {
        if usage_request_count(server).await >= expected_count {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "quota refresh task did not request usage before timeout"
        );
        tokio::time::sleep(StdDuration::from_millis(25)).await;
    }
}

async fn usage_request_count(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .expect("received requests should load")
        .iter()
        .filter(|request| request.url.path() == "/api/codex/usage")
        .count()
}
