use std::collections::BTreeMap;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tower::ServiceExt;

use codex_proxy_rs::{
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    platform::crypto::SecretBox,
    platform::identity::client_key::ApiKeyHasher,
    platform::storage::db::connect_sqlite,
    runtime::build_router,
    runtime::state::AppState,
};

use crate::support::{response_json, seed_admin_session};

fn test_config(database_url: String) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        api: ApiConfig {
            base_url: "https://chatgpt.com/backend-api".to_string(),
        },
        model: ModelConfig {
            default_model: "gpt-5.5".to_string(),
            default_reasoning_effort: None,
            service_tier: None,
            aliases: BTreeMap::new(),
        },
        auth: AuthConfig {
            refresh_margin_seconds: 300,
            refresh_enabled: true,
            refresh_concurrency: 2,
            max_concurrent_per_account: 3,
            request_interval_ms: 50,
            rotation_strategy: "least_used".to_string(),
            tier_priority: Vec::new(),
            oauth_client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
            oauth_auth_endpoint: "https://auth.openai.com/oauth/authorize".to_string(),
            oauth_token_endpoint: "https://auth.openai.com/oauth/token".to_string(),
        },
        quota: QuotaConfig {
            refresh_interval_minutes: 5,
            warning_thresholds: QuotaWarningThresholds {
                primary: vec![80, 90],
                secondary: vec![80, 90],
            },
            skip_exhausted: true,
        },
        usage_stats: UsageStatsConfig {
            history_retention_days: None,
        },
        database: DatabaseConfig { url: database_url },
        security: SecurityConfig {
            master_key_file: "data/master.key".to_string(),
            api_key_pepper_file: "data/api-key-pepper.key".to_string(),
        },
        tls: TlsConfig {
            force_http11: false,
        },
        ws_pool: Default::default(),
        admin: AdminConfig {
            session_ttl_minutes: 1440,
            default_username: "admin".to_string(),
            default_password: "admin".to_string(),
            session_cleanup_interval_secs: 3600,
        },
        logging: LoggingConfig {
            directory: "logs".to_string(),
            retention_days: 14,
            enabled: false,
            capacity: 2_000,
            capture_body: false,
        },
    }
}

#[tokio::test]
async fn admin_api_keys_should_create_list_and_authorize_v1_requests() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-api-keys.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([41u8; 32]),
        ApiKeyHasher::new([42u8; 32]),
    ));

    let rejected = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", "Bearer cpr_not_stored")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/api-keys")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_api_key")
                .body(Body::from(r#"{"name":"cursor"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_response.status(), StatusCode::OK);
    let body = response_json(create_response).await;
    let plaintext = body["data"]["plaintext"].as_str().unwrap().to_string();
    assert!(plaintext.starts_with("cpr_"));
    assert_eq!(body["requestId"], "req_api_key");

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/api-keys?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let body = response_json(list_response).await;
    assert_eq!(body["data"][0]["name"], "cursor");
    assert!(body["data"][0].get("plaintext").is_none());
    assert!(body["data"][0].get("keyHash").is_none());

    let models_response = app
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
    assert_eq!(models_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_api_key_status_should_disable_and_reenable_client_key_authorization() {
    let (app, _dir) = admin_api_keys_test_app("admin-api-key-status.sqlite").await;
    let (key_id, plaintext) = create_admin_api_key(&app, "session_1", "status-key").await;

    let disabled = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/api-keys/{key_id}/status"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"status":"disabled"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);
    let body = response_json(disabled).await;
    assert_eq!(body["data"]["enabled"], false);

    let rejected = app
        .clone()
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
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);

    let enabled = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/api-keys/{key_id}/status"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"status":"active"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(enabled.status(), StatusCode::OK);
    let body = response_json(enabled).await;
    assert_eq!(body["data"]["enabled"], true);

    let accepted = app
        .clone()
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
    assert_eq!(accepted.status(), StatusCode::OK);

    let invalid = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/api-keys/{key_id}/status"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"status":"expired"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_api_key_delete_should_remove_client_key_authorization() {
    let (app, _dir) = admin_api_keys_test_app("admin-api-key-delete.sqlite").await;
    let (key_id, plaintext) = create_admin_api_key(&app, "session_1", "delete-key").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/admin/api-keys/{key_id}"))
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["deleted"], true);

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/api-keys?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let body = response_json(list_response).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 0);

    let rejected = app
        .clone()
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
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);

    let missing = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/admin/api-keys/{key_id}"))
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_api_key_label_should_update_and_clear_local_client_key_name() {
    let (app, _dir) = admin_api_keys_test_app("admin-api-key-label.sqlite").await;
    let (key_id, _plaintext) = create_admin_api_key(&app, "session_1", "label-key").await;

    let renamed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/api-keys/{key_id}/label"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":"automation"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(renamed.status(), StatusCode::OK);
    let body = response_json(renamed).await;
    assert_eq!(body["data"]["name"], "label-key");
    assert_eq!(body["data"]["label"], "automation");

    let cleared = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/api-keys/{key_id}/label"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":null}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::OK);
    let body = response_json(cleared).await;
    assert_eq!(body["data"]["name"], "label-key");
    assert!(body["data"]["label"].is_null());

    let too_long = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/api-keys/{key_id}/label"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({ "label": "x".repeat(65) }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(too_long.status(), StatusCode::BAD_REQUEST);

    let missing = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/api-keys/missing/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":"automation"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_api_keys_batch_delete_should_remove_found_keys_and_report_missing_ids() {
    let (app, _dir) = admin_api_keys_test_app("admin-api-key-batch-delete.sqlite").await;
    let (key_a, plaintext_a) = create_admin_api_key(&app, "session_1", "batch-a").await;
    let (key_b, plaintext_b) = create_admin_api_key(&app, "session_1", "batch-b").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/api-keys/batch-delete")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": [key_a, "ghost", key_b]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["deleted"], 2);
    assert_eq!(body["data"]["notFound"], json!(["ghost"]));

    for plaintext in [plaintext_a, plaintext_b] {
        let rejected = app
            .clone()
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
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    }

    let empty = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/api-keys/batch-delete")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"ids":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
}

async fn admin_api_keys_test_app(db_name: &str) -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([43u8; 32]),
        ApiKeyHasher::new([44u8; 32]),
    ));
    (app, dir)
}

async fn create_admin_api_key(
    app: &axum::Router,
    session_id: &str,
    name: &str,
) -> (String, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/api-keys")
                .header("content-type", "application/json")
                .header("cookie", format!("cpr_admin_session={session_id}"))
                .body(Body::from(json!({ "name": name }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    (
        body["data"]["id"].as_str().unwrap().to_string(),
        body["data"]["plaintext"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn admin_api_keys_export_should_return_metadata_without_secret_material() {
    let (app, _dir) = admin_api_keys_test_app("admin-api-key-export.sqlite").await;
    let (key_id, plaintext) = create_admin_api_key(&app, "session_1", "export-key").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/admin/api-keys/export?ids={key_id}"))
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_api_key_export")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["requestId"], "req_api_key_export");
    assert_eq!(body["data"]["sourceFormat"], "rustLocalClientApiKeys");
    assert_eq!(body["data"]["rotationRequired"], true);
    assert_eq!(body["data"]["apiKeys"][0]["id"], key_id);
    assert_eq!(body["data"]["apiKeys"][0]["name"], "export-key");
    assert!(body["data"]["apiKeys"][0]["prefix"]
        .as_str()
        .unwrap()
        .starts_with("cpr_"));
    assert!(body["data"]["apiKeys"][0].get("plaintext").is_none());
    assert!(body["data"]["apiKeys"][0].get("keyHash").is_none());
    assert!(body["data"].get("pepper").is_none());
    assert!(!body.to_string().contains(&plaintext));
}

#[tokio::test]
async fn admin_api_keys_import_should_rotate_exported_metadata_and_return_new_plaintext_once() {
    let source_dir = tempfile::tempdir().unwrap();
    let source_db = source_dir.path().join("admin-api-key-export-source.sqlite");
    let source_url = format!("sqlite://{}", source_db.display());
    let source_pool = connect_sqlite(&source_url).await.unwrap();
    seed_admin_session(&source_pool, "session_1").await;
    let source_app = build_router(AppState::with_pool_secret_and_api_key_hasher(
        test_config(source_url),
        source_pool,
        SecretBox::new([51u8; 32]),
        ApiKeyHasher::new([52u8; 32]),
    ));
    let (source_key_id, source_plaintext) =
        create_admin_api_key(&source_app, "session_1", "rotated-key").await;

    let export_response = source_app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/api-keys/export")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(export_response.status(), StatusCode::OK);
    let export_body = response_json(export_response).await;

    let target_dir = tempfile::tempdir().unwrap();
    let target_db = target_dir.path().join("admin-api-key-import-target.sqlite");
    let target_url = format!("sqlite://{}", target_db.display());
    let target_pool = connect_sqlite(&target_url).await.unwrap();
    seed_admin_session(&target_pool, "session_1").await;
    let target_app = build_router(AppState::with_pool_secret_and_api_key_hasher(
        test_config(target_url),
        target_pool,
        SecretBox::new([61u8; 32]),
        ApiKeyHasher::new([62u8; 32]),
    ));

    let import_response = target_app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/api-keys/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_api_key_import")
                .body(Body::from(export_body["data"].to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(import_response.status(), StatusCode::OK);
    let import_body = response_json(import_response).await;
    assert_eq!(import_body["requestId"], "req_api_key_import");
    assert_eq!(import_body["data"]["imported"], 1);
    assert_eq!(import_body["data"]["skipped"], 0);
    assert_eq!(import_body["data"]["rotated"], true);
    assert_eq!(import_body["data"]["keys"][0]["sourceId"], source_key_id);
    assert_eq!(import_body["data"]["keys"][0]["name"], "rotated-key");
    assert!(import_body["data"]["keys"][0].get("keyHash").is_none());
    let rotated_plaintext = import_body["data"]["keys"][0]["plaintext"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(rotated_plaintext.starts_with("cpr_"));
    assert_ne!(rotated_plaintext, source_plaintext);

    let list_response = target_app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/api-keys?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = response_json(list_response).await;
    assert!(list_body["data"][0].get("plaintext").is_none());

    let old_rejected = target_app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", format!("Bearer {source_plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(old_rejected.status(), StatusCode::UNAUTHORIZED);

    let new_accepted = target_app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", format!("Bearer {rotated_plaintext}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(new_accepted.status(), StatusCode::OK);
}
