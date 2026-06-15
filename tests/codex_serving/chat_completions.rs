use std::collections::BTreeMap;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::{
    codex::gateway::conversation_identity::build_conversation_identity,
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    platform::crypto::SecretBox,
    platform::identity::{client_key::ApiKeyHasher, client_key_repository::ClientApiKeyRepository},
    platform::storage::db::connect_sqlite,
    runtime::build_router,
    runtime::state::AppState,
};

use crate::support::{response_json, response_text, seed_admin_session};

const CHAT_RETRY_AFTER_SUCCESS_SSE: &str = include_str!("../fixtures/chat/retry_after_success.sse");
const CHAT_NON_STREAM_TEXT_SSE: &str = include_str!("../fixtures/chat/non_stream_text.sse");
const CHAT_STREAM_TEXT_SSE: &str = include_str!("../fixtures/chat/stream_text.sse");
const CHAT_PARALLEL_TOOLS_SUCCESS_SSE: &str =
    include_str!("../fixtures/chat/parallel_tools_success.sse");
const CHAT_TOOL_REASONING_COMPLETE_SSE: &str =
    include_str!("../fixtures/chat/tool_reasoning_complete.sse");
const CHAT_TOOL_STREAM_SSE: &str = include_str!("../fixtures/chat/tool_stream.sse");
const CHAT_TUPLE_OBJECT_SSE: &str = include_str!("../fixtures/chat/tuple_object.sse");
const CHAT_NO_AVAILABLE_ACCOUNTS_GOLDEN: &str =
    include_str!("../fixtures/chat/golden/no_available_accounts.json");
const CHAT_RATE_LIMIT_FALLBACK_EXHAUSTED_GOLDEN: &str =
    include_str!("../fixtures/chat/golden/rate_limit_fallback_exhausted.json");
const CHAT_QUOTA_EXHAUSTED_FALLBACK_EXHAUSTED_GOLDEN: &str =
    include_str!("../fixtures/chat/golden/quota_exhausted_fallback_exhausted.json");
const CHAT_RETRY_AFTER_STREAM_GOLDEN: &str =
    include_str!("../fixtures/chat/golden/retry_after_stream.sse");
const CHAT_NON_STREAM_TEXT_GOLDEN: &str =
    include_str!("../fixtures/chat/golden/non_stream_text.json");
const CHAT_STREAM_TEXT_GOLDEN: &str = include_str!("../fixtures/chat/golden/stream_text.sse");
const CHAT_TUPLE_RECONVERT_GOLDEN: &str =
    include_str!("../fixtures/chat/golden/tuple_reconvert.json");
const CHAT_TUPLE_RECONVERT_STREAM_GOLDEN: &str =
    include_str!("../fixtures/chat/golden/tuple_reconvert_stream.sse");
const CHAT_PARALLEL_TOOLS_GOLDEN: &str =
    include_str!("../fixtures/chat/golden/parallel_tools.json");
const CHAT_TOOL_REASONING_GOLDEN: &str =
    include_str!("../fixtures/chat/golden/tool_reasoning.json");
const CHAT_TOOL_STREAM_GOLDEN: &str = include_str!("../fixtures/chat/golden/tool_stream.sse");

struct ImportedApp {
    app: axum::Router,
    client_api_key: String,
    _tempdir: tempfile::TempDir,
}

struct ImportAccount {
    id: &'static str,
    account_id: &'static str,
    token: &'static str,
    refresh_token: &'static str,
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

async fn build_imported_app(base_url: String) -> ImportedApp {
    build_imported_app_with_accounts(
        base_url,
        &[ImportAccount {
            id: "acct_chat",
            account_id: "chatgpt-account",
            token: "access-secret",
            refresh_token: "refresh-secret",
        }],
    )
    .await
}

async fn build_imported_app_with_accounts(
    base_url: String,
    accounts: &[ImportAccount],
) -> ImportedApp {
    let tempdir = tempfile::tempdir().unwrap();
    let db = tempdir.path().join("chat-completions.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([61u8; 32]);
    let hasher = ApiKeyHasher::new([62u8; 32]);
    let generated = hasher.generate_client_api_key("test");
    ClientApiKeyRepository::new(pool.clone())
        .insert_generated("test", &generated)
        .await
        .unwrap();
    let app = build_router(AppState::with_pool_secret_and_api_key_hasher(
        test_config(url, base_url),
        pool,
        secret_box,
        hasher,
    ));
    if !accounts.is_empty() {
        let accounts = accounts
            .iter()
            .map(|account| {
                json!({
                    "id": account.id,
                    "accountId": account.account_id,
                    "token": account.token,
                    "refreshToken": account.refresh_token,
                    "accessTokenExpiresAt": "2999-01-01T00:00:00Z",
                    "status": "active"
                })
            })
            .collect::<Vec<_>>();

        let import_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/accounts/import")
                    .header("content-type", "application/json")
                    .header("cookie", "cpr_admin_session=session_1")
                    .body(Body::from(json!({ "accounts": accounts }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(import_response.status(), StatusCode::OK);
    }

    ImportedApp {
        app,
        client_api_key: generated.plaintext,
        _tempdir: tempdir,
    }
}

async fn single_upstream_post_body(server: &MockServer) -> Value {
    let requests = server.received_requests().await.unwrap();
    let post_requests = requests
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .collect::<Vec<_>>();
    assert_eq!(post_requests.len(), 1);
    assert_eq!(requests.len(), 1);
    serde_json::from_slice(&post_requests[0].body).unwrap()
}

fn normalize_chat_completion(mut value: Value) -> Value {
    value["id"] = json!("chatcmpl-REDACTED");
    value["created"] = json!(0);
    value
}

fn normalize_chat_stream(body: &str) -> String {
    let mut normalized = String::new();
    for line in body.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            normalized.push_str(line);
            normalized.push('\n');
            continue;
        };
        if data == "[DONE]" {
            normalized.push_str("data: [DONE]\n");
            continue;
        }
        let mut value: Value = serde_json::from_str(data).unwrap();
        value["id"] = json!("chatcmpl-REDACTED");
        value["created"] = json!(0);
        normalized.push_str("data: ");
        normalized.push_str(&value.to_string());
        normalized.push('\n');
    }
    normalized
}

#[tokio::test]
async fn chat_completions_should_return_openai_error_when_no_accounts_are_available() {
    let server = MockServer::start().await;
    let imported = build_imported_app_with_accounts(server.uri(), &[]).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [
                            {"role": "user", "content": "Say ok"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response_json(response).await;
    let expected: Value = serde_json::from_str(CHAT_NO_AVAILABLE_ACCOUNTS_GOLDEN)
        .expect("chat no-accounts golden should parse");
    assert_eq!(body, expected);
}

#[tokio::test]
async fn chat_completions_should_return_openai_error_when_429_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {"message": "rate limited"}
        })))
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [
                            {"role": "user", "content": "Say ok"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = response_json(response).await;
    let expected: Value = serde_json::from_str(CHAT_RATE_LIMIT_FALLBACK_EXHAUSTED_GOLDEN)
        .expect("chat rate-limit golden should parse");
    assert_eq!(body, expected);
}

#[tokio::test]
async fn chat_completions_should_return_openai_error_when_402_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "error": {"message": "quota reached"}
        })))
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [
                            {"role": "user", "content": "Say ok"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    let body = response_json(response).await;
    let expected: Value = serde_json::from_str(CHAT_QUOTA_EXHAUSTED_FALLBACK_EXHAUSTED_GOLDEN)
        .expect("chat quota-exhausted golden should parse");
    assert_eq!(body, expected);
}

#[tokio::test]
async fn chat_completions_should_use_user_as_client_conversation_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_NON_STREAM_TEXT_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "user": "chat-client-session",
                        "messages": [
                            {"role": "user", "content": "Say ok"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let upstream_body = single_upstream_post_body(&server).await;
    let identity = build_conversation_identity(Some("chat-client-session"), None, "acct_chat");
    assert_eq!(
        upstream_body["prompt_cache_key"],
        identity.conversation_id.unwrap()
    );
}

#[tokio::test]
async fn chat_completions_stream_should_retry_next_account_after_429_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-a"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "90")
                .set_body_json(json!({
                    "error": {"message": "rate limited"}
                })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-b"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_RETRY_AFTER_SUCCESS_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app_with_accounts(
        server.uri(),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "stream": true,
                        "messages": [
                            {"role": "user", "content": "Say ok"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert_eq!(
        normalize_chat_stream(&body),
        with_sse_terminal_separator(CHAT_RETRY_AFTER_STREAM_GOLDEN)
    );
}

#[tokio::test]
async fn chat_completions_translates_messages_and_returns_openai_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_NON_STREAM_TEXT_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [
                            {"role": "system", "content": "You are concise."},
                            {"role": "user", "content": "Say hello"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let expected: Value = serde_json::from_str(CHAT_NON_STREAM_TEXT_GOLDEN)
        .expect("chat non-stream text golden should parse");
    assert_eq!(normalize_chat_completion(body), expected);
    let upstream_body = single_upstream_post_body(&server).await;
    assert_eq!(upstream_body["model"], "gpt-5.5");
    assert_eq!(upstream_body["instructions"], "You are concise.");
    assert_eq!(upstream_body["input"][0]["role"], "user");
    assert_eq!(upstream_body["input"][0]["content"], "Say hello");
    assert!(upstream_body["client_metadata"]["x-codex-installation-id"].is_string());
}

#[tokio::test]
async fn chat_completions_should_convert_and_reconvert_tuple_schema() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_TUPLE_OBJECT_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [{"role": "user", "content": "Return JSON"}],
                        "response_format": {
                            "type": "json_schema",
                            "json_schema": {
                                "name": "TupleAnswer",
                                "schema": {
                                    "type": "object",
                                    "properties": {
                                        "point": {
                                            "type": "array",
                                            "prefixItems": [
                                                {"type": "number"},
                                                {"type": "number"}
                                            ]
                                        }
                                    }
                                },
                                "strict": true
                            }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let expected: Value =
        serde_json::from_str(CHAT_TUPLE_RECONVERT_GOLDEN).expect("chat tuple golden should parse");
    assert_eq!(normalize_chat_completion(body), expected);
    let upstream_body = single_upstream_post_body(&server).await;
    let schema = &upstream_body["text"]["format"]["schema"];
    assert!(schema["properties"]["point"].get("prefixItems").is_none());
    assert_eq!(schema["properties"]["point"]["type"], "object");
}

#[tokio::test]
async fn chat_completions_streams_openai_chunks_from_codex_sse() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_STREAM_TEXT_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "stream": true,
                        "messages": [
                            {"role": "user", "content": "Say hello"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert_eq!(
        normalize_chat_stream(&body),
        with_sse_terminal_separator(CHAT_STREAM_TEXT_GOLDEN)
    );
}

#[tokio::test]
async fn chat_completions_stream_should_reconvert_tuple_schema() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_TUPLE_OBJECT_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "stream": true,
                        "messages": [{"role": "user", "content": "Return JSON"}],
                        "response_format": {
                            "type": "json_schema",
                            "json_schema": {
                                "name": "TupleAnswer",
                                "schema": {
                                    "type": "object",
                                    "properties": {
                                        "point": {
                                            "type": "array",
                                            "prefixItems": [
                                                {"type": "number"},
                                                {"type": "number"}
                                            ]
                                        }
                                    }
                                },
                                "strict": true
                            }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert_eq!(
        normalize_chat_stream(&body),
        with_sse_terminal_separator(CHAT_TUPLE_RECONVERT_STREAM_GOLDEN)
    );
}

#[tokio::test]
async fn chat_completions_forwards_parallel_tools_and_model_suffix_options() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_PARALLEL_TOOLS_SUCCESS_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5-high-fast",
                        "parallel_tool_calls": true,
                        "messages": [
                            {"role": "user", "content": "Use tools if needed"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let expected: Value = serde_json::from_str(CHAT_PARALLEL_TOOLS_GOLDEN)
        .expect("chat parallel tools golden should parse");
    assert_eq!(normalize_chat_completion(body), expected);
    let upstream_body = single_upstream_post_body(&server).await;
    assert_eq!(upstream_body["reasoning"]["effort"], "high");
    assert_eq!(upstream_body["reasoning"]["summary"], "auto");
    assert_eq!(upstream_body["service_tier"], "priority");
    assert_eq!(upstream_body["parallel_tool_calls"], true);
}

#[tokio::test]
async fn chat_completions_collects_tool_calls_and_reasoning_content() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_TOOL_REASONING_COMPLETE_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "reasoning_effort": "high",
                        "messages": [
                            {"role": "user", "content": "Find it"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let expected: Value = serde_json::from_str(CHAT_TOOL_REASONING_GOLDEN)
        .expect("chat tool reasoning golden should parse");
    assert_eq!(normalize_chat_completion(body), expected);
}

#[tokio::test]
async fn chat_completions_streams_tool_call_chunks_from_codex_sse() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_TOOL_STREAM_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "stream": true,
                        "messages": [
                            {"role": "user", "content": "Find it"}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert_eq!(
        normalize_chat_stream(&body),
        with_sse_terminal_separator(CHAT_TOOL_STREAM_GOLDEN)
    );
}

fn with_sse_terminal_separator(body: &str) -> String {
    if body.ends_with("\n\n") {
        body.to_string()
    } else {
        format!("{body}\n")
    }
}
