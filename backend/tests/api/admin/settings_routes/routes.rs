use std::{
    collections::BTreeMap,
    time::{Duration as StdDuration, Instant},
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Utc;
use codex_proxy_rs::{
    api::AppState,
    bootstrap::{
        config::AppConfig,
        services::{apply_settings_to_config, settings_snapshot_from_config, Services},
        tasks::coordinator::TaskCoordinator,
    },
    fleet::pool::AccountAcquireRequest,
    settings::service::SettingsService,
};
use serde_json::json;
use sqlx::PgPool;
use tower::util::ServiceExt;

use crate::support::{
    admin::seed_admin_session,
    config::test_config as base_test_config,
    fingerprint::runtime_fingerprint,
    http::response_json,
    storage::{background_task_stores, create_test_redis, init_test_db, test_database_url},
};

#[tokio::test]
async fn admin_settings_should_require_admin_session_cookie() {
    let (app, _dir) = admin_settings_test_app("admin-settings-auth").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/settings")
                .header("x-request-id", "req_settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response_json(response).await["code"], 40101);
}

#[tokio::test]
async fn admin_settings_should_return_runtime_fields() {
    let (app, _dir) = admin_settings_test_app("admin-settings-get").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/settings")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_get")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    assert_eq!(body["data"]["modelAliases"]["codex-fast"], "gpt-5.5");
    assert_eq!(body["data"]["refreshMarginSeconds"], 240);
    assert_eq!(body["data"]["refreshConcurrency"], 2);
    assert_eq!(body["data"]["rotationStrategy"], "smart");
}

#[tokio::test]
async fn admin_settings_admin_api_key_status_should_be_empty_by_default() {
    let (app, _dir) = admin_settings_test_app("admin-settings-admin-key-empty").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/settings/admin-api-key")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_admin_key_empty")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["exists"], false);
    assert!(body["data"]["maskedKey"].is_null());
}

#[tokio::test]
async fn admin_settings_admin_api_key_should_regenerate_and_return_exists_only() {
    let (app, _dir) = admin_settings_test_app("admin-settings-admin-key-regenerate").await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/settings/admin-api-key/regenerate")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_admin_key_regenerate")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let key = body["data"]["key"].as_str().unwrap();
    assert!(key.starts_with("admin-"));
    assert_eq!(key.len(), 70);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/settings/admin-api-key")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_admin_key_status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["exists"], true);
    assert!(body["data"]["maskedKey"].is_null());
}

#[tokio::test]
async fn admin_settings_should_accept_valid_admin_api_key_header() {
    let (app, _dir) = admin_settings_test_app("admin-settings-admin-key-auth").await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/settings/admin-api-key/regenerate")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_admin_key_auth_create")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let key = body["data"]["key"].as_str().unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/settings")
                .header("x-api-key", key)
                .header("x-request-id", "req_settings_admin_key_auth")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["refreshMarginSeconds"], 240);
}

#[tokio::test]
async fn admin_settings_should_reject_invalid_admin_api_key_header() {
    let (app, _dir) = admin_settings_test_app("admin-settings-admin-key-invalid").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/settings")
                .header("x-api-key", "admin-invalid")
                .header("x-request-id", "req_settings_admin_key_invalid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response_json(response).await["code"], 40103);
}

#[tokio::test]
async fn admin_settings_admin_api_key_should_delete_and_revoke_access() {
    let (app, _dir) = admin_settings_test_app("admin-settings-admin-key-delete").await;
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/settings/admin-api-key/regenerate")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_admin_key_delete_create")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let key = body["data"]["key"].as_str().unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/settings/admin-api-key")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_admin_key_delete")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/settings")
                .header("x-api-key", key)
                .header("x-request-id", "req_settings_admin_key_revoked")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response_json(response).await["code"], 40103);
}

#[tokio::test]
async fn admin_settings_update_should_require_admin_session_cookie() {
    let (app, _dir) = admin_settings_test_app("admin-settings-update-auth").await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/settings")
                .header("content-type", "application/json")
                .header("x-request-id", "req_settings_update_auth")
                .body(Body::from(r#"{"refreshMarginSeconds":300}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_settings_update_should_persist_runtime_settings_to_database() {
    let (pool, dir) = init_test_db("admin-settings-update").await;
    let redis = create_test_redis("admin-settings-update").await;
    let config = test_config(test_database_url());
    seed_admin_session(&pool, &redis, "session_1").await;
    let stores = background_task_stores(pool.clone(), redis);
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = Services::new(&config, stores, runtime_fingerprint(fingerprint));
    let services = std::sync::Arc::new(services);
    let state = AppState::from(services.as_ref());
    let app = codex_proxy_rs::api::router::router().with_state(state);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/settings")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_update")
                .body(Body::from(
                    json!({
                        "rotationStrategy": "round_robin",
                        "modelAliases": {
                            "gpt-5.2": "gpt-5.5",
                            "claude-sonnet": "gpt-5.5"
                        },
                        "refreshMarginSeconds": 900,
                        "refreshConcurrency": 4,
                        "maxConcurrentPerAccount": 7,
                        "requestIntervalMs": 80
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["modelAliases"]["gpt-5.2"], "gpt-5.5");
    assert_eq!(body["data"]["refreshMarginSeconds"], 900);
    assert_eq!(body["data"]["refreshConcurrency"], 4);

    let get_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/settings")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response_json(get_response).await["data"]["rotationStrategy"],
        "round_robin"
    );

    let row: (serde_json::Value, i64, i64, i64, i64, String) = sqlx::query_as(
        "select model_aliases_json, refresh_margin_seconds, refresh_concurrency, max_concurrent_per_account, request_interval_ms, rotation_strategy from runtime_settings where id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let aliases: BTreeMap<String, String> = serde_json::from_value(row.0).unwrap();
    assert_eq!(aliases["gpt-5.2"], "gpt-5.5");
    assert_eq!(aliases["claude-sonnet"], "gpt-5.5");
    assert_eq!(row.1, 900);
    assert_eq!(row.2, 4);
    assert_eq!(row.3, 7);
    assert_eq!(row.4, 80);
    assert_eq!(row.5, "round_robin");
    assert!(!dir.path().join("config.yaml").exists());

    let restarted_settings =
        SettingsService::load_or_initialize(settings_snapshot_from_config(&config), &pool)
            .await
            .unwrap();
    let mut restarted_config = config.clone();
    apply_settings_to_config(&mut restarted_config, &restarted_settings);
    assert_eq!(restarted_config.model_aliases["gpt-5.2"], "gpt-5.5");
    assert_eq!(restarted_config.auth.refresh_margin_seconds, 900);
    assert_eq!(restarted_config.auth.refresh_concurrency, 4);
    assert_eq!(restarted_config.auth.rotation_strategy, "round_robin");
    assert_eq!(restarted_config.auth.max_concurrent_per_account, 7);
    assert_eq!(restarted_config.auth.request_interval_ms, 80);
    assert_eq!(restarted_config.database.url, config.database.url);
}

#[tokio::test]
async fn admin_settings_update_should_apply_runtime_services() {
    let (pool, _dir) = init_test_db("admin-settings-runtime").await;
    let redis = create_test_redis("admin-settings-runtime").await;
    let config = test_config(test_database_url());
    seed_admin_session(&pool, &redis, "session_1").await;
    seed_schedulable_account(&pool, "acct_runtime_a").await;
    seed_schedulable_account(&pool, "acct_runtime_b").await;
    let stores = background_task_stores(pool.clone(), redis);
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let settings_tasks = TaskCoordinator::start_settings_subscriptions(&services);
    services.account_pool.restore_from_store().await.unwrap();
    let state = AppState::from(services.as_ref());
    let app = codex_proxy_rs::api::router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/settings")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_runtime")
                .body(Body::from(
                    json!({
                        "modelAliases": {
                            "runtime-alias": "gpt-5.5"
                        },
                        "refreshMarginSeconds": 120,
                        "refreshConcurrency": 3,
                        "maxConcurrentPerAccount": 2,
                        "requestIntervalMs": 30,
                        "rotationStrategy": "round_robin"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    tokio::time::timeout(StdDuration::from_secs(1), async {
        loop {
            let resolved = services
                .models
                .catalog()
                .await
                .resolve_model_id("runtime-alias");
            let refresh_policy = services.refresh_policy.snapshot();
            let capacity = services.account_pool.capacity_summary(Utc::now()).await;
            if resolved == "gpt-5.5"
                && refresh_policy.refresh_margin_seconds == 120
                && refresh_policy.refresh_concurrency == 3
                && capacity.total_slots == 4
                && capacity.max_concurrent_per_account == 2
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("runtime settings subscribers should observe the persisted update");

    let resolved = services
        .models
        .catalog()
        .await
        .resolve_model_id("runtime-alias");
    assert_eq!(resolved, "gpt-5.5");

    let refresh_policy = services.refresh_policy.snapshot();
    assert_eq!(refresh_policy.refresh_margin_seconds, 120);
    assert_eq!(refresh_policy.refresh_concurrency, 3);

    let capacity = services.account_pool.capacity_summary(Utc::now()).await;
    assert_eq!(capacity.total_slots, 4);
    assert_eq!(capacity.max_concurrent_per_account, 2);

    let rotation_request = AccountAcquireRequest::new("gpt-5.6", Utc::now());
    let first = services
        .account_pool
        .acquire_with(&rotation_request)
        .await
        .unwrap();
    let second = services
        .account_pool
        .acquire_with(&rotation_request)
        .await
        .unwrap();
    assert_ne!(first.account.id, second.account.id);

    let repeated = services
        .account_pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", Utc::now()))
        .await
        .unwrap();
    let started_at = Instant::now();
    services
        .account_pool
        .wait_for_request_interval(&repeated)
        .await;
    assert!(started_at.elapsed() >= StdDuration::from_millis(20));
    settings_tasks.shutdown().await;
}

#[tokio::test]
async fn admin_settings_update_should_reject_unsupported_or_invalid_fields() {
    let (pool, _dir) = init_test_db("admin-settings-update-invalid").await;
    let redis = create_test_redis("admin-settings-update-invalid").await;
    let config = test_config(test_database_url());
    seed_admin_session(&pool, &redis, "session_1").await;
    let stores = background_task_stores(pool.clone(), redis);
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let state = AppState::from(services.as_ref());
    let app = codex_proxy_rs::api::router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/settings")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_settings_update_invalid")
                .body(Body::from(
                    json!({"refreshEnabled": false, "rotationStrategy": "random"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

async fn admin_settings_test_app(
    db_name: &str,
) -> (axum::Router, crate::support::storage::TestDatabaseGuard) {
    let (pool, dir) = init_test_db(db_name).await;
    let redis = create_test_redis(db_name).await;
    seed_admin_session(&pool, &redis, "session_1").await;
    let config = test_config(test_database_url());
    let stores = background_task_stores(pool.clone(), redis);
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let state = AppState::from(services.as_ref());
    (codex_proxy_rs::api::router::router().with_state(state), dir)
}

async fn seed_schedulable_account(pool: &PgPool, account_id: &str) {
    sqlx::query(
        r"
insert into accounts (
  id,
  access_token,
  access_token_expires_at,
  status,
  added_at,
  updated_at
) values ($1, $2, $3, 'active', $4, $5)",
    )
    .bind(account_id)
    .bind("access-token")
    .bind(crate::support::storage::timestamp("2099-01-01T00:00:00Z"))
    .bind(crate::support::storage::timestamp("2026-06-18T00:00:00Z"))
    .bind(crate::support::storage::timestamp("2026-06-18T00:00:00Z"))
    .execute(pool)
    .await
    .unwrap();
}

fn test_config(database_url: String) -> AppConfig {
    let mut config = base_test_config(database_url);
    config
        .model_aliases
        .insert("codex-fast".to_string(), "gpt-5.5".to_string());
    config.auth.refresh_margin_seconds = 240;
    config.auth.max_concurrent_per_account = 4;
    config.auth.tier_priority = vec!["team".to_string(), "plus".to_string()];
    config.logging.enabled = true;
    config
}
