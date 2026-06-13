use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tokio::sync::Mutex;
use tower::ServiceExt;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::codex::gateway::oauth::{RefreshFailure, TokenPair};

use crate::support::{
    admin_accounts::{
        admin_accounts_test_app, admin_accounts_test_app_with_base_url,
        admin_accounts_test_app_with_refresher, import_test_account, test_jwt,
        FailingTokenRefresher, StaticTokenRefresher,
    },
    response_json,
};

#[tokio::test]
async fn admin_account_cookies_should_set_get_and_delete_encrypted_cookie_header() {
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-account-cookies.sqlite", 22).await;
    import_test_account(&app, "session_1", "acct_cookies").await;

    let set_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/acct_cookies/cookies")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    r#"{"cookies":"cf_clearance=clear-secret; __cf_bm=bm-secret"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(set_response.status(), StatusCode::OK);
    let body = response_json(set_response).await;
    assert_eq!(
        body["data"]["cookies"],
        "__cf_bm=bm-secret; cf_clearance=clear-secret"
    );

    let stored = sqlx::query_as::<_, (String, String)>(
        "select name, value_cipher from account_cookies where account_id = ? order by name asc",
    )
    .bind("acct_cookies")
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(stored.len(), 2);
    assert!(stored.iter().all(|(_, cipher)| cipher.starts_with("v1:")));
    assert!(stored
        .iter()
        .all(|(_, cipher)| !cipher.contains("clear-secret") && !cipher.contains("bm-secret")));

    let get_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/acct_cookies/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_response.status(), StatusCode::OK);
    let body = response_json(get_response).await;
    assert_eq!(
        body["data"]["cookies"],
        "__cf_bm=bm-secret; cf_clearance=clear-secret"
    );

    let delete_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/accounts/acct_cookies/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_response.status(), StatusCode::OK);

    let get_empty = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/acct_cookies/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_empty.status(), StatusCode::OK);
    let body = response_json(get_empty).await;
    assert!(body["data"]["cookies"].is_null());
}

#[tokio::test]
async fn admin_account_cookies_should_require_existing_account_and_non_empty_cookies() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-cookies-invalid.sqlite", 23).await;

    let missing = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/missing/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    import_test_account(&app, "session_1", "acct_cookie_invalid").await;
    let invalid = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/acct_cookie_invalid/cookies")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"cookies":""}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_account_refresh_should_update_tokens_and_runtime_pool_without_returning_secrets() {
    let refreshed_access_token = test_jwt(
        Some("chatgpt-account"),
        Some("chatgpt-user"),
        Some("refresh@example.com"),
        Some("plus"),
        3600,
    );
    let (app, state, pool, _dir) = admin_accounts_test_app_with_refresher(
        "admin-account-refresh.sqlite",
        28,
        StaticTokenRefresher {
            result: Ok(TokenPair {
                access_token: refreshed_access_token.clone(),
                refresh_token: Some("new-admin-refresh-rt".to_string()),
            }),
            calls: Arc::new(Mutex::new(Vec::new())),
        },
    )
    .await;
    import_test_account(&app, "session_1", "acct_refresh").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/acct_refresh/refresh")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_refresh_account")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["id"], "acct_refresh");
    assert_eq!(body["data"]["result"], "alive");
    assert_eq!(body["data"]["previousStatus"], "active");
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains(&refreshed_access_token));
    assert!(!serialized.contains("new-admin-refresh-rt"));

    let stored: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where id = ?",
    )
    .bind("acct_refresh")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(stored.0.starts_with("v1:"));
    assert!(!stored.0.contains("new-admin-refresh-access"));
    assert!(stored.1.starts_with("v1:"));
    assert!(!stored.1.contains("new-admin-refresh-rt"));
    let acquired = state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .unwrap();
    assert_eq!(acquired.access_token, refreshed_access_token);
}

#[tokio::test]
async fn admin_account_refresh_should_mark_invalid_refresh_token_as_expired() {
    let (app, state, pool, _dir) = admin_accounts_test_app_with_refresher(
        "admin-account-refresh-invalid.sqlite",
        29,
        FailingTokenRefresher {
            failure: RefreshFailure::InvalidGrant,
        },
    )
    .await;
    import_test_account(&app, "session_1", "acct_refresh_invalid").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/acct_refresh_invalid/refresh")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_refresh_invalid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["result"], "dead");
    assert_eq!(body["data"]["status"], "expired");
    let status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_refresh_invalid")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status.0, "expired");
    assert!(state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .is_none());
}

#[tokio::test]
async fn admin_account_reset_usage_should_clear_local_counters_and_pool_last_used() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-account-reset-usage.sqlite", 30).await;
    import_test_account(&app, "session_1", "acct_reset_usage").await;
    sqlx::query(
        "insert into account_usage (account_id, request_count, input_tokens, output_tokens, cached_tokens, last_used_at) values (?, 7, 11, 13, 17, ?)",
    )
    .bind("acct_reset_usage")
    .bind("2026-06-12T12:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    state.reload_account_pool_from_repository().await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/acct_reset_usage/reset-usage")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_reset_usage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["id"], "acct_reset_usage");
    assert_eq!(body["data"]["reset"], true);
    let usage: (i64, i64, i64, i64, Option<String>) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens, cached_tokens, last_used_at from account_usage where account_id = ?",
    )
    .bind("acct_reset_usage")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(usage, (0, 0, 0, 0, None));
    let acquired = state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .unwrap();
    assert_eq!(acquired.request_count, 1);
    assert_ne!(
        acquired.last_used_at.as_deref(),
        Some("2026-06-12T12:00:00Z")
    );
}

#[tokio::test]
async fn admin_account_quota_should_fetch_usage_store_quota_and_not_return_secrets() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-acct_quota"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 25,
                    "limit_window_seconds": 300,
                    "reset_after_seconds": 120,
                    "reset_at": 1770000000
                },
                "secondary_window": null
            },
            "code_review_rate_limit": null,
            "additional_rate_limits": null,
            "credits": {
                "has_credits": false,
                "unlimited": false,
                "overage_limit_reached": false,
                "balance": "0"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) =
        admin_accounts_test_app_with_base_url("admin-account-quota.sqlite", 31, server.uri()).await;
    import_test_account(&app, "session_1", "acct_quota").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/acct_quota/quota")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_quota")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["quota"]["plan_type"], "plus");
    assert_eq!(body["data"]["quota"]["rate_limit"]["remaining_percent"], 75);
    assert_eq!(
        body["data"]["raw"]["rate_limit"]["primary_window"]["used_percent"],
        25
    );
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("access-acct_quota"));
    assert!(!serialized.contains("refresh-acct_quota"));
    let stored: (String,) = sqlx::query_as("select quota_json from accounts where id = ?")
        .bind("acct_quota")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stored.0.contains("\"remaining_percent\":75"));
    assert!(!stored.0.contains("access-acct_quota"));
}

#[tokio::test]
async fn admin_account_quota_warnings_should_require_admin_session_cookie() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-quota-warnings-auth.sqlite", 33).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/quota-warnings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_account_quota_warnings_should_return_threshold_matches_from_cached_quota() {
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-account-quota-warnings.sqlite", 34).await;
    import_test_account(&app, "session_1", "acct_warn").await;
    import_test_account(&app, "session_1", "acct_quiet").await;

    sqlx::query(
        "update accounts set quota_json = ?, quota_fetched_at = ?, updated_at = ? where id = ?",
    )
    .bind(
        json!({
            "rate_limit": {
                "used_percent": 85,
                "reset_at": 1770000100
            },
            "secondary_rate_limit": {
                "used_percent": 91,
                "reset_at": 1770000200
            }
        })
        .to_string(),
    )
    .bind("2026-06-13T00:00:00Z")
    .bind("2026-06-13T00:00:00Z")
    .bind("acct_warn")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "update accounts set quota_json = ?, quota_fetched_at = ?, updated_at = ? where id = ?",
    )
    .bind(
        json!({
            "rate_limit": {
                "used_percent": 25,
                "reset_at": 1770000300
            },
            "secondary_rate_limit": null
        })
        .to_string(),
    )
    .bind("2026-06-13T01:00:00Z")
    .bind("2026-06-13T01:00:00Z")
    .bind("acct_quiet")
    .execute(&pool)
    .await
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/quota-warnings")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["updatedAt"], "2026-06-13T00:00:00Z");
    let warnings = body["data"]["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 2);
    assert!(warnings
        .iter()
        .all(|warning| warning["accountId"] == "acct_warn"));
    assert!(warnings.iter().all(|warning| {
        warning["email"] == "acct_warn@example.com" && warning["usedPercent"].as_f64().is_some()
    }));
    assert!(warnings.iter().any(|warning| {
        warning["window"] == "primary"
            && warning["level"] == "warning"
            && warning["usedPercent"] == 85.0
            && warning["resetAt"] == 1770000100
    }));
    assert!(warnings.iter().any(|warning| {
        warning["window"] == "secondary"
            && warning["level"] == "critical"
            && warning["usedPercent"] == 91.0
            && warning["resetAt"] == 1770000200
    }));
}

#[tokio::test]
async fn admin_accounts_health_check_should_probe_backend_and_mark_invalid_accounts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-acct_health_alive"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": null,
                "secondary_window": null
            },
            "code_review_rate_limit": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-acct_health_dead"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "invalid token"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) =
        admin_accounts_test_app_with_base_url("admin-account-health.sqlite", 32, server.uri())
            .await;
    import_test_account(&app, "session_1", "acct_health_alive").await;
    import_test_account(&app, "session_1", "acct_health_dead").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/health-check")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_health")
                .body(Body::from(
                    json!({
                        "ids": ["acct_health_alive", "acct_health_dead"],
                        "concurrency": 2
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["summary"]["total"], 2);
    assert_eq!(body["data"]["summary"]["alive"], 1);
    assert_eq!(body["data"]["summary"]["dead"], 1);
    assert_eq!(body["data"]["summary"]["skipped"], 0);
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("access-acct_health_alive"));
    assert!(!serialized.contains("access-acct_health_dead"));
    let status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_health_dead")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status.0, "expired");
}
