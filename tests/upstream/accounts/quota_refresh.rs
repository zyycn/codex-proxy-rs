use std::{sync::Arc, time::Duration as StdDuration};

use chrono::{Duration, Utc};
use codex_proxy_rs::infra::crypto::SecretBox;
use codex_proxy_rs::infra::database::connect_sqlite;
use codex_proxy_rs::upstream::accounts::cookies::SqliteCookieStore;
use codex_proxy_rs::upstream::accounts::model::AccountStatus;
use codex_proxy_rs::upstream::accounts::quota::{
    quota_from_usage, quota_snapshot_limit_reached, RuntimeQuotaRefreshService,
};
use codex_proxy_rs::upstream::accounts::store::{NewAccount, SqliteAccountStore};
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

#[test]
fn quota_from_usage_should_preserve_spend_control_individual_limit() {
    let usage = json!({
        "plan_type": "plus",
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {
                "used_percent": 18,
                "reset_at": 1_800_000_000,
                "limit_window_seconds": 18_000
            }
        },
        "spend_control": {
            "reached": false,
            "individual_limit": {
                "used_percent": 52,
                "remaining_percent": 48,
                "reset_at": 1_802_592_000
            }
        }
    });

    let quota = quota_from_usage(&usage);

    assert_eq!(
        quota
            .pointer("/monthly_limit/source")
            .and_then(Value::as_str),
        Some("spend_control")
    );
    assert_eq!(
        quota
            .pointer("/monthly_limit/used_percent")
            .and_then(Value::as_f64),
        Some(52.0)
    );
}

#[test]
fn quota_from_usage_should_preserve_additional_limit_snapshot_by_limit_name() {
    let usage = json!({
        "plan_type": "plus",
        "additional_rate_limits": [{
            "limit_name": "gpt-5.3-codex-spark",
            "rate_limit": {
                "allowed": false,
                "limit_reached": true,
                "primary_window": {
                    "used_percent": 100,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 18_000
                },
                "secondary_window": {
                    "used_percent": 45,
                    "reset_at": 1_800_604_800,
                    "limit_window_seconds": 604_800
                }
            }
        }]
    });

    let quota = quota_from_usage(&usage);

    assert_eq!(
        quota.pointer("/snapshots/0/source"),
        Some(&Value::String("additional".to_string()))
    );
    assert_eq!(
        quota.pointer("/snapshots/0/limit_name"),
        Some(&Value::String("gpt-5.3-codex-spark".to_string()))
    );
    assert_eq!(
        quota.pointer("/snapshots/0/blocked"),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        quota
            .pointer("/snapshots/0/secondary/window_minutes")
            .and_then(Value::as_u64),
        Some(10_080)
    );
}

#[test]
fn quota_from_usage_should_preserve_additional_limit_name_and_metered_feature() {
    let usage = json!({
        "plan_type": "plus",
        "additional_rate_limits": [{
            "metered_feature": "review",
            "limit_name": "Code review",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 35,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 604_800
                }
            }
        }]
    });

    let quota = quota_from_usage(&usage);

    assert_eq!(
        quota.pointer("/snapshots/0/limit_name"),
        Some(&Value::String("Code review".to_string()))
    );
    assert_eq!(
        quota.pointer("/snapshots/0/metered_feature"),
        Some(&Value::String("review".to_string()))
    );
}

#[test]
fn quota_snapshot_limit_reached_should_keep_allowed_free_account_without_reset_credits_available() {
    let usage = json!({
        "plan_type": "free",
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {
                "used_percent": 6,
                "reset_at": 1_806_364_800,
                "limit_window_seconds": 2_592_000
            }
        },
        "credits": {
            "has_credits": false,
            "unlimited": false,
            "overage_limit_reached": false,
            "balance": 0
        }
    });
    let quota = quota_from_usage(&usage);

    assert!(!quota_snapshot_limit_reached(&quota));
}

#[test]
fn quota_snapshot_limit_reached_should_block_explicit_credit_overage_limit() {
    let quota = json!({
        "credits": {
            "has_credits": false,
            "unlimited": false,
            "overage_limit_reached": true,
            "balance": 0
        }
    });

    assert!(quota_snapshot_limit_reached(&quota));
}

#[tokio::test]
async fn quota_refresh_service_should_send_usage_cookie_when_cookie_store_is_configured() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 12.0,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 18_000
                }
            }
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("quota-refresh-cookie.sqlite");
    let pool = connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let secret_box = SecretBox::new([14u8; 32]);
    let store = SqliteAccountStore::new(pool.clone(), secret_box.clone());
    let cookies = SqliteCookieStore::new(pool.clone(), secret_box);
    insert_quota_locked_account(&store, &pool, "acct-quota-cookie", "access-token-cookie").await;
    cookies
        .set_cookie_header("acct-quota-cookie", "cf_clearance=quota-refresh")
        .await
        .expect("cookie should be stored");
    let codex = CodexBackendClient::new(
        reqwest::Client::new(),
        server.uri(),
        crate::support::fingerprint::test_fingerprint(),
    );
    let service =
        RuntimeQuotaRefreshService::new(store, Arc::new(codex)).with_cookie_store(cookies);

    let summary = service
        .refresh_locked_accounts_once()
        .await
        .expect("quota refresh should succeed");
    let requests = server
        .received_requests()
        .await
        .expect("received requests should load");
    let cookie_header = requests
        .iter()
        .find(|request| request.url.path() == "/api/codex/usage")
        .and_then(|request| request.headers.get("cookie"))
        .and_then(|value| value.to_str().ok());

    assert_eq!(
        (summary.refreshed, cookie_header),
        (1, Some("cf_clearance=quota-refresh"))
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
        crate::support::fingerprint::test_fingerprint(),
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
                .pointer("/snapshots/0/blocked")
                .and_then(Value::as_bool),
            quota
                .pointer("/snapshots/0/primary/remaining_percent")
                .and_then(Value::as_i64),
            quota
                .pointer("/snapshots/0/secondary/limit_reached")
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
        crate::support::fingerprint::test_fingerprint(),
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
