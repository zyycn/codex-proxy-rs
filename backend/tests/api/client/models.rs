use std::collections::BTreeMap;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use codex_proxy_rs::{api::AppState, bootstrap::services::Services};
use tower::util::ServiceExt;

use crate::support::{
    client_keys::insert_client_api_key,
    config::test_config,
    http::response_json,
    storage::{background_task_stores, create_test_redis, init_test_db, test_database_url},
    wire_profile::wire_profile,
};

#[tokio::test]
async fn models_route_should_reject_unknown_client_api_key() {
    let (app, _key, _dir) = test_app("openai-models-auth", false).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", "Bearer sk_not_stored")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn models_route_should_return_uniform_error_for_malformed_authorization() {
    let (app, _key, _dir) = test_app("openai-models-auth-malformed", false).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", "Token sk_not_bearer")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "invalid_api_key"
    );
}

#[tokio::test]
async fn models_route_should_accept_stored_client_api_key() {
    let (app, plaintext, _dir) = test_app("openai-models-auth-valid", true).await;
    let plaintext = plaintext.expect("client api key should be seeded");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", format!("Bearer {plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert!(
        body["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["id"] == "gpt-5.5")
    );
}

#[tokio::test]
async fn model_detail_route_should_accept_configured_alias() {
    let mut aliases = BTreeMap::new();
    aliases.insert("codex-fast".to_string(), "gpt-5.5".to_string());
    let (app, plaintext, _dir) = test_app_with_aliases("openai-models-alias", aliases).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models/codex-fast")
                .header("authorization", format!("Bearer {plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "codex-fast");
}

async fn test_app(
    db_name: &str,
    seed_client_key: bool,
) -> (
    Router,
    Option<String>,
    crate::support::storage::TestDatabaseGuard,
) {
    let (pool, dir) = init_test_db(db_name).await;
    let plaintext = if seed_client_key {
        Some(insert_client_api_key(&pool).await)
    } else {
        None
    };
    let config = test_config(test_database_url());
    build_test_app(pool, config, plaintext, dir, db_name).await
}

async fn test_app_with_aliases(
    db_name: &str,
    model_aliases: BTreeMap<String, String>,
) -> (Router, String, crate::support::storage::TestDatabaseGuard) {
    let (pool, dir) = init_test_db(db_name).await;
    let plaintext = insert_client_api_key(&pool).await;
    let mut config = test_config(test_database_url());
    config.model_aliases = model_aliases;
    let (app, plaintext, dir) = build_test_app(pool, config, Some(plaintext), dir, db_name).await;
    (
        app,
        plaintext.expect("client api key should be seeded"),
        dir,
    )
}

async fn build_test_app(
    pool: sqlx::PgPool,
    config: codex_proxy_rs::bootstrap::config::AppConfig,
    plaintext: Option<String>,
    dir: crate::support::storage::TestDatabaseGuard,
    label: &str,
) -> (
    Router,
    Option<String>,
    crate::support::storage::TestDatabaseGuard,
) {
    let redis = create_test_redis(label).await;
    seed_model_snapshot(&redis).await;
    let stores = background_task_stores(pool.clone(), redis);
    let profile = crate::support::wire_profile::test_wire_profile_value();
    let services = std::sync::Arc::new(Services::new(&config, stores, wire_profile(profile)));
    services
        .initialize_hot_path_state()
        .await
        .expect("hot path state should initialize");
    let state = AppState::from(services.as_ref());
    let app = codex_proxy_rs::api::router::router().with_state(state);

    (app, plaintext, dir)
}

async fn seed_model_snapshot(redis: &codex_proxy_rs::infra::redis::RedisConnection) {
    let mut connection = redis.manager();
    let value = serde_json::json!({
        "models": [{
            "id": "gpt-5.5",
            "displayName": "GPT 5.5",
            "description": "Test model",
            "isDefault": false,
            "supportedReasoningEfforts": [{"reasoningEffort": "medium", "description": "medium"}],
            "defaultReasoningEffort": "medium",
            "inputModalities": ["text"],
            "outputModalities": ["text"],
            "supportsPersonality": false,
            "upgrade": null,
            "source": "test"
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
