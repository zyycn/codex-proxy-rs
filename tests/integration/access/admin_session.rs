use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use chrono::Utc;
use codex_proxy_rs::infra::{
    crypto::SecretBox,
    database::connect_sqlite,
    identity::{hash_admin_password, ApiKeyHasher},
};
use sqlx::SqlitePool;
use tower::util::ServiceExt;

use crate::support::config::test_config;

#[tokio::test]
async fn admin_login_should_issue_http_only_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-login.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_user(&pool, "correct-password").await;
    let config = test_config(url);
    let secret_box = SecretBox::new([121u8; 32]);
    let hasher = ApiKeyHasher::new([122u8; 32]);
    let stores = codex_proxy_rs::app::services::BackgroundTaskStores {
        accounts: codex_proxy_rs::accounts::store::SqliteAccountStore::new(
            pool.clone(),
            secret_box.clone(),
        ),
        admin_sessions: codex_proxy_rs::access::admin_session::SqliteAdminSessionStore::new(
            pool.clone(),
        ),
        cookies: codex_proxy_rs::accounts::cookies::SqliteCookieStore::new(
            pool.clone(),
            secret_box.clone(),
        ),
        fingerprints: codex_proxy_rs::codex::fingerprint::FingerprintRepository::new(pool.clone()),
        session_affinity:
            codex_proxy_rs::gateway::dispatch::session_affinity::SqliteSessionAffinityStore::new(
                pool.clone(),
            ),
        refresh_leases: codex_proxy_rs::accounts::token_refresh::RefreshLeaseStore::new(
            pool.clone(),
        ),
        client_keys: codex_proxy_rs::access::client_keys::SqliteClientKeyStore::new(
            pool.clone(),
            hasher,
        ),
        event_logs: codex_proxy_rs::telemetry::event_store::SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = codex_proxy_rs::codex::fingerprint::Fingerprint::default_for_tests();
    let services = std::sync::Arc::new(codex_proxy_rs::app::services::Services::new(
        &config,
        stores,
        fingerprint,
    ));
    let state = codex_proxy_rs::app::state::AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header("content-type", "application/json")
                .header("x-request-id", "req_login")
                .body(Body::from(
                    r#"{"username":"admin","password":"correct-password"}"#,
                ))
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
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
    assert_eq!(body["code"], 200);
    assert_eq!(body["requestId"], "req_login");
    assert!(body["data"]["expiresAt"].is_string());
}

#[tokio::test]
async fn admin_login_should_reject_client_api_key_as_password_or_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-login-rejects-client-key.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_user(&pool, "correct-password").await;
    let config = test_config(url);
    let secret_box = SecretBox::new([123u8; 32]);
    let hasher = ApiKeyHasher::new([124u8; 32]);
    let stores = codex_proxy_rs::app::services::BackgroundTaskStores {
        accounts: codex_proxy_rs::accounts::store::SqliteAccountStore::new(
            pool.clone(),
            secret_box.clone(),
        ),
        admin_sessions: codex_proxy_rs::access::admin_session::SqliteAdminSessionStore::new(
            pool.clone(),
        ),
        cookies: codex_proxy_rs::accounts::cookies::SqliteCookieStore::new(
            pool.clone(),
            secret_box.clone(),
        ),
        fingerprints: codex_proxy_rs::codex::fingerprint::FingerprintRepository::new(pool.clone()),
        session_affinity:
            codex_proxy_rs::gateway::dispatch::session_affinity::SqliteSessionAffinityStore::new(
                pool.clone(),
            ),
        refresh_leases: codex_proxy_rs::accounts::token_refresh::RefreshLeaseStore::new(
            pool.clone(),
        ),
        client_keys: codex_proxy_rs::access::client_keys::SqliteClientKeyStore::new(
            pool.clone(),
            hasher,
        ),
        event_logs: codex_proxy_rs::telemetry::event_store::SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = codex_proxy_rs::codex::fingerprint::Fingerprint::default_for_tests();
    let services = std::sync::Arc::new(codex_proxy_rs::app::services::Services::new(
        &config,
        stores,
        fingerprint,
    ));
    let state = codex_proxy_rs::app::state::AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header("content-type", "application/json")
                .header("authorization", "Bearer cpr_not_an_admin_session")
                .header("x-request-id", "req_login_bad")
                .body(Body::from(
                    r#"{"username":"admin","password":"cpr_not_an_admin_password"}"#,
                ))
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

async fn response_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
