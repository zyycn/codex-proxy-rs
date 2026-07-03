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
    admin::auth::service::SqliteAdminSessionStore,
    admin::keys::service::SqliteClientKeyStore,
    admin::monitoring::usage_record_store::SqliteUsageRecordStore,
    config::{schema::AppConfig, settings::RuntimeSettingsService},
    infra::database::connect_sqlite,
    proxy::dispatch::session_affinity::SqliteSessionAffinityStore,
    runtime::services::{BackgroundTaskStores, Services},
    runtime::state::AppState,
    upstream::accounts::{
        cookies::SqliteCookieStore, pool::AccountAcquireRequest, store::SqliteAccountStore,
        token_refresh::RefreshLeaseStore,
    },
    upstream::fingerprint::FingerprintRepository,
};
use serde_json::json;
use sqlx::SqlitePool;
use tower::util::ServiceExt;

use crate::support::{
    admin::seed_admin_session, config::test_config as base_test_config, http::response_json,
};

#[tokio::test]
async fn admin_settings_should_require_admin_session_cookie() {
    let (app, _dir) = admin_settings_test_app("admin-settings-auth.sqlite").await;
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
    let (app, _dir) = admin_settings_test_app("admin-settings-get.sqlite").await;
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
    assert_eq!(body["data"]["modelAccountRoutes"], json!({}));
    assert_eq!(body["data"]["refreshMarginSeconds"], 240);
    assert_eq!(body["data"]["refreshConcurrency"], 2);
    assert_eq!(body["data"]["rotationStrategy"], "least_used");
}

#[tokio::test]
async fn admin_settings_admin_api_key_status_should_be_empty_by_default() {
    let (app, _dir) = admin_settings_test_app("admin-settings-admin-key-empty.sqlite").await;
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
async fn admin_settings_admin_api_key_should_regenerate_and_return_masked_status() {
    let (app, _dir) = admin_settings_test_app("admin-settings-admin-key-regenerate.sqlite").await;
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
    assert_eq!(
        body["data"]["maskedKey"],
        format!("{}...{}", &key[..10], &key[key.len() - 4..])
    );
}

#[tokio::test]
async fn admin_settings_should_accept_valid_admin_api_key_header() {
    let (app, _dir) = admin_settings_test_app("admin-settings-admin-key-auth.sqlite").await;
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
    let (app, _dir) = admin_settings_test_app("admin-settings-admin-key-invalid.sqlite").await;
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
    let (app, _dir) = admin_settings_test_app("admin-settings-admin-key-delete.sqlite").await;
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
    let (app, _dir) = admin_settings_test_app("admin-settings-update-auth.sqlite").await;
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
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-settings-update.sqlite");
    let url = format!("sqlite://{}", db.display());
    let config = test_config(url);
    let pool = connect_sqlite(&config.database.url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    seed_account(&pool, "acct_route_a").await;
    seed_account(&pool, "acct_route_b").await;
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        usage_records: SqliteUsageRecordStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = Services::new(&config, stores, fingerprint);
    let services = std::sync::Arc::new(services);
    let state = AppState {
        config: config.clone(),
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);

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
                        "modelAccountRoutes": {
                            "gpt-5.5": ["acct_route_a", "acct_route_b"]
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
    assert_eq!(
        body["data"]["modelAccountRoutes"]["gpt-5.5"][0],
        "acct_route_a"
    );
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

    let row: (String, i64, i64, i64, i64, String) = sqlx::query_as(
        "select model_aliases_json, refresh_margin_seconds, refresh_concurrency, max_concurrent_per_account, request_interval_ms, rotation_strategy from runtime_settings where id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let aliases: BTreeMap<String, String> = serde_json::from_str(&row.0).unwrap();
    assert_eq!(aliases["gpt-5.2"], "gpt-5.5");
    assert_eq!(aliases["claude-sonnet"], "gpt-5.5");
    assert_eq!(row.1, 900);
    assert_eq!(row.2, 4);
    assert_eq!(row.3, 7);
    assert_eq!(row.4, 80);
    assert_eq!(row.5, "round_robin");
    let route_rows: Vec<(String, String, i64)> = sqlx::query_as(
        "select model, account_id, priority from model_account_routes order by model, priority",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        route_rows,
        vec![
            ("gpt-5.5".to_string(), "acct_route_a".to_string(), 0),
            ("gpt-5.5".to_string(), "acct_route_b".to_string(), 1),
        ]
    );
    assert!(!dir.path().join("config.yaml").exists());

    let restarted_config = RuntimeSettingsService::load_or_initialize_config(config.clone(), &pool)
        .await
        .unwrap();
    assert_eq!(restarted_config.model_aliases["gpt-5.2"], "gpt-5.5");
    assert_eq!(
        restarted_config.model_account_routes["gpt-5.5"],
        vec!["acct_route_a".to_string(), "acct_route_b".to_string()]
    );
    assert_eq!(restarted_config.auth.refresh_margin_seconds, 900);
    assert_eq!(restarted_config.auth.refresh_concurrency, 4);
    assert_eq!(restarted_config.auth.rotation_strategy, "round_robin");
    assert_eq!(restarted_config.auth.max_concurrent_per_account, 7);
    assert_eq!(restarted_config.auth.request_interval_ms, 80);
    assert_eq!(restarted_config.database.url, config.database.url);
}

#[tokio::test]
async fn admin_settings_update_should_apply_runtime_services() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-settings-runtime.sqlite");
    let url = format!("sqlite://{}", db.display());
    let config = test_config(url);
    let pool = connect_sqlite(&config.database.url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    seed_schedulable_account(&pool, "acct_runtime_a").await;
    seed_schedulable_account(&pool, "acct_runtime_b").await;
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        usage_records: SqliteUsageRecordStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(Services::new(&config, stores, fingerprint));
    services
        .account_pool
        .restore_from_repository()
        .await
        .unwrap();
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);

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
                        "modelAccountRoutes": {
                            "gpt-5.5": ["acct_runtime_b"],
                            "gpt-5.6": ["acct_runtime_a", "acct_runtime_b"]
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

    let parsed = services
        .models
        .catalog()
        .await
        .parse_model_name("runtime-alias");
    assert_eq!(parsed.model_id, "gpt-5.5");

    let refresh_policy = services.refresh_policy.snapshot();
    assert_eq!(refresh_policy.refresh_margin_seconds, 120);
    assert_eq!(refresh_policy.refresh_concurrency, 3);

    let capacity = services.account_pool.capacity_summary(Utc::now()).await;
    assert_eq!(capacity.total_slots, 4);
    assert_eq!(capacity.max_concurrent_per_account, 2);

    let route_request = AccountAcquireRequest::new("gpt-5.5", Utc::now());
    let routed = services
        .account_pool
        .acquire_with(&route_request)
        .await
        .unwrap();
    assert_eq!(routed.account.id, "acct_runtime_b");
    services.account_pool.release(&routed.account.id).await;

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
    assert_eq!(first.account.id, "acct_runtime_b");
    assert_eq!(second.account.id, "acct_runtime_a");

    let repeated = services
        .account_pool
        .acquire_with(&AccountAcquireRequest::new("gpt-5.5", Utc::now()))
        .await
        .unwrap();
    assert_eq!(repeated.account.id, "acct_runtime_b");
    let started_at = Instant::now();
    services
        .account_pool
        .wait_for_request_interval(&repeated)
        .await;
    assert!(started_at.elapsed() >= StdDuration::from_millis(20));
}

#[tokio::test]
async fn admin_settings_update_should_reject_unsupported_or_invalid_fields() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-settings-update-invalid.sqlite");
    let url = format!("sqlite://{}", db.display());
    let config = test_config(url);
    let pool = connect_sqlite(&config.database.url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        usage_records: SqliteUsageRecordStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);

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

async fn admin_settings_test_app(db_name: &str) -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let config = test_config(url);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        usage_records: SqliteUsageRecordStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = std::sync::Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    (
        codex_proxy_rs::http::router::router().with_state(state),
        dir,
    )
}

async fn seed_account(pool: &SqlitePool, account_id: &str) {
    sqlx::query(
        r"
insert into accounts (
  id,
  access_token,
  status,
  added_at,
  updated_at
) values (?, ?, 'active', ?, ?)",
    )
    .bind(account_id)
    .bind("access-token")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_schedulable_account(pool: &SqlitePool, account_id: &str) {
    sqlx::query(
        r"
insert into accounts (
  id,
  access_token,
  access_token_expires_at,
  status,
  added_at,
  updated_at
) values (?, ?, ?, 'active', ?, ?)",
    )
    .bind(account_id)
    .bind("access-token")
    .bind("2099-01-01T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
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
