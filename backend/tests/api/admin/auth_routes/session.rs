use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Utc;
use codex_proxy_rs::infra::identity::hash_admin_password;
use sqlx::PgPool;
use tower::util::ServiceExt;

use crate::support::{
    config::test_config,
    fingerprint::runtime_fingerprint,
    http::response_json,
    storage::{background_task_stores, create_test_redis, init_test_db, test_database_url},
};

#[tokio::test]
async fn admin_login_should_issue_http_only_session_cookie() {
    let (app, _dir, _pool) = admin_login_test_app("admin-login", "correct-password").await;

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
    let cache_control = response
        .headers()
        .get("cache-control")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);

    assert_eq!(status, StatusCode::OK, "login status");
    assert_eq!(cache_control.as_deref(), Some("no-store"));
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
async fn admin_logout_should_revoke_session_and_clear_cookie() {
    let (app, _dir, _pool) =
        admin_login_test_app("admin-logout-revokes-session", "correct-password").await;
    let login = post_login(&app, "correct-password").await;
    let cookie = login
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .unwrap()
        .to_string();

    let logout = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/logout")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::OK);
    assert!(logout
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("Max-Age=0")));

    let status = app
        .oneshot(
            Request::builder()
                .uri("/api/admin/auth/status")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(status).await;
    assert_eq!(body["data"]["authenticated"], false);
}

#[tokio::test]
async fn admin_login_should_reject_client_api_key_as_password_or_authorization() {
    let (app, _dir, _pool) =
        admin_login_test_app("admin-login-rejects-client-key", "correct-password").await;

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
    let (app, _dir, _pool) = admin_login_test_app("admin-login-throttle", "correct-password").await;

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
        admin_login_test_app("admin-login-success-usage", "correct-password").await;

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
        admin_login_test_app("admin-login-invalid-usage", "correct-password").await;

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
        admin_login_test_app("admin-login-throttled-usage", "correct-password").await;

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
) -> (
    axum::Router,
    crate::support::storage::TestDatabaseGuard,
    PgPool,
) {
    let (pool, dir) = init_test_db(db_name).await;
    let redis = create_test_redis(db_name).await;
    seed_admin_user(&pool, password).await;
    let config = test_config(test_database_url());
    let stores = background_task_stores(pool.clone(), redis);
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(codex_proxy_rs::bootstrap::services::Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let state = codex_proxy_rs::api::AppState::from(services.as_ref());
    (
        codex_proxy_rs::api::router::router().with_state(state),
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

async fn usage_record_count_by_request_id(pool: &PgPool, request_id: &str) -> i64 {
    sqlx::query_scalar("select count(*) from usage_records where request_id = $1")
        .bind(request_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn seed_admin_user(pool: &PgPool, password: &str) {
    let now = Utc::now();
    let hash = hash_admin_password(password).unwrap();
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at) values ($1, $2, $3, $3)",
    )
    .bind("admin_1")
    .bind(hash)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
}
