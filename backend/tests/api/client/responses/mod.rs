use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use codex_proxy_rs::{api::AppState, bootstrap::services::Services};
use tower::util::ServiceExt;

use crate::support::{
    client_keys::insert_client_api_key,
    config::test_config,
    fingerprint::runtime_fingerprint,
    http::response_json,
    storage::{background_task_stores, create_test_redis, init_test_db, test_database_url},
};

mod websocket;

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
async fn responses_route_should_accept_body_larger_than_axum_default_limit() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let large_input = "x".repeat(3 * 1024 * 1024);
    let body = serde_json::json!({
        "model": "unknown-model",
        "input": large_input,
    })
    .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn responses_route_should_reject_body_over_proxy_limit() {
    let (app, api_key, _dir) = test_app_with_client_api_key().await;
    let body = serde_json::json!({
        "model": "gpt-5.5",
        "input": "x".repeat(17 * 1024 * 1024),
    })
    .to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
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
    assert!(body.contains(r#""code":"no_available_accounts""#));
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

pub(super) async fn test_app_with_client_api_key() -> (
    axum::Router,
    String,
    crate::support::storage::TestDatabaseGuard,
) {
    let (pool, dir) = init_test_db("openai-responses-routes").await;
    let redis = create_test_redis("openai-responses-routes").await;
    let plaintext = insert_client_api_key(&pool).await;
    insert_model_snapshot(&redis).await;
    let config = test_config(test_database_url());
    let stores = background_task_stores(pool.clone(), redis);
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    services
        .initialize_hot_path_state()
        .await
        .expect("hot path state should initialize");
    let state = AppState::from(services.as_ref());
    (
        codex_proxy_rs::api::router::router().with_state(state),
        plaintext,
        dir,
    )
}

async fn insert_model_snapshot(redis: &codex_proxy_rs::infra::redis::RedisConnection) {
    let mut connection = redis.manager();
    let value = serde_json::json!({
        "models": [{
            "id":"gpt-5.5",
            "displayName":"GPT 5.5",
            "description":"Test model",
            "isDefault":false,
            "supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"medium"}],
            "defaultReasoningEffort":"medium",
            "inputModalities":["text"],
            "outputModalities":["text"],
            "supportsPersonality":false,
            "upgrade":null,
            "source":"test"
        }],
        "fetchedAt": chrono::Utc::now()
    });
    let _: usize = redis::cmd("HSET")
        .arg(redis.key("models:plan_snapshots"))
        .arg("plus")
        .arg(value.to_string())
        .query_async(&mut connection)
        .await
        .unwrap();
}
