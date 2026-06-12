use std::collections::BTreeMap;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::{
    matchers::{body_json, header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::{
    app::build_router,
    app::state::AppState,
    auth::{api_key::ApiKeyHasher, api_key_repository::ClientApiKeyRepository},
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    storage::db::connect_sqlite,
    utils::crypto::SecretBox,
};

mod common;

use common::{response_json, response_sse_data, seed_admin_session};

const CHAT_RETRY_AFTER_SUCCESS_SSE: &str = include_str!("fixtures/chat_retry_after_success.sse");
const CHAT_NON_STREAM_TEXT_SSE: &str = include_str!("fixtures/chat_non_stream_text.sse");
const CHAT_STREAM_TEXT_SSE: &str = include_str!("fixtures/chat_stream_text.sse");
const CHAT_PARALLEL_TOOLS_SUCCESS_SSE: &str =
    include_str!("fixtures/chat_parallel_tools_success.sse");
const CHAT_TOOL_REASONING_COMPLETE_SSE: &str =
    include_str!("fixtures/chat_tool_reasoning_complete.sse");
const CHAT_TOOL_STREAM_SSE: &str = include_str!("fixtures/chat_tool_stream.sse");

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
    let accounts = accounts
        .iter()
        .map(|account| {
            json!({
                "id": account.id,
                "accountId": account.account_id,
                "token": account.token,
                "refreshToken": account.refresh_token,
                "status": "active"
            })
        })
        .collect::<Vec<_>>();

    let import_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({ "accounts": accounts }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(import_response.status(), StatusCode::OK);

    ImportedApp {
        app,
        client_api_key: generated.plaintext,
        _tempdir: tempdir,
    }
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
    let frames = response_sse_data(response).await;
    assert_eq!(frames.last().unwrap(), "[DONE]");
    let chunks = frames
        .iter()
        .filter(|frame| frame.as_str() != "[DONE]")
        .map(|frame| serde_json::from_str::<Value>(frame).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(chunks[1]["choices"][0]["delta"]["content"], "ok");
    assert_eq!(chunks[2]["usage"]["prompt_tokens"], 4);
}

#[tokio::test]
async fn chat_completions_translates_messages_and_returns_openai_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .and(body_json(json!({
            "model": "gpt-5.5",
            "instructions": "You are concise.",
            "input": [{
                "role": "user",
                "content": "Say hello"
            }],
            "stream": true,
            "store": false
        })))
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
    assert!(body["id"].as_str().unwrap().starts_with("chatcmpl-"));
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["model"], "gpt-5.5");
    assert_eq!(body["choices"][0]["message"]["role"], "assistant");
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert_eq!(body["usage"]["prompt_tokens"], 9);
    assert_eq!(body["usage"]["completion_tokens"], 3);
    assert_eq!(body["usage"]["total_tokens"], 12);
}

#[tokio::test]
async fn chat_completions_streams_openai_chunks_from_codex_sse() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .and(body_json(json!({
            "model": "gpt-5.5",
            "instructions": "You are a helpful assistant.",
            "input": [{
                "role": "user",
                "content": "Say hello"
            }],
            "stream": true,
            "store": false
        })))
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
    let frames = response_sse_data(response).await;
    assert_eq!(frames.last().unwrap(), "[DONE]");
    let chunks = frames
        .iter()
        .filter(|frame| frame.as_str() != "[DONE]")
        .map(|frame| serde_json::from_str::<Value>(frame).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(chunks[0]["object"], "chat.completion.chunk");
    assert_eq!(chunks[0]["model"], "gpt-5.5");
    assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
    assert_eq!(chunks[1]["choices"][0]["delta"]["content"], "hel");
    assert_eq!(chunks[2]["choices"][0]["delta"]["content"], "lo");
    assert_eq!(chunks[3]["choices"][0]["finish_reason"], "stop");
    assert_eq!(chunks[3]["usage"]["prompt_tokens"], 9);
    assert_eq!(chunks[3]["usage"]["completion_tokens"], 3);
    assert_eq!(chunks[3]["usage"]["total_tokens"], 12);
    assert_eq!(
        chunks[3]["usage"]["prompt_tokens_details"]["cached_tokens"],
        4
    );
}

#[tokio::test]
async fn chat_completions_forwards_parallel_tools_and_model_suffix_options() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .and(body_json(json!({
            "model": "gpt-5.5",
            "instructions": "You are a helpful assistant.",
            "input": [{
                "role": "user",
                "content": "Use tools if needed"
            }],
            "stream": true,
            "store": false,
            "reasoning": {
                "effort": "high",
                "summary": "auto"
            },
            "service_tier": "priority",
            "parallel_tool_calls": true
        })))
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
    assert_eq!(body["model"], "gpt-5.5-high-fast");
}

#[tokio::test]
async fn chat_completions_collects_tool_calls_and_reasoning_content() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .and(body_json(json!({
            "model": "gpt-5.5",
            "instructions": "You are a helpful assistant.",
            "input": [{
                "role": "user",
                "content": "Find it"
            }],
            "stream": true,
            "store": false,
            "reasoning": {
                "effort": "high",
                "summary": "auto"
            }
        })))
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
    let message = &body["choices"][0]["message"];
    assert_eq!(message["content"], Value::Null);
    assert_eq!(message["reasoning_content"], "thinking ");
    assert_eq!(message["tool_calls"][0]["id"], "call_abc");
    assert_eq!(message["tool_calls"][0]["type"], "function");
    assert_eq!(message["tool_calls"][0]["function"]["name"], "lookup");
    assert_eq!(
        message["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"Paris\"}"
    );
    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        body["usage"]["completion_tokens_details"]["reasoning_tokens"],
        7
    );
}

#[tokio::test]
async fn chat_completions_streams_tool_call_chunks_from_codex_sse() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .and(body_json(json!({
            "model": "gpt-5.5",
            "instructions": "You are a helpful assistant.",
            "input": [{
                "role": "user",
                "content": "Find it"
            }],
            "stream": true,
            "store": false
        })))
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
    let frames = response_sse_data(response).await;
    let chunks = frames
        .iter()
        .filter(|frame| frame.as_str() != "[DONE]")
        .map(|frame| serde_json::from_str::<Value>(frame).unwrap())
        .collect::<Vec<_>>();

    let start_call = &chunks[1]["choices"][0]["delta"]["tool_calls"][0];
    assert_eq!(start_call["index"], 0);
    assert_eq!(start_call["id"], "call_abc");
    assert_eq!(start_call["type"], "function");
    assert_eq!(start_call["function"]["name"], "lookup");
    assert_eq!(start_call["function"]["arguments"], "");
    assert_eq!(
        chunks[2]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
        "{\"city\""
    );
    assert_eq!(
        chunks[3]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
        ":\"Paris\"}"
    );
    assert_eq!(chunks[4]["choices"][0]["finish_reason"], "tool_calls");
}
