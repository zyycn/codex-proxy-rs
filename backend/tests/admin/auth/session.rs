use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Utc;
use codex_proxy_rs::infra::{database::connect_sqlite, identity::hash_admin_password};
use sqlx::SqlitePool;
use tower::util::ServiceExt;

use crate::support::{config::test_config, fingerprint::runtime_fingerprint, http::response_json};

#[tokio::test]
async fn admin_login_should_issue_http_only_session_cookie() {
    let (app, _dir, _pool) = admin_login_test_app("admin-login.sqlite", "correct-password").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header("content-type", "application/json")
                .header("x-request-id", "req_login")
                .body(Body::from(r#"{"password":"correct-password"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);

    assert_eq!(status, StatusCode::OK, "login status");
    let body = response_json(response).await;
    let cookie = cookie.expect("login should set admin session cookie");
    assert!(cookie.starts_with("cpr_admin_session="));
    assert!(cookie.contains("Secure"));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
    assert_eq!(body["code"], 200);
    assert!(body["data"]["expiresAt"].is_string());

    let usage_records_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage/records")
                .header("cookie", cookie.split(';').next().unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(usage_records_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_login_should_reject_client_api_key_as_password_or_authorization() {
    let (app, _dir, _pool) =
        admin_login_test_app("admin-login-rejects-client-key.sqlite", "correct-password").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header("content-type", "application/json")
                .header("authorization", "Bearer cpr_not_an_admin_session")
                .header("x-request-id", "req_login_bad")
                .body(Body::from(r#"{"password":"cpr_not_an_admin_password"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let has_cookie = response.headers().get("set-cookie").is_some();

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert!(!has_cookie);
    assert_eq!(body["code"], 40102);
}

#[tokio::test]
async fn admin_login_should_throttle_repeated_failures_from_same_source() {
    let (app, _dir, _pool) =
        admin_login_test_app("admin-login-throttle.sqlite", "correct-password").await;

    for _ in 0..5 {
        let response = post_login(&app, "wrong-password").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    let response = post_login(&app, "correct-password").await;
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], 42901);
}

#[tokio::test]
async fn admin_login_should_not_write_usage_record_on_success() {
    let (app, _dir, pool) =
        admin_login_test_app("admin-login-success-usage.sqlite", "correct-password").await;

    let response = post_login_with_request_id(&app, "correct-password", "req_login_usage_ok").await;
    assert_eq!(response.status(), StatusCode::OK);

    assert_eq!(
        usage_record_count_by_request_id(&pool, "req_login_usage_ok").await,
        0
    );
}

#[tokio::test]
async fn admin_login_should_not_write_usage_record_on_invalid_credentials() {
    let (app, _dir, pool) =
        admin_login_test_app("admin-login-invalid-usage.sqlite", "correct-password").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header("content-type", "application/json")
                .header("x-request-id", "req_login_usage_bad")
                .body(Body::from(
                    r#"{"username":"admin","password":"wrong-password"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(
        usage_record_count_by_request_id(&pool, "req_login_usage_bad").await,
        0
    );
}

#[tokio::test]
async fn admin_login_should_not_write_usage_record_when_throttled() {
    let (app, _dir, pool) =
        admin_login_test_app("admin-login-throttled-usage.sqlite", "correct-password").await;

    for index in 0..5 {
        let response =
            post_login_with_request_id(&app, "wrong-password", &format!("req_login_usage_{index}"))
                .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let response =
        post_login_with_request_id(&app, "correct-password", "req_login_usage_throttled").await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    assert_eq!(
        usage_record_count_by_request_id(&pool, "req_login_usage_throttled").await,
        0
    );
}

async fn admin_login_test_app(
    db_name: &str,
    password: &str,
) -> (axum::Router, tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_user(&pool, password).await;
    let config = test_config(url);
    let stores = codex_proxy_rs::runtime::services::BackgroundTaskStores {
        accounts: codex_proxy_rs::upstream::accounts::store::SqliteAccountStore::new(pool.clone()),
        admin_sessions: codex_proxy_rs::admin::auth::service::SqliteAdminSessionStore::new(
            pool.clone(),
        ),
        cookies: codex_proxy_rs::upstream::accounts::cookies::SqliteCookieStore::new(pool.clone()),
        fingerprints: codex_proxy_rs::upstream::fingerprint::FingerprintRepository::new(
            pool.clone(),
        ),
        session_affinity:
            codex_proxy_rs::proxy::dispatch::session_affinity::SqliteSessionAffinityStore::new(
                pool.clone(),
            ),
        refresh_leases: codex_proxy_rs::upstream::accounts::token_refresh::RefreshLeaseStore::new(
            pool.clone(),
        ),
        client_keys: codex_proxy_rs::admin::keys::service::SqliteClientKeyStore::new(pool.clone()),
        usage_records:
            codex_proxy_rs::admin::monitoring::usage_record_store::SqliteUsageRecordStore::new(
                pool.clone(),
            ),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(codex_proxy_rs::runtime::services::Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let state = codex_proxy_rs::runtime::state::AppState {
        services: (*services).clone(),
    };
    (
        codex_proxy_rs::http::router::router().with_state(state),
        dir,
        pool,
    )
}

async fn post_login(app: &axum::Router, password: &str) -> axum::response::Response {
    post_login_with_request_id(app, password, "req_login_throttle").await
}

async fn post_login_with_request_id(
    app: &axum::Router,
    password: &str,
    request_id: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header("content-type", "application/json")
                .header("x-request-id", request_id)
                .body(Body::from(format!(r#"{{"password":"{password}"}}"#)))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn usage_record_count_by_request_id(pool: &SqlitePool, request_id: &str) -> i64 {
    sqlx::query_scalar("select count(*) from usage_records where request_id = ?")
        .bind(request_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn seed_admin_user(pool: &SqlitePool, password: &str) {
    let now = Utc::now().to_rfc3339();
    let hash = hash_admin_password(password).unwrap();
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at) values (?, ?, ?, ?)",
    )
    .bind("admin_1")
    .bind(hash)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .unwrap();
}
