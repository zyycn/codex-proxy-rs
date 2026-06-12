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
    crypto::SecretBox,
    state::AppState,
    storage::db::connect_sqlite,
};

struct ImportedApp {
    app: axum::Router,
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
                            "id": "acct_chat",
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
        client_api_key: generated.plaintext,
        _tempdir: tempdir,
    }
}

#[tokio::test]
async fn chat_completions_translates_messages_and_returns_openai_response() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"delta\":\"hello\"}\n",
        "\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_chat\",\"object\":\"response\",\"status\":\"completed\",\"usage\":{\"input_tokens\":9,\"output_tokens\":3}}}\n",
        "\n",
    );
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
    let sse_body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"delta\":\"hel\"}\n",
        "\n",
        "event: response.output_text.delta\n",
        "data: {\"delta\":\"lo\"}\n",
        "\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_chat\",\"object\":\"response\",\"status\":\"completed\",\"usage\":{\"input_tokens\":9,\"output_tokens\":3,\"input_tokens_details\":{\"cached_tokens\":4}}}}\n",
        "\n",
    );
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
    let sse_body = concat!(
        "event: response.output_text.delta\n",
        "data: {\"delta\":\"ok\"}\n",
        "\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_chat\",\"object\":\"response\",\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n",
        "\n",
    );
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
    let sse_body = concat!(
        "event: response.reasoning_summary_text.delta\n",
        "data: {\"delta\":\"thinking \"}\n",
        "\n",
        "event: response.output_item.added\n",
        "data: {\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_abc\",\"name\":\"lookup\"}}\n",
        "\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"item_id\":\"item_1\",\"delta\":\"{\\\"city\\\"\"}\n",
        "\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"item_id\":\"item_1\",\"delta\":\":\\\"Paris\\\"}\"}\n",
        "\n",
        "event: response.function_call_arguments.done\n",
        "data: {\"item_id\":\"item_1\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\"}\n",
        "\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_chat\",\"object\":\"response\",\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"output_tokens_details\":{\"reasoning_tokens\":7}}}}\n",
        "\n",
    );
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
    let sse_body = concat!(
        "event: response.output_item.added\n",
        "data: {\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_abc\",\"name\":\"lookup\"}}\n",
        "\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"item_id\":\"item_1\",\"delta\":\"{\\\"city\\\"\"}\n",
        "\n",
        "event: response.function_call_arguments.delta\n",
        "data: {\"item_id\":\"item_1\",\"delta\":\":\\\"Paris\\\"}\"}\n",
        "\n",
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_chat\",\"object\":\"response\",\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n",
        "\n",
    );
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

async fn response_sse_data(response: axum::response::Response) -> Vec<String> {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("data: ").map(ToString::to_string))
        .collect()
}
