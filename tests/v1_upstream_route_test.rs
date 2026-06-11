use std::collections::BTreeMap;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use chrono::Utc;
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::{
    matchers::{body_json, header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::{
    app::build_router,
    auth::{api_key::ApiKeyHasher, api_key_repository::ClientApiKeyRepository},
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    cookies::repository::CookieRepository,
    crypto::SecretBox,
    state::AppState,
    storage::db::connect_sqlite,
};

struct ImportedApp {
    app: axum::Router,
    pool: sqlx::SqlitePool,
    secret_box: SecretBox,
    client_api_key: String,
    _tempdir: tempfile::TempDir,
}

fn test_config(database_url: String, base_url: String) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        api: ApiConfig { base_url },
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
        admin: AdminConfig {
            session_ttl_minutes: 1440,
        },
        logging: LoggingConfig {
            directory: "logs".to_string(),
            max_file_bytes: 10_485_760,
            retention_days: 14,
            enabled: false,
            capacity: 2_000,
            capture_body: false,
        },
    }
}

async fn build_imported_app(base_url: String) -> ImportedApp {
    let tempdir = tempfile::tempdir().unwrap();
    let db = tempdir.path().join("v1-upstream.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([21u8; 32]);
    let hasher = ApiKeyHasher::new([22u8; 32]);
    let generated = hasher.generate_client_api_key("test");
    ClientApiKeyRepository::new(pool.clone())
        .insert_generated("test", &generated)
        .await
        .unwrap();
    let app = build_router(AppState::with_pool_secret_and_api_key_hasher(
        test_config(url, base_url),
        pool.clone(),
        secret_box.clone(),
        hasher,
    ));

    let import_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_imported",
                            "accountId": "chatgpt-account",
                            "token": "access-secret",
                            "refreshToken": "refresh-secret",
                            "status": "active"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(import_response.status(), StatusCode::OK);

    ImportedApp {
        app,
        pool,
        secret_box,
        client_api_key: generated.plaintext,
        _tempdir: tempdir,
    }
}

#[tokio::test]
async fn v1_responses_should_use_imported_account_and_record_usage() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"usage\":{\"input_tokens\":7,\"output_tokens\":4,\"input_tokens_details\":{\"cached_tokens\":2}}}}\n",
        "\n",
    );
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .and(body_json(json!({
            "model": "gpt-5.5",
            "instructions": "",
            "input": [],
            "stream": true,
            "store": false
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header(
                    "set-cookie",
                    "cf_clearance=new; Domain=.chatgpt.com; Path=/",
                )
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .header("x-request-id", "req_non_stream")
                .body(Body::from(r#"{"model":"gpt-5.5","input":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_1");
    assert_eq!(body["usage"]["input_tokens"], 7);
    let usage: (i64, i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens, cached_tokens from account_usage where account_id = ?",
    )
    .bind("acct_imported")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    assert_eq!(usage, (1, 7, 4, 2));
    let cookie_header = CookieRepository::new(imported.pool.clone(), imported.secret_box)
        .cookie_header("acct_imported", "chatgpt.com")
        .await
        .unwrap();
    assert_eq!(cookie_header.as_deref(), Some("cf_clearance=new"));
    let event = fetch_v1_event_log(&imported.pool, "req_non_stream").await;
    assert_eq!(event.0, "acct_imported");
    assert_eq!(event.1, "/v1/responses");
    assert_eq!(event.2, "gpt-5.5");
    assert_eq!(event.3, 200);
    assert_eq!(event.4["stream"], false);
    assert_eq!(event.4["usage"]["inputTokens"], 7);
}

#[tokio::test]
async fn v1_responses_should_passthrough_stream_and_record_usage_and_log() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"delta\":\"pong\"}\n",
        "\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_stream\",\"object\":\"response\",\"usage\":{\"input_tokens\":3,\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":1}}}}\n",
        "\n",
    );
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(body_json(json!({
            "model": "gpt-5.5",
            "instructions": "",
            "input": [],
            "stream": true,
            "store": false
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .header("x-request-id", "req_stream")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));
    let body = response_text(response).await;
    assert!(body.contains("event: response.output_text.delta"));
    assert!(body.contains("event: response.completed"));
    let usage: (i64, i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens, cached_tokens from account_usage where account_id = ?",
    )
    .bind("acct_imported")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    assert_eq!(usage, (1, 3, 5, 1));
    let event = fetch_v1_event_log(&imported.pool, "req_stream").await;
    assert_eq!(event.0, "acct_imported");
    assert_eq!(event.1, "/v1/responses");
    assert_eq!(event.2, "gpt-5.5");
    assert_eq!(event.3, 200);
    assert_eq!(event.4["stream"], true);
    assert_eq!(event.4["usage"]["outputTokens"], 5);
}

async fn seed_admin_session(pool: &sqlx::SqlitePool, session_id: &str) {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at) values (?, ?, ?, ?)",
    )
    .bind("admin_1")
    .bind("hash")
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into admin_sessions (id, user_id, expires_at, created_at) values (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind("admin_1")
    .bind("2999-01-01T00:00:00Z")
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn response_text(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn fetch_v1_event_log(
    pool: &sqlx::SqlitePool,
    request_id: &str,
) -> (String, String, String, i64, Value) {
    let row: (String, String, String, i64, String) = sqlx::query_as(
        "select account_id, route, model, status_code, metadata_json from event_logs where request_id = ? and kind = 'v1.response'",
    )
    .bind(request_id)
    .fetch_one(pool)
    .await
    .unwrap();
    (
        row.0,
        row.1,
        row.2,
        row.3,
        serde_json::from_str(&row.4).unwrap(),
    )
}
