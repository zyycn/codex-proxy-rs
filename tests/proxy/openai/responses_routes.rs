use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use codex_proxy_rs::{
    admin::auth::service::SqliteAdminSessionStore,
    admin::keys::service::SqliteClientKeyStore,
    admin::monitoring::event_store::SqliteEventLogStore,
    infra::database::connect_sqlite,
    proxy::dispatch::session_affinity::SqliteSessionAffinityStore,
    runtime::services::{BackgroundTaskStores, Services},
    runtime::state::AppState,
    upstream::accounts::token_refresh::RefreshLeaseStore,
    upstream::accounts::{cookies::SqliteCookieStore, store::SqliteAccountStore},
    upstream::fingerprint::FingerprintRepository,
};
use sqlx::SqlitePool;
use tower::util::ServiceExt;

use crate::support::{
    client_keys::insert_client_api_key, config::test_config, http::response_json,
};

#[tokio::test]
async fn responses_route_should_reject_missing_client_api_key() {
    let (app, _key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .body(Body::from(r#"{"model":"gpt-5.5","input":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn responses_route_should_reject_unknown_models_with_openai_error() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"unknown-model","input":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "model_not_found"
    );
}

#[tokio::test]
async fn responses_route_should_terminate_generated_stream_errors_with_done_marker() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("event: response.failed"));
    assert!(body.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn responses_route_should_default_omitted_stream_to_sse() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-5.5","input":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn chat_completions_route_should_reject_empty_messages_with_bad_request() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"gpt-5.5","messages":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

async fn test_app_with_client_api_key() -> (axum::Router, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("openai-responses-routes.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let plaintext = insert_client_api_key(&pool).await;
    insert_model_snapshot(&pool).await;
    let config = test_config(url);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        event_logs: SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    (
        codex_proxy_rs::http::router::router().with_state(state),
        plaintext,
        dir,
    )
}

async fn insert_model_snapshot(pool: &SqlitePool) {
    sqlx::query(
        r"insert or replace into model_plan_snapshots (plan_type, models_json, fetched_at) values (?, ?, ?)",
    )
    .bind("plus")
    .bind(
        r#"[{"id":"gpt-5.5","displayName":"GPT 5.5","description":"Test model","isDefault":false,"supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"medium"}],"defaultReasoningEffort":"medium","inputModalities":["text"],"outputModalities":["text"],"supportsPersonality":false,"upgrade":null,"source":"test"}]"#,
    )
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
}
