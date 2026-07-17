use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration as StdDuration,
};

use chrono::{Duration, Utc};
use codex_proxy_rs::fleet::{
    account::AccountStatus,
    account_gateway::{AccountUpstreamContext, AccountUpstreamGateway},
    cookies::PgCookieStore,
    pool::{AccountPoolOptions, AccountPoolService},
    quota::{
        QuotaLimitObservation, QuotaObservation, QuotaRefreshService, QuotaSnapshot,
        QuotaWindowObservation, quota_from_observation, quota_snapshot_limit_reached,
        quota_snapshot_limit_window_seconds, quota_snapshot_reset_at,
    },
    store::{AccountStore, NewAccount, PgAccountStore},
};
use codex_proxy_rs::infra::identity::AccountPseudonymizer;
use codex_proxy_rs::telemetry::account_usage::store::PgAccountUsageStore;
use codex_proxy_rs::upstream::openai::transport::CodexBackendClient;
use secrecy::SecretString;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use crate::support::storage::init_test_db;

fn test_account_pseudonymizer() -> Arc<AccountPseudonymizer> {
    Arc::new(AccountPseudonymizer::new([7; 32]))
}

fn test_account_pool(pool: &sqlx::PgPool) -> Arc<AccountPoolService> {
    Arc::new(AccountPoolService::new(
        Arc::new(PgAccountStore::new(pool.clone())) as Arc<dyn AccountStore>,
        Arc::new(PgAccountUsageStore::new(pool.clone())),
        AccountPoolOptions::default(),
        0,
    ))
}

async fn quota_from_usage_fixture(usage: Value) -> QuotaSnapshot {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(usage))
        .mount(&server)
        .await;
    let codex = CodexBackendClient::new(
        reqwest::Client::new(),
        server.uri(),
        crate::support::wire_profile::test_wire_profile(),
    );
    AccountUpstreamGateway::fetch_usage(
        &codex,
        AccountUpstreamContext {
            access_token: SecretString::new("access-token".to_string().into()),
            account_id: Some("upstream-account".to_string()),
            request_id: "quota-fixture".to_string(),
            cookie_header: None,
            installation_id: Some("installation-id".to_string()),
        },
    )
    .await
    .expect("usage should parse")
    .quota
}

#[tokio::test]
async fn quota_from_usage_should_preserve_spend_control_individual_limit() {
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

    let quota = quota_from_usage_fixture(usage).await;

    let monthly = quota.monthly_limit.expect("monthly limit");
    assert_eq!(monthly.source.as_deref(), Some("spend_control"));
    assert_eq!(monthly.used_percent, Some(52.0));
}

#[tokio::test]
async fn quota_from_usage_should_preserve_additional_limit_snapshot_by_limit_name() {
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

    let quota = quota_from_usage_fixture(usage).await;

    let limit = quota.limits.first().expect("additional limit");
    assert_eq!(limit.source, "additional");
    assert_eq!(limit.limit_name.as_deref(), Some("gpt-5.3-codex-spark"));
    assert!(limit.blocked);
    assert_eq!(
        limit
            .secondary
            .as_ref()
            .and_then(|window| window.window_minutes),
        Some(10_080)
    );
}

#[tokio::test]
async fn quota_from_usage_should_preserve_additional_limit_name_and_metered_feature() {
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

    let quota = quota_from_usage_fixture(usage).await;

    let limit = quota.limits.first().expect("additional limit");
    assert_eq!(limit.limit_name.as_deref(), Some("Code review"));
    assert_eq!(limit.metered_feature.as_deref(), Some("review"));
}

#[tokio::test]
async fn quota_snapshot_limit_reached_should_keep_allowed_free_account_without_reset_credits_available()
 {
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
    let quota = quota_from_usage_fixture(usage).await;

    assert!(!quota_snapshot_limit_reached(&quota));
}

#[tokio::test]
async fn quota_snapshot_reset_at_should_use_non_blocking_core_window() {
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
        }
    });
    let quota = quota_from_usage_fixture(usage).await;

    assert_eq!(
        quota_snapshot_reset_at(&quota).map(|reset_at| reset_at.timestamp()),
        Some(1_806_364_800)
    );
    assert_eq!(quota_snapshot_limit_window_seconds(&quota), Some(2_592_000));
}

#[test]
fn quota_snapshot_reset_at_should_prefer_core_window_over_preserved_monthly_limit() {
    let quota = QuotaSnapshot::from_value(json!({
        "snapshots": [{
            "source": "core",
            "blocked": false,
            "primary": {
                "used_percent": 25,
                "remaining_percent": 75,
                "reset_at": 1_893_456_300,
                "window_minutes": 5,
                "limit_reached": false
            },
            "secondary": null
        }],
        "monthly_limit": {
            "key": "spend-control-monthly",
            "source": "spend_control",
            "used_percent": 52,
            "remaining_percent": 48,
            "reset_at": 1_896_048_000,
            "window_minutes": 43200,
            "limit_reached": false
        }
    }))
    .expect("quota should parse");

    assert_eq!(
        quota_snapshot_reset_at(&quota).map(|reset_at| reset_at.timestamp()),
        Some(1_893_456_300)
    );
    assert_eq!(quota_snapshot_limit_window_seconds(&quota), Some(300));
}

#[test]
fn quota_snapshot_limit_reached_should_block_explicit_credit_overage_limit() {
    let quota = QuotaSnapshot::from_value(json!({
        "credits": {
            "has_credits": false,
            "unlimited": false,
            "overage_limit_reached": true,
            "balance": 0
        }
    }))
    .expect("quota should parse");

    assert!(quota_snapshot_limit_reached(&quota));
}

#[test]
fn quota_snapshot_limit_reached_should_block_any_exhausted_window() {
    let quota = QuotaSnapshot::from_value(json!({
        "snapshots": [{
            "source": "additional",
            "blocked": false,
            "allowed": true,
            "primary": {
                "used_percent": 100,
                "remaining_percent": 0,
                "reset_at": 1_893_456_300,
                "window_minutes": 300,
                "limit_reached": false
            },
            "secondary": null
        }]
    }))
    .expect("quota should parse");

    assert!(quota_snapshot_limit_reached(&quota));
}

#[test]
fn quota_snapshot_window_should_follow_blocking_window() {
    let quota = QuotaSnapshot::from_value(json!({
        "snapshots": [{
            "source": "additional",
            "blocked": false,
            "primary": {
                "used_percent": 100,
                "remaining_percent": 0,
                "reset_at": 1_893_456_300,
                "window_minutes": 10_080,
                "limit_reached": false
            },
            "secondary": null
        }]
    }))
    .expect("quota should parse");

    assert_eq!(
        (
            quota_snapshot_reset_at(&quota).map(|reset_at| reset_at.timestamp()),
            quota_snapshot_limit_window_seconds(&quota),
        ),
        (Some(1_893_456_300), Some(604_800))
    );
}

#[test]
fn quota_observation_should_preserve_unobserved_monthly_limit_and_credits() {
    let existing = QuotaSnapshot::from_value(json!({
        "monthly_limit": {
            "key": "spend-control-monthly",
            "source": "spend_control",
            "used_percent": 52,
            "remaining_percent": 48,
            "reset_at": 1896048000,
            "window_minutes": 43200,
            "limit_reached": false
        },
        "credits": {
            "has_credits": true,
            "unlimited": false,
            "balance": 12
        }
    }))
    .expect("existing quota should parse");
    let observation = QuotaObservation {
        limits: BTreeMap::from([(
            "codex".to_string(),
            QuotaLimitObservation {
                limit_id: "codex".to_string(),
                limit_name: None,
                metered_feature: None,
                allowed: None,
                limit_reached: None,
                primary: Some(QuotaWindowObservation {
                    used_percent: 25.0,
                    window_minutes: Some(5),
                    reset_at: Some(1_893_456_300),
                    used: None,
                    limit: None,
                }),
                secondary: None,
            },
        )]),
        ..QuotaObservation::default()
    };

    let quota = quota_from_observation(&observation, Some("plus"), Some(&existing));

    assert_eq!(quota.plan_type.as_deref(), Some("plus"));
    assert_eq!(
        quota.limits[0]
            .primary
            .as_ref()
            .and_then(|window| window.remaining_percent),
        Some(75)
    );
    assert_eq!(
        quota
            .monthly_limit
            .as_ref()
            .and_then(|limit| limit.used_percent),
        Some(52.0)
    );
    assert_eq!(
        quota
            .credits
            .as_ref()
            .and_then(|credits| credits.balance.as_ref()),
        Some(&json!(12))
    );
}

#[test]
fn quota_observation_should_block_exhausted_window_without_explicit_flag() {
    let observation = QuotaObservation {
        limits: BTreeMap::from([(
            "codex".to_string(),
            QuotaLimitObservation {
                limit_id: "codex".to_string(),
                limit_name: None,
                metered_feature: None,
                allowed: None,
                limit_reached: Some(false),
                primary: Some(QuotaWindowObservation {
                    used_percent: 100.0,
                    window_minutes: Some(300),
                    reset_at: Some(1_893_456_300),
                    used: None,
                    limit: None,
                }),
                secondary: None,
            },
        )]),
        ..QuotaObservation::default()
    };

    let quota = quota_from_observation(&observation, Some("plus"), None);

    assert!(quota.limits[0].blocked);
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

    let (pool, _guard) = init_test_db("quota-refresh-cookie").await;
    let store = PgAccountStore::new(pool.clone());
    let cookies = PgCookieStore::new(pool.clone());
    insert_quota_locked_account(&store, &pool, "acct-quota-cookie", "access-token-cookie").await;
    cookies
        .capture_set_cookie(
            "acct-quota-cookie",
            "cf_clearance=quota-refresh; Domain=.chatgpt.com; Path=/",
        )
        .await
        .expect("cookie should be stored");
    let codex = CodexBackendClient::new(
        reqwest::Client::new(),
        server.uri(),
        crate::support::wire_profile::test_wire_profile(),
    );
    let service = QuotaRefreshService::new(
        Arc::new(store.clone()),
        Arc::new(PgAccountUsageStore::new(pool.clone())),
        Arc::new(codex),
        test_account_pool(&pool),
        test_account_pseudonymizer(),
    )
    .with_cookie_store(cookies);

    let mut last_refreshed = HashMap::new();
    let summary = service
        .refresh_locked_accounts(&mut last_refreshed)
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

    let (pool, _guard) = init_test_db("quota-refresh").await;
    let store = PgAccountStore::new(pool.clone());
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
    sqlx::query("update accounts set quota_limit_reached = true where id = $1")
        .bind("acct-quota")
        .execute(&pool)
        .await
        .expect("quota lock should be set");
    let codex = CodexBackendClient::new(
        reqwest::Client::new(),
        server.uri(),
        crate::support::wire_profile::test_wire_profile(),
    );
    let service = QuotaRefreshService::new(
        Arc::new(store.clone()),
        Arc::new(PgAccountUsageStore::new(pool.clone())),
        Arc::new(codex),
        test_account_pool(&pool),
        test_account_pseudonymizer(),
    );

    let mut last_refreshed = HashMap::new();
    let summary = service
        .refresh_locked_accounts(&mut last_refreshed)
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
async fn quota_refresh_service_should_ban_deactivated_workspace() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(
            ResponseTemplate::new(402)
                .set_body_json(json!({ "detail": { "code": "deactivated_workspace" } })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (pool, _guard) = init_test_db("quota-refresh-deactivated-workspace").await;
    let store = PgAccountStore::new(pool.clone());
    insert_quota_locked_account(&store, &pool, "acct-deactivated", "access-token").await;
    let codex = Arc::new(CodexBackendClient::new(
        reqwest::Client::new(),
        server.uri(),
        crate::support::wire_profile::test_wire_profile(),
    ));
    let service = QuotaRefreshService::new(
        Arc::new(store.clone()),
        Arc::new(PgAccountUsageStore::new(pool.clone())),
        codex,
        test_account_pool(&pool),
        test_account_pseudonymizer(),
    );

    let summary = service
        .refresh_locked_accounts(&mut HashMap::new())
        .await
        .expect("quota refresh should complete");
    let status: String = sqlx::query_scalar("select status from accounts where id = $1")
        .bind("acct-deactivated")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(summary.failed, 1);
    assert_eq!(status, "banned");
}

#[tokio::test]
async fn quota_refresh_service_should_refresh_quota_exhausted_accounts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 12,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 18_000
                }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (pool, _guard) = init_test_db("quota-refresh-exhausted").await;
    let store = PgAccountStore::new(pool.clone());
    insert_quota_locked_account(&store, &pool, "acct-quota", "access-token").await;
    store
        .set_status("acct-quota", AccountStatus::QuotaExhausted)
        .await
        .expect("status should update");
    sqlx::query(
        "update accounts set quota_limit_reached = false, quota_verify_required = false where id = $1",
    )
    .bind("acct-quota")
    .execute(&pool)
    .await
    .expect("quota flags should reset");
    let codex = CodexBackendClient::new(
        reqwest::Client::new(),
        server.uri(),
        crate::support::wire_profile::test_wire_profile(),
    );
    let service = QuotaRefreshService::new(
        Arc::new(store.clone()),
        Arc::new(PgAccountUsageStore::new(pool.clone())),
        Arc::new(codex),
        test_account_pool(&pool),
        test_account_pseudonymizer(),
    );

    let mut last_refreshed = HashMap::new();
    let summary = service
        .refresh_locked_accounts(&mut last_refreshed)
        .await
        .expect("quota refresh should succeed");
    let stored: (String, bool) =
        sqlx::query_as("select status, quota_limit_reached from accounts where id = $1")
            .bind("acct-quota")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(summary.refreshed, 1);
    assert_eq!(stored, ("active".to_string(), false));
}

#[tokio::test]
async fn quota_refresh_service_should_skip_recent_locked_account_before_cooldown_expires() {
    let server = MockServer::start().await;

    let (pool, _guard) = init_test_db("quota-refresh-recent-future-cooldown").await;
    let store = PgAccountStore::new(pool.clone());
    insert_quota_locked_account(&store, &pool, "acct-quota", "access-token").await;
    set_quota_cooldown_until(&pool, "acct-quota", Utc::now() + Duration::minutes(5)).await;
    let codex = CodexBackendClient::new(
        reqwest::Client::new(),
        server.uri(),
        crate::support::wire_profile::test_wire_profile(),
    );
    let service = QuotaRefreshService::with_min_refresh_interval_secs(
        Arc::new(store.clone()),
        Arc::new(PgAccountUsageStore::new(pool.clone())),
        Arc::new(codex),
        test_account_pool(&pool),
        test_account_pseudonymizer(),
        1_800,
    );

    let mut last_refreshed = HashMap::new();
    last_refreshed.insert("acct-quota".to_string(), tokio::time::Instant::now());
    let summary = service
        .refresh_locked_accounts(&mut last_refreshed)
        .await
        .expect("quota refresh should succeed");

    assert_eq!(
        (
            summary.candidates,
            summary.skipped_cooldown,
            summary.skipped_recent,
            summary.refreshed,
            usage_request_count(&server).await,
        ),
        (1, 1, 0, 0, 0)
    );
}

#[tokio::test]
async fn quota_refresh_service_should_skip_recent_locked_account_inside_cooldown_grace() {
    let server = MockServer::start().await;

    let (pool, _guard) = init_test_db("quota-refresh-recent-cooldown-grace").await;
    let store = PgAccountStore::new(pool.clone());
    insert_quota_locked_account(&store, &pool, "acct-quota", "access-token").await;
    set_quota_cooldown_until(&pool, "acct-quota", Utc::now() - Duration::seconds(20)).await;
    let codex = CodexBackendClient::new(
        reqwest::Client::new(),
        server.uri(),
        crate::support::wire_profile::test_wire_profile(),
    );
    let service = QuotaRefreshService::with_min_refresh_interval_secs(
        Arc::new(store.clone()),
        Arc::new(PgAccountUsageStore::new(pool.clone())),
        Arc::new(codex),
        test_account_pool(&pool),
        test_account_pseudonymizer(),
        1_800,
    );

    let mut last_refreshed = HashMap::new();
    last_refreshed.insert("acct-quota".to_string(), tokio::time::Instant::now());
    let summary = service
        .refresh_locked_accounts(&mut last_refreshed)
        .await
        .expect("quota refresh should succeed");

    assert_eq!(
        (
            summary.candidates,
            summary.skipped_cooldown,
            summary.skipped_recent,
            summary.refreshed,
            usage_request_count(&server).await,
        ),
        (1, 1, 0, 0, 0)
    );
}

#[tokio::test]
async fn quota_refresh_service_should_bypass_recent_skip_after_cooldown_grace() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 8,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 18_000
                }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (pool, _guard) = init_test_db("quota-refresh-recent-expired-cooldown").await;
    let store = PgAccountStore::new(pool.clone());
    insert_quota_locked_account(&store, &pool, "acct-quota", "access-token").await;
    set_quota_cooldown_until(&pool, "acct-quota", Utc::now() - Duration::seconds(31)).await;
    let codex = CodexBackendClient::new(
        reqwest::Client::new(),
        server.uri(),
        crate::support::wire_profile::test_wire_profile(),
    );
    let service = QuotaRefreshService::with_min_refresh_interval_secs(
        Arc::new(store.clone()),
        Arc::new(PgAccountUsageStore::new(pool.clone())),
        Arc::new(codex),
        test_account_pool(&pool),
        test_account_pseudonymizer(),
        1_800,
    );

    let mut last_refreshed = HashMap::new();
    last_refreshed.insert("acct-quota".to_string(), tokio::time::Instant::now());
    let summary = service
        .refresh_locked_accounts(&mut last_refreshed)
        .await
        .expect("quota refresh should succeed");

    assert_eq!(
        (
            summary.skipped_recent,
            summary.skipped_cooldown,
            summary.refreshed,
            usage_request_count(&server).await,
        ),
        (0, 0, 1, 1)
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

    let (pool, _guard) = init_test_db("quota-refresh-spacing").await;
    let store = PgAccountStore::new(pool.clone());
    insert_quota_locked_account(&store, &pool, "acct-quota-1", "access-token-1").await;
    insert_quota_locked_account(&store, &pool, "acct-quota-2", "access-token-2").await;
    let codex = CodexBackendClient::new(
        reqwest::Client::new(),
        server.uri(),
        crate::support::wire_profile::test_wire_profile(),
    );
    let service = QuotaRefreshService::new(
        Arc::new(store.clone()),
        Arc::new(PgAccountUsageStore::new(pool.clone())),
        Arc::new(codex),
        test_account_pool(&pool),
        test_account_pseudonymizer(),
    )
    .with_request_spacing(StdDuration::from_millis(200));

    let refresh = tokio::spawn(async move {
        let mut last_refreshed = HashMap::new();
        service.refresh_locked_accounts(&mut last_refreshed).await
    });
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
    store: &PgAccountStore,
    pool: &sqlx::PgPool,
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
    sqlx::query("update accounts set quota_limit_reached = true where id = $1")
        .bind(id)
        .execute(pool)
        .await
        .expect("quota lock should be set");
}

async fn set_quota_cooldown_until(
    pool: &sqlx::PgPool,
    id: &str,
    cooldown_until: chrono::DateTime<Utc>,
) {
    sqlx::query("update accounts set quota_cooldown_until = $1 where id = $2")
        .bind(cooldown_until)
        .bind(id)
        .execute(pool)
        .await
        .expect("quota cooldown should be set");
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
