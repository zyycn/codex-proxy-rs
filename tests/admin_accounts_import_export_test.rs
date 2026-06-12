use std::{fs, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use secrecy::ExposeSecret;
use serde_json::json;
use sqlx::Row;
use tokio::sync::Mutex;
use tower::ServiceExt;

use codex_proxy_rs::{
    app::build_router, app::state::AppState, codex::accounts::repository::AccountRepository,
    codex::oauth::TokenPair, storage::db::connect_sqlite, utils::crypto::SecretBox,
};

mod common;

use common::{
    admin_accounts::{
        admin_accounts_test_app, admin_accounts_test_app_with_refresher, import_test_account,
        post_admin_account, test_config, test_jwt, StaticTokenRefresher,
    },
    response_json, seed_admin_session,
};

#[tokio::test]
async fn admin_accounts_import_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool,
        SecretBox::new([11u8; 32]),
    ));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("x-request-id", "req_accounts")
                .body(Body::from(r#"{"accounts":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_accounts");
}

#[tokio::test]
async fn admin_accounts_import_should_store_tokens_encrypted_and_list_sanitized_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool.clone(),
        SecretBox::new([12u8; 32]),
    ));
    let import_body = json!({
        "accounts": [{
            "id": "acct_imported",
            "email": "user@example.com",
            "accountId": "chatgpt-account",
            "userId": "chatgpt-user",
            "label": "primary",
            "planType": "plus",
            "token": "access-secret",
            "refreshToken": "refresh-secret",
            "status": "active"
        }]
    });

    let import_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts")
                .body(Body::from(import_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(import_response.status(), StatusCode::OK);
    let body = response_json(import_response).await;
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["skipped"], 0);
    let stored: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where id = ?",
    )
    .bind("acct_imported")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(stored.0.starts_with("v1:"));
    assert!(!stored.0.contains("access-secret"));
    assert!(stored.1.starts_with("v1:"));
    assert!(!stored.1.contains("refresh-secret"));

    let list_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(list_response.status(), StatusCode::OK);
    let body = response_json(list_response).await;
    assert_eq!(body["data"][0]["id"], "acct_imported");
    assert_eq!(body["data"][0]["email"], "user@example.com");
    assert!(body["data"][0].get("token").is_none());
    assert!(body["data"][0].get("refreshToken").is_none());
    assert_eq!(body["page"]["limit"], 10);
}

#[tokio::test]
async fn admin_accounts_import_should_accept_sub2api_oauth_export_and_mark_format() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-sub2api.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool.clone(),
        SecretBox::new([14u8; 32]),
    ));
    let import_body = json!({
        "type": "sub2api-data",
        "version": 1,
        "proxies": [],
        "accounts": [
            {
                "name": "Sub2API Team",
                "platform": "openai",
                "type": "oauth",
                "credentials": {
                    "access_token": "Bearer sub2api-access-secret",
                    "refresh_token": "rt_sub2api",
                    "email": "team@example.com",
                    "chatgpt_account_id": "chatgpt-account",
                    "chatgpt_user_id": "chatgpt-user",
                    "plan_type": "team"
                },
                "concurrency": 0,
                "priority": 0
            },
            {
                "name": "Other Provider",
                "platform": "anthropic",
                "type": "oauth",
                "credentials": {
                    "access_token": "ignored-secret"
                }
            }
        ]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_sub2api")
                .body(Body::from(import_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["sourceFormat"], "sub2api");
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["skipped"], 0);
    let stored: (String, String, String, String, String) =
        sqlx::query_as("select email, account_id, user_id, label, plan_type from accounts")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored.0, "team@example.com");
    assert_eq!(stored.1, "chatgpt-account");
    assert_eq!(stored.2, "chatgpt-user");
    assert_eq!(stored.3, "Sub2API Team");
    assert_eq!(stored.4, "team");
}

#[tokio::test]
async fn admin_accounts_import_should_accept_sub2api_native_account_export_without_proxy_data() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-sub2api-native.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool,
        SecretBox::new([15u8; 32]),
    ));
    let import_body = json!({
        "accounts": [{
            "id": "acct_sub2api_native",
            "token": "native-access-secret",
            "refreshToken": "native-refresh-secret",
            "email": "native@example.com",
            "accountId": "native-account",
            "userId": "native-user",
            "label": "Native Sub2API",
            "planType": "plus",
            "proxyApiKey": "ignored-proxy-secret",
            "status": "active",
            "usage": {
                "request_count": 1,
                "input_tokens": 0,
                "output_tokens": 0,
                "cached_tokens": 0
            },
            "cachedQuota": null,
            "quotaVerifyRequired": false
        }]
    });

    let import_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_sub2api_native")
                .body(Body::from(import_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(import_response.status(), StatusCode::OK);
    let body = response_json(import_response).await;
    assert_eq!(body["data"]["sourceFormat"], "sub2api");
    assert_eq!(body["data"]["imported"], 1);

    let list_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_sub2api_native_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = response_json(list_response).await;
    assert_eq!(list["data"][0]["id"], "acct_sub2api_native");
    assert_eq!(list["data"][0]["planType"], "plus");
    assert!(list["data"][0].get("proxyApiKey").is_none());
    assert!(list["data"][0].get("usage").is_none());
}

#[tokio::test]
async fn admin_accounts_export_should_return_native_accounts_with_tokens_and_filter_ids() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-export-native.sqlite", 24).await;
    import_test_account(&app, "session_1", "acct_export_a").await;
    import_test_account(&app, "session_1", "acct_export_b").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/export?ids=acct_export_a")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["sourceFormat"], "native");
    assert_eq!(body["data"]["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["accounts"][0]["id"], "acct_export_a");
    assert_eq!(body["data"]["accounts"][0]["token"], "access-acct_export_a");
    assert_eq!(
        body["data"]["accounts"][0]["refreshToken"],
        "refresh-acct_export_a"
    );
    assert!(body["data"]["accounts"][0].get("proxyApiKey").is_none());

    let invalid = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/export?format=proxy")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_accounts_export_should_return_sub2api_openai_oauth_payload_without_proxy_fields() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-export-sub2api.sqlite", 25).await;
    import_test_account(&app, "session_1", "acct_export_sub2api").await;

    let label_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/accounts/acct_export_sub2api/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":"Sub2API Export"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(label_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/export?format=sub2api")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["type"], "sub2api-data");
    assert_eq!(body["data"]["version"], 1);
    assert_eq!(body["data"]["proxies"], json!([]));
    assert_eq!(body["data"]["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["accounts"][0]["name"], "Sub2API Export");
    assert_eq!(body["data"]["accounts"][0]["platform"], "openai");
    assert_eq!(body["data"]["accounts"][0]["type"], "oauth");
    assert_eq!(
        body["data"]["accounts"][0]["credentials"]["access_token"],
        "access-acct_export_sub2api"
    );
    assert_eq!(
        body["data"]["accounts"][0]["credentials"]["refresh_token"],
        "refresh-acct_export_sub2api"
    );
    assert_eq!(
        body["data"]["accounts"][0]["credentials"]["email"],
        "acct_export_sub2api@example.com"
    );
    assert_eq!(
        body["data"]["accounts"][0]["credentials"]["plan_type"],
        "plus"
    );
    assert!(body["data"]["accounts"][0].get("proxy").is_none());
    assert!(body["data"]["accounts"][0].get("proxyUrl").is_none());
}

#[tokio::test]
async fn manual_add_should_derive_claims_ignore_metadata_and_sync_pool() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-account-manual-add.sqlite", 26).await;
    let token = test_jwt(
        Some("jwt-account"),
        Some("jwt-user"),
        Some("jwt@example.com"),
        Some("team"),
        3600,
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "id": "caller-id",
                        "email": "caller@example.com",
                        "accountId": "caller-account",
                        "userId": "caller-user",
                        "label": "Caller Label",
                        "planType": "caller-plan",
                        "token": token,
                        "refreshToken": "manual-refresh-secret",
                        "status": "disabled"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let response_id = body["data"]["id"].as_str().unwrap();
    assert_ne!(response_id, "caller-id");
    assert_eq!(body["data"]["email"], "jwt@example.com");
    assert_eq!(body["data"]["accountId"], "jwt-account");
    assert_eq!(body["data"]["userId"], "jwt-user");
    assert_eq!(body["data"]["planType"], "team");
    assert!(body["data"]["label"].is_null());
    assert_eq!(body["data"]["status"], "active");
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());

    let stored = sqlx::query(
        "select id, email, account_id, user_id, label, plan_type, status, access_token_cipher, refresh_token_cipher from accounts",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored.get::<String, _>("id"), response_id);
    assert_eq!(
        stored.get::<Option<String>, _>("email").as_deref(),
        Some("jwt@example.com")
    );
    assert_eq!(
        stored.get::<Option<String>, _>("account_id").as_deref(),
        Some("jwt-account")
    );
    assert_eq!(
        stored.get::<Option<String>, _>("user_id").as_deref(),
        Some("jwt-user")
    );
    assert_eq!(stored.get::<Option<String>, _>("label"), None);
    assert_eq!(
        stored.get::<Option<String>, _>("plan_type").as_deref(),
        Some("team")
    );
    assert_eq!(stored.get::<String, _>("status"), "active");
    let access_token_cipher = stored.get::<String, _>("access_token_cipher");
    assert!(access_token_cipher.starts_with("v1:"));
    assert!(!access_token_cipher.contains(&token));
    let refresh_token_cipher = stored.get::<String, _>("refresh_token_cipher");
    assert!(refresh_token_cipher.starts_with("v1:"));
    assert!(!refresh_token_cipher.contains("manual-refresh-secret"));

    let acquired = state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .unwrap();
    assert_eq!(acquired.id, response_id);
    assert_eq!(acquired.email.as_deref(), Some("jwt@example.com"));
    assert_eq!(acquired.account_id.as_deref(), Some("jwt-account"));
    assert_eq!(acquired.user_id.as_deref(), Some("jwt-user"));
    assert_eq!(acquired.plan_type.as_deref(), Some("team"));
}

#[tokio::test]
async fn admin_account_manual_add_should_reject_missing_invalid_expired_or_unbound_tokens() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-manual-add-invalid.sqlite", 27).await;

    let missing_tokens = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_tokens.status(), StatusCode::BAD_REQUEST);

    let invalid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "token": "not-a-jwt"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let expired = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "token": test_jwt(
                            Some("expired-account"),
                            Some("expired-user"),
                            Some("expired@example.com"),
                            Some("plus"),
                            -3600,
                        )
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(expired.status(), StatusCode::BAD_REQUEST);

    let missing_account_claim = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "token": test_jwt(
                            None,
                            Some("claimless-user"),
                            Some("claimless@example.com"),
                            Some("free"),
                            3600,
                        )
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_account_claim.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_account_manual_add_should_exchange_refresh_token_only_and_rotate_refresh_token() {
    let token = test_jwt(
        Some("rt-account"),
        Some("rt-user"),
        Some("rt@example.com"),
        Some("plus"),
        3600,
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let refresher = StaticTokenRefresher {
        result: Ok(TokenPair {
            access_token: token.clone(),
            refresh_token: Some("rotated-refresh".to_string()),
        }),
        calls: calls.clone(),
    };
    let (app, state, pool, _dir) = admin_accounts_test_app_with_refresher(
        "admin-account-manual-refresh-only.sqlite",
        28,
        refresher,
    )
    .await;

    let response = post_admin_account(
        &app,
        json!({
            "refreshToken": "initial-refresh",
            "email": "caller@example.com",
            "planType": "caller-plan"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["email"], "rt@example.com");
    assert_eq!(body["data"]["accountId"], "rt-account");
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());
    assert_eq!(*calls.lock().await, vec!["initial-refresh".to_string()]);

    let repo = AccountRepository::new(pool, SecretBox::new([28u8; 32]));
    let stored = repo
        .get(body["data"]["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose_secret(), &token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "rotated-refresh"
    );
    let acquired = state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .unwrap();
    assert_eq!(acquired.refresh_token.as_deref(), Some("rotated-refresh"));
}

#[tokio::test]
async fn admin_account_manual_add_should_preserve_input_refresh_token_when_exchange_omits_rotation()
{
    let token = test_jwt(
        Some("rt-preserve-account"),
        Some("rt-preserve-user"),
        Some("preserve@example.com"),
        Some("free"),
        3600,
    );
    let refresher = StaticTokenRefresher {
        result: Ok(TokenPair {
            access_token: token,
            refresh_token: None,
        }),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_refresher(
        "admin-account-manual-refresh-preserve.sqlite",
        29,
        refresher,
    )
    .await;

    let response = post_admin_account(&app, json!({ "refreshToken": "preserved-refresh" })).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let repo = AccountRepository::new(pool, SecretBox::new([29u8; 32]));
    let stored = repo
        .get(body["data"]["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "preserved-refresh"
    );
}

#[tokio::test]
async fn admin_account_manual_add_should_update_existing_account_for_same_account_and_user() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-account-manual-update-existing.sqlite", 30).await;
    let first_token = test_jwt(
        Some("team-account"),
        Some("same-user"),
        Some("first@example.com"),
        Some("free"),
        3600,
    );
    let first_response = post_admin_account(
        &app,
        json!({
            "token": first_token,
            "refreshToken": "first-refresh"
        }),
    )
    .await;
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = response_json(first_response).await;
    let first_id = first_body["data"]["id"].as_str().unwrap().to_string();

    let second_token = test_jwt(
        Some("team-account"),
        Some("same-user"),
        Some("second@example.com"),
        Some("team"),
        7200,
    );
    let second_response = post_admin_account(&app, json!({ "token": second_token })).await;

    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = response_json(second_response).await;
    assert_eq!(second_body["data"]["id"], first_id);
    assert_eq!(second_body["data"]["email"], "second@example.com");
    assert_eq!(second_body["data"]["planType"], "team");

    let count: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
    let repo = AccountRepository::new(pool, SecretBox::new([30u8; 32]));
    let stored = repo.get(&first_id).await.unwrap().unwrap();
    assert_eq!(stored.access_token.expose_secret(), &second_token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "first-refresh"
    );
    assert_eq!(stored.email.as_deref(), Some("second@example.com"));
    assert_eq!(stored.plan_type.as_deref(), Some("team"));

    let acquired = state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .unwrap();
    assert_eq!(acquired.id, first_id);
    assert_eq!(acquired.access_token, second_token);
    assert_eq!(acquired.refresh_token.as_deref(), Some("first-refresh"));
}

#[tokio::test]
async fn manual_add_should_preserve_existing_refresh_when_refresh_only_omits_rotation() {
    let refreshed_token = test_jwt(
        Some("rt-existing-account"),
        Some("rt-existing-user"),
        Some("second@example.com"),
        Some("team"),
        7200,
    );
    let refresher = StaticTokenRefresher {
        result: Ok(TokenPair {
            access_token: refreshed_token.clone(),
            refresh_token: None,
        }),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_refresher(
        "admin-account-manual-refresh-existing-preserve.sqlite",
        31,
        refresher,
    )
    .await;
    let first_token = test_jwt(
        Some("rt-existing-account"),
        Some("rt-existing-user"),
        Some("first@example.com"),
        Some("free"),
        3600,
    );
    let first_response = post_admin_account(
        &app,
        json!({
            "token": first_token,
            "refreshToken": "old-refresh"
        }),
    )
    .await;
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = response_json(first_response).await;
    let account_id = first_body["data"]["id"].as_str().unwrap().to_string();

    let second_response =
        post_admin_account(&app, json!({ "refreshToken": "incoming-refresh" })).await;

    assert_eq!(second_response.status(), StatusCode::OK);
    let repo = AccountRepository::new(pool, SecretBox::new([31u8; 32]));
    let stored = repo.get(&account_id).await.unwrap().unwrap();
    assert_eq!(stored.access_token.expose_secret(), &refreshed_token);
    assert_eq!(stored.refresh_token.unwrap().expose_secret(), "old-refresh");
    assert_eq!(stored.email.as_deref(), Some("second@example.com"));
    assert_eq!(stored.plan_type.as_deref(), Some("team"));
}

#[tokio::test]
async fn admin_accounts_import_cli_should_read_codex_auth_file_store_encrypted_and_sync_pool() {
    let (app, state, pool, dir) =
        admin_accounts_test_app("admin-account-import-cli.sqlite", 28).await;
    let codex_home = dir.path().join("codex-home");
    fs::create_dir_all(&codex_home).unwrap();
    let token = test_jwt(
        Some("cli-account"),
        Some("cli-user"),
        Some("cli@example.com"),
        Some("plus"),
        3600,
    );
    fs::write(
        codex_home.join("auth.json"),
        json!({
            "access_token": token,
            "refresh_token": "cli-refresh-secret"
        })
        .to_string(),
    )
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import-cli")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_import_cli")
                .body(Body::from(
                    json!({
                        "codexHome": codex_home.display().to_string(),
                        "id": "caller-id",
                        "email": "caller@example.com",
                        "accountId": "caller-account",
                        "userId": "caller-user",
                        "label": "caller-label",
                        "planType": "caller-plan"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["sourceFormat"], "codex_cli");
    assert_eq!(body["data"]["imported"], 1);
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());

    let stored: (
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        String,
        String,
    ) = sqlx::query_as(
        "select id, email, account_id, user_id, label, plan_type, access_token_cipher, refresh_token_cipher from accounts limit 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_ne!(stored.0, "caller-id");
    assert_eq!(stored.1, "cli@example.com");
    assert_eq!(stored.2, "cli-account");
    assert_eq!(stored.3, "cli-user");
    assert_eq!(stored.4, None);
    assert_eq!(stored.5, "plus");
    assert!(stored.6.starts_with("v1:"));
    assert!(!stored.6.contains(&token));
    assert!(stored.7.starts_with("v1:"));
    assert!(!stored.7.contains("cli-refresh-secret"));

    let acquired = state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .unwrap();
    assert_eq!(acquired.id, stored.0);
}
