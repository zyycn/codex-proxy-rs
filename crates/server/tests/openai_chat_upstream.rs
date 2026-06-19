use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration as StdDuration, Instant},
};

use axum::{
    body::{to_bytes, Body, Bytes},
    http::{Request, StatusCode},
};
use chrono::{DateTime, Duration, Utc};
use codex_proxy_adapters::sqlite::{
    cookies::SqliteCookieStore,
    events::{EventLogFilter, SqliteEventLogStore},
    session_affinity::SqliteSessionAffinityStore,
};
use codex_proxy_core::{
    events::model::EventLevel, gateway::conversation::build_conversation_identity,
    serving::affinity::SessionAffinityEntry,
};
use codex_proxy_platform::{
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig, WebSocketPoolConfig,
    },
    crypto::SecretBox,
    identity::ApiKeyHasher,
    storage::connect_sqlite,
};
use codex_proxy_runtime::state::AppState;
use codex_proxy_server::router;
use futures::{SinkExt, StreamExt};
use secrecy::SecretString;
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::timeout,
};
use tokio_tungstenite::{
    accept_async, accept_hdr_async,
    tungstenite::{
        handshake::server::{Request as WsRequest, Response as WsResponse},
        Message,
    },
};
use tower::util::ServiceExt;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

const TEST_INSTALLATION_ID: &str = "b4f9d503-07b1-457b-a0da-87e6836b1c43";

const CHAT_SUCCESS_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"hello"}

event: response.completed
data: {"response":{"id":"resp_1","output_text":"hello","usage":{"input_tokens":9,"output_tokens":3}}}

"#;

const RESPONSES_SUCCESS_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"response hello"}

event: response.completed
data: {"response":{"id":"resp_response_1","object":"response","status":"completed","usage":{"input_tokens":5,"output_tokens":2}}}

"#;

const RESPONSES_COMPLETED_USAGE_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"usage hello"}

event: response.completed
data: {"response":{"id":"resp_usage","object":"response","status":"completed","output_text":"usage hello","usage":{"input_tokens":7,"output_tokens":4,"input_tokens_details":{"cached_tokens":2}}}}

"#;

const RESPONSES_COMPLETED_IMAGE_USAGE_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"image done"}

event: response.completed
data: {"response":{"id":"resp_image","object":"response","status":"completed","output_text":"image done","usage":{"input_tokens":7,"output_tokens":4,"input_tokens_details":{"cached_tokens":2}},"tool_usage":{"image_gen":{"input_tokens":31,"output_tokens":9}}}}

"#;

const RESPONSES_EMPTY_COMPLETED_SSE: &str = r#"event: response.completed
data: {"response":{"id":"resp_empty","object":"response","status":"completed"}}

"#;

const RESPONSES_TEXT_DELTAS_COMPLETED_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"hello "}

event: response.output_text.delta
data: {"delta":"from deltas"}

event: response.completed
data: {"response":{"id":"resp_text","object":"response","status":"completed","output":[],"usage":{"input_tokens":2,"output_tokens":3}}}

"#;

const RESPONSES_DONE_ITEM_COMPLETED_SSE: &str = r#"event: response.output_item.done
data: {"item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"from done item"}]}}

event: response.completed
data: {"response":{"id":"resp_item","object":"response","status":"completed","output":[],"usage":{"input_tokens":2,"output_tokens":3}}}

"#;

const RESPONSES_TUPLE_OBJECT_SSE: &str = r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"{\"point\":{\"0\":1,\"1\":2}}"}

event: response.completed
data: {"response":{"id":"resp_tuple","object":"response","status":"completed","output_text":"{\"point\":{\"0\":0,\"1\":0}}","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"{\"point\":{\"0\":1,\"1\":2}}"}]}],"usage":{"input_tokens":1,"output_tokens":1}}}

"#;

const RESPONSES_AFTER_5XX_RETRY_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"retry hello"}

event: response.completed
data: {"response":{"id":"resp_after_5xx_retry","object":"response","status":"completed","usage":{"input_tokens":4,"output_tokens":2}}}

"#;

const RESPONSES_AFTER_402_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"quota fallback hello"}

event: response.completed
data: {"response":{"id":"resp_after_402","object":"response","status":"completed","usage":{"input_tokens":6,"output_tokens":2}}}

"#;

const RESPONSES_FAILED_QUOTA_SSE: &str = r#"event: response.failed
data: {"response":{"id":"resp_sse_quota_failed","object":"response","status":"failed","error":{"code":"quota_exceeded","message":"quota exhausted"}}}

"#;

const RESPONSES_FAILED_AUTH_SSE: &str = r#"event: response.failed
data: {"response":{"id":"resp_sse_auth_failed","object":"response","status":"failed","error":{"code":"token_invalid","message":"token revoked"}}}

"#;

const RESPONSES_AFTER_401_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"auth fallback hello"}

event: response.completed
data: {"response":{"id":"resp_after_401","object":"response","status":"completed","usage":{"input_tokens":3,"output_tokens":1}}}

"#;

const RESPONSES_AFTER_403_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"banned fallback hello"}

event: response.completed
data: {"response":{"id":"resp_after_403","object":"response","status":"completed","usage":{"input_tokens":5,"output_tokens":1}}}

"#;

const RESPONSES_STREAM_USAGE_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"stream hello"}

event: response.completed
data: {"response":{"id":"resp_stream_usage","object":"response","status":"completed","usage":{"input_tokens":3,"output_tokens":5}}}

"#;

const RESPONSES_STREAM_AFTER_429_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"rate fallback hello"}

event: response.completed
data: {"response":{"id":"resp_stream_after_429","object":"response","status":"completed","usage":{"input_tokens":4,"output_tokens":1}}}

"#;

const RESPONSES_STREAM_AFTER_5XX_RETRY_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"retry stream hello"}

event: response.completed
data: {"response":{"id":"resp_stream_after_5xx_retry","object":"response","status":"completed","usage":{"input_tokens":7,"output_tokens":2}}}

"#;

const RESPONSES_STREAM_AFTER_402_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"quota stream fallback hello"}

event: response.completed
data: {"response":{"id":"resp_stream_after_402","object":"response","status":"completed","usage":{"input_tokens":6,"output_tokens":3}}}

"#;

const RESPONSES_STREAM_AFTER_401_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"auth stream fallback hello"}

event: response.completed
data: {"response":{"id":"resp_stream_after_401","object":"response","status":"completed","usage":{"input_tokens":4,"output_tokens":2}}}

"#;

const RESPONSES_STREAM_AFTER_403_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"banned stream fallback hello"}

event: response.completed
data: {"response":{"id":"resp_stream_after_403","object":"response","status":"completed","usage":{"input_tokens":5,"output_tokens":2}}}

"#;

const RESPONSES_AFTER_CLOUDFLARE_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"cloudflare fallback hello"}

event: response.completed
data: {"response":{"id":"resp_after_cloudflare","object":"response","status":"completed","usage":{"input_tokens":5,"output_tokens":2}}}

"#;

const RESPONSES_STREAM_AFTER_CLOUDFLARE_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"cloudflare stream fallback hello"}

event: response.completed
data: {"response":{"id":"resp_stream_after_cloudflare","object":"response","status":"completed","usage":{"input_tokens":6,"output_tokens":2}}}

"#;

const RESPONSES_AFTER_MODEL_UNSUPPORTED_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"model fallback hello"}

event: response.completed
data: {"response":{"id":"resp_after_model_unsupported","object":"response","status":"completed","usage":{"input_tokens":4,"output_tokens":2}}}

"#;

const RESPONSES_STREAM_AFTER_MODEL_UNSUPPORTED_SSE: &str = r#"event: response.output_text.delta
data: {"delta":"model stream fallback hello"}

event: response.completed
data: {"response":{"id":"resp_stream_after_model_unsupported","object":"response","status":"completed","usage":{"input_tokens":5,"output_tokens":2}}}

"#;

const RESPONSES_FAILED_MODEL_UNSUPPORTED_SSE: &str = r#"event: response.failed
data: {"response":{"id":"resp_model_unsupported_failed","object":"response","status":"failed","error":{"code":"model_not_supported","message":"Model gpt-5.5 is not supported on this account plan"}}}

"#;

const WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY: &str = include_str!(
    "../../../tests/fixtures/responses/websocket/completed_with_reasoning_replay.json"
);
const WEBSOCKET_HISTORY_RATE_LIMITED: &str =
    include_str!("../../../tests/fixtures/responses/websocket/history_rate_limited.json");
const WEBSOCKET_RATE_LIMITED: &str =
    include_str!("../../../tests/fixtures/responses/websocket/rate_limited.json");
const WEBSOCKET_TOKEN_REVOKED: &str =
    include_str!("../../../tests/fixtures/responses/websocket/token_revoked.json");
const WEBSOCKET_FIRST_ACCOUNT_LIMITED: &str =
    include_str!("../../../tests/fixtures/responses/websocket/first_account_limited.json");
const WEBSOCKET_SECOND_ACCOUNT_LIMITED: &str =
    include_str!("../../../tests/fixtures/responses/websocket/second_account_limited.json");
const WEBSOCKET_INVALID_ENCRYPTED_CONTENT: &str =
    include_str!("../../../tests/fixtures/responses/websocket/invalid_encrypted_content.json");
const WEBSOCKET_PREVIOUS_RESPONSE_NOT_FOUND: &str =
    include_str!("../../../tests/fixtures/responses/websocket/previous_response_not_found.json");
const WEBSOCKET_UNANSWERED_FUNCTION_CALL: &str =
    include_str!("../../../tests/fixtures/responses/websocket/unanswered_function_call.json");
const REASONING_REPLAY_REQUEST_GOLDEN: &str =
    include_str!("../../../tests/fixtures/responses/golden/reasoning_replay_request.json");

#[tokio::test]
async fn chat_completions_should_dispatch_to_codex_and_return_openai_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_SUCCESS_SSE),
        )
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
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
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["model"], "gpt-5.5");
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
    assert_eq!(body["usage"]["prompt_tokens"], 9);
    assert_eq!(body["usage"]["completion_tokens"], 3);
    let usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(usage, (1, 9, 3));
}

#[tokio::test]
async fn chat_completions_should_forward_runtime_installation_id_to_codex() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("x-codex-installation-id", TEST_INSTALLATION_ID))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_SUCCESS_SSE),
        )
        .mount(&server)
        .await;

    let (app, api_key, _dir) =
        test_app_with_account_and_installation_id(server.uri(), TEST_INSTALLATION_ID.to_string())
            .await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
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
}

#[tokio::test]
async fn chat_completions_should_dispatch_from_restored_runtime_account_pool() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_SUCCESS_SSE),
        )
        .mount(&server)
        .await;

    let (app, api_key, _dir) =
        test_app_with_restored_pool_then_disabled_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
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
}

#[tokio::test]
async fn chat_completions_should_fallback_to_next_account_after_rate_limit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "120")
                .set_body_json(json!({
                    "error": {
                        "message": "rate limited",
                        "resets_in_seconds": 120
                    }
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [{"role": "user", "content": "Say hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let authorizations = received_authorizations(&server).await;

    assert_eq!(status, StatusCode::OK, "requests: {authorizations:?}");
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
    let persisted_quota_state: (i64, Option<String>) = sqlx::query_as(
        "select quota_limit_reached, quota_cooldown_until from accounts where id = ?",
    )
    .bind("acct_primary")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(persisted_quota_state.0, 1);
    let cooldown_until =
        chrono::DateTime::parse_from_rfc3339(persisted_quota_state.1.as_deref().unwrap())
            .unwrap()
            .with_timezone(&Utc);
    assert!(cooldown_until > Utc::now());
}

#[tokio::test]
async fn chat_completions_should_return_rate_limit_error_when_429_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {"message": "rate limited"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [{"role": "user", "content": "Say hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let quota_state: (i64, Option<String>) = sqlx::query_as(
        "select quota_limit_reached, quota_cooldown_until from accounts where id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();
    let message = body["error"]["message"].as_str().unwrap_or_default();

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(message.contains("All accounts exhausted (1 rate-limited)"));
    assert!(message.contains("rate limited"));
    assert_eq!(body["error"]["code"], "upstream_error");
    assert_eq!(quota_state.0, 1);
    assert!(quota_state.1.is_some());
}

#[tokio::test]
async fn chat_completions_should_fallback_after_http_model_unsupported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "code": "model_not_supported",
                "message": "Model gpt-5.5 is not supported on this account plan"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [{"role": "user", "content": "Say hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let primary_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
    assert_eq!(primary_status.0, "active");
}

#[tokio::test]
async fn chat_completions_should_return_model_unsupported_error_when_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "code": "model_not_supported",
                "message": "Model gpt-5.5 is not supported on this account plan"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [{"role": "user", "content": "Say hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let message = body["error"]["message"].as_str().unwrap_or_default();
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(message.contains("All accounts exhausted (1 model-unsupported)"));
    assert!(message.contains("model_not_supported"));
    assert_eq!(body["error"]["code"], "upstream_error");
    assert_eq!(account_status.0, "active");
}

#[tokio::test]
async fn chat_completions_should_mark_expired_after_401_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_revoked", "message": "token revoked"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [{"role": "user", "content": "Say hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
    assert_eq!(account_status.0, "expired");
}

#[tokio::test]
async fn chat_completions_should_return_auth_error_when_401_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_revoked", "message": "token revoked"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [{"role": "user", "content": "Say hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let message = body["error"]["message"].as_str().unwrap_or_default();
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(message.contains("All accounts exhausted (1 expired)"));
    assert!(message.contains("token_revoked"));
    assert_eq!(body["error"]["code"], "upstream_error");
    assert_eq!(account_status.0, "expired");
}

#[tokio::test]
async fn chat_completions_should_mark_banned_when_401_says_account_deactivated() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "account deactivated"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [{"role": "user", "content": "Say hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(account_status.0, "banned");
}

#[tokio::test]
async fn chat_completions_should_mark_banned_after_403_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"message": "account banned"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [{"role": "user", "content": "Say hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
    assert_eq!(account_status.0, "banned");
}

#[tokio::test]
async fn chat_completions_should_cool_down_cloudflare_403_and_fallback() {
    let started_at = Utc::now();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<html><title>Just a moment...</title><body>cf_chl challenge</body></html>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let cookie_store = SqliteCookieStore::new(pool.clone(), SecretBox::new([83u8; 32]));
    cookie_store
        .set_cookie_header("acct_primary", "cf_clearance=old")
        .await
        .unwrap();
    cookie_store
        .set_cookie_header("acct_secondary", "cf_clearance=keep")
        .await
        .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [{"role": "user", "content": "Say hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let primary_state: (String, Option<String>) =
        sqlx::query_as("select status, cloudflare_cooldown_until from accounts where id = ?")
            .bind("acct_primary")
            .fetch_one(&pool)
            .await
            .unwrap();
    let cooldown_until = chrono::DateTime::parse_from_rfc3339(primary_state.1.as_deref().unwrap())
        .unwrap()
        .with_timezone(&Utc);

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
    assert_eq!(primary_state.0, "active");
    assert!(cooldown_until > started_at);
    assert_eq!(
        cookie_store
            .cookie_header("acct_primary", "chatgpt.com")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        cookie_store
            .cookie_header("acct_secondary", "chatgpt.com")
            .await
            .unwrap()
            .as_deref(),
        Some("cf_clearance=keep")
    );
}

#[tokio::test]
async fn chat_completions_should_mark_quota_exhausted_after_402_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "error": {"message": "quota reached"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CHAT_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [{"role": "user", "content": "Say hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
    assert_eq!(account_status.0, "quota_exhausted");
}

#[tokio::test]
async fn chat_completions_should_return_quota_error_when_402_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "error": {"message": "quota reached"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "messages": [{"role": "user", "content": "Say hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();
    let message = body["error"]["message"].as_str().unwrap_or_default();

    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert!(message.contains("All accounts exhausted (1 quota-exhausted)"));
    assert!(message.contains("quota reached"));
    assert_eq!(body["error"]["code"], "upstream_error");
    assert_eq!(account_status.0, "quota_exhausted");
}

#[tokio::test]
async fn responses_should_dispatch_to_codex_and_return_completed_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_SUCCESS_SSE),
        )
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_response_1");
    assert_eq!(body["output_text"], "response hello");
    assert_eq!(body["usage"]["input_tokens"], 5);
    assert_eq!(body["usage"]["output_tokens"], 2);
}

#[tokio::test]
async fn responses_should_reject_invalid_json_without_upstream_request() {
    let server = MockServer::start().await;
    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let requests = server.received_requests().await.unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
    assert!(requests.is_empty());
}

#[tokio::test]
async fn responses_should_reject_non_object_json_without_upstream_request() {
    let server = MockServer::start().await;
    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from("[]"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let requests = server.received_requests().await.unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
    assert!(requests.is_empty());
}

#[tokio::test]
async fn responses_should_return_no_available_accounts_error_when_no_accounts_are_available() {
    let server = MockServer::start().await;
    let (app, api_key, _dir) = test_app_without_accounts(server.uri()).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "server_error");
    assert_eq!(body["error"]["code"], "no_available_accounts");
}

#[tokio::test]
async fn responses_should_honor_explicit_http_sse_transport() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_STREAM_USAGE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_text(response).await;
    let requests = server.received_requests().await.unwrap();
    let upstream_body: Value = serde_json::from_slice(&requests[0].body).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("event: response.output_text.delta"));
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "POST");
    assert_eq!(upstream_body["model"], "gpt-5.5");
    assert!(upstream_body.get("use_websocket").is_none());
}

#[tokio::test]
async fn responses_should_stagger_same_account_requests_before_sending_upstream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let request_times = Arc::new(Mutex::new(Vec::new()));
    let request_times_for_server = Arc::clone(&request_times);
    let (first_seen_tx, first_seen_rx) = oneshot::channel();
    let (release_first_tx, release_first_rx) = oneshot::channel();
    let upstream = tokio::spawn(async move {
        let (mut first_socket, _) = listener.accept().await.unwrap();
        request_times_for_server
            .lock()
            .unwrap()
            .push(Instant::now());
        first_seen_tx.send(()).unwrap();
        read_http_request(&mut first_socket).await;

        let (mut second_socket, _) = listener.accept().await.unwrap();
        request_times_for_server
            .lock()
            .unwrap()
            .push(Instant::now());
        read_http_request(&mut second_socket).await;
        write_http_sse_response(&mut second_socket, RESPONSES_COMPLETED_USAGE_SSE).await;

        release_first_rx.await.unwrap();
        write_http_sse_response(&mut first_socket, RESPONSES_COMPLETED_USAGE_SSE).await;
    });

    let (app, api_key, _pool, _dir) = test_app_with_account_pool_config(base_url, |config| {
        config.auth.max_concurrent_per_account = 2;
        config.auth.request_interval_ms = 300;
    })
    .await;
    let first_app = app.clone();
    let first_api_key = api_key.clone();
    let first_response = tokio::spawn(async move {
        first_app
            .oneshot(responses_http_sse_request(
                &first_api_key,
                "req_stagger_first",
            ))
            .await
            .unwrap()
    });
    first_seen_rx.await.unwrap();

    let second = app
        .clone()
        .oneshot(responses_http_sse_request(&api_key, "req_stagger_second"))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    release_first_tx.send(()).unwrap();
    let first = first_response.await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    upstream.await.unwrap();

    let times = request_times.lock().unwrap();
    assert_eq!(times.len(), 2);
    let elapsed = times[1].duration_since(times[0]);
    assert!(
        elapsed >= StdDuration::from_millis(180),
        "second upstream request was sent too early: {elapsed:?}"
    );
}

#[tokio::test]
async fn responses_should_use_websocket_upstream_by_default_while_serving_sse() {
    let (base_url, upstream) = spawn_single_websocket_completed_upstream("resp_ws_default").await;
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "generate": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let captured = upstream.await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("event: response.completed"));
    assert!(body.contains("\"id\":\"resp_ws_default\""));
    assert_eq!(captured.payload["type"], "response.create");
    assert_eq!(captured.payload["model"], "gpt-5.5");
    assert_eq!(captured.payload["generate"], false);
    assert!(captured.payload.get("previous_response_id").is_none());
    assert!(captured.payload["prompt_cache_key"]
        .as_str()
        .is_some_and(|value| value.starts_with("cp_")));
    assert!(captured.payload["client_metadata"]["x-codex-installation-id"].is_string());
    assert!(captured.payload["client_metadata"]["x-codex-window-id"]
        .as_str()
        .is_some_and(|value| value.starts_with("cp_") && value.ends_with(":0")));
}

#[tokio::test]
async fn responses_should_ignore_camel_case_use_websocket_field() {
    let (base_url, upstream) =
        spawn_single_websocket_completed_upstream("resp_ws_camel_case").await;
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "useWebSocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let captured = upstream.await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(captured.payload["type"], "response.create");
    assert!(captured.payload.get("useWebSocket").is_none());
    assert!(captured.payload.get("use_websocket").is_none());
}

#[tokio::test]
async fn responses_websocket_should_stream_first_frame_before_terminal_event() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (first_frame_tx, first_frame_rx) = oneshot::channel();
    let (terminal_tx, terminal_rx) = oneshot::channel();
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let payload = serde_json::from_str::<Value>(&message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "first websocket frame"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        first_frame_tx.send(()).unwrap();
        terminal_rx.await.unwrap();
        websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_ws_streaming").into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        payload
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;
    let response_task = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": true,
                        "use_websocket": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
    });

    first_frame_rx.await.unwrap();
    let response = timeout(StdDuration::from_millis(250), response_task)
        .await
        .expect("websocket response should be returned after the first non-error frame")
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let first_chunk = timeout(
        StdDuration::from_secs(1),
        first_response_body_chunk(response),
    )
    .await
    .expect("downstream should receive a websocket-backed SSE chunk before completion")
    .expect("response body should produce a chunk");
    terminal_tx.send(()).unwrap();
    let payload = upstream.await.unwrap();

    assert!(first_chunk.contains("event: response.output_text.delta"));
    assert_eq!(payload["type"], "response.create");
}

#[tokio::test]
async fn responses_websocket_stream_should_synthesize_response_failed_when_closed_before_terminal()
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let payload = serde_json::from_str::<Value>(&message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "partial before websocket close"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        payload
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": true,
                        "use_websocket": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_text(response).await;
    let payload = upstream.await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload["type"], "response.create");
    assert!(body.contains("event: response.output_text.delta"));
    assert!(body.contains("event: response.failed"));
    assert!(body.contains("stream_disconnected"));
}

#[tokio::test]
async fn responses_websocket_should_reuse_connection_for_recorded_conversation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_payload = serde_json::from_str::<Value>(&first_message.into_text().unwrap())
            .expect("first websocket payload should be json");
        websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_pool_first").into(),
            ))
            .await
            .unwrap();

        loop {
            tokio::select! {
                message = websocket.next() => {
                    match message {
                        Some(Ok(message)) if message.is_text() => {
                            let second_payload = serde_json::from_str::<Value>(
                                &message.into_text().unwrap(),
                            )
                            .expect("second websocket payload should be json");
                            websocket
                                .send(Message::Text(
                                    response_completed_websocket_message("resp_pool_second").into(),
                                ))
                                .await
                                .unwrap();
                            websocket.close(None).await.unwrap();
                            break (true, first_payload, second_payload);
                        }
                        Some(_) => continue,
                        None => {
                            let second_payload = accept_successful_websocket_response(
                                &listener,
                                "resp_pool_second",
                            )
                            .await;
                            break (false, first_payload, second_payload);
                        }
                    }
                }
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let mut second_websocket = accept_async(stream).await.unwrap();
                    let second_message = second_websocket.next().await.unwrap().unwrap();
                    let second_payload = serde_json::from_str::<Value>(
                        &second_message.into_text().unwrap(),
                    )
                    .expect("second websocket payload should be json");
                    second_websocket
                        .send(Message::Text(
                            response_completed_websocket_message("resp_pool_second").into(),
                        ))
                        .await
                        .unwrap();
                    second_websocket.close(None).await.unwrap();
                    break (false, first_payload, second_payload);
                }
            }
        }
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{
                            "role": "user",
                            "content": "reuse this upstream websocket"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert!(response_text(first_response)
        .await
        .contains("\"id\":\"resp_pool_first\""));

    let second_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "previous_response_id": "resp_pool_first"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    assert!(response_text(second_response)
        .await
        .contains("\"id\":\"resp_pool_second\""));
    let (reused_connection, first_payload, second_payload) = upstream.await.unwrap();

    assert!(reused_connection, "second request opened a new websocket");
    assert_eq!(
        second_payload["prompt_cache_key"], first_payload["prompt_cache_key"],
        "pooled websocket reuse should stay on the recorded conversation key"
    );
    assert_eq!(second_payload["previous_response_id"], "resp_pool_first");
}

#[tokio::test]
async fn responses_websocket_should_not_reuse_connection_when_pool_is_disabled() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_payload = serde_json::from_str::<Value>(&first_message.into_text().unwrap())
            .expect("first websocket payload should be json");
        websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_disabled_pool_first").into(),
            ))
            .await
            .unwrap();

        loop {
            tokio::select! {
                message = websocket.next() => {
                    match message {
                        Some(Ok(message)) if message.is_text() => {
                            let second_payload = serde_json::from_str::<Value>(
                                &message.into_text().unwrap(),
                            )
                            .expect("second websocket payload should be json");
                            websocket
                                .send(Message::Text(
                                    response_completed_websocket_message(
                                        "resp_disabled_pool_second",
                                    )
                                    .into(),
                                ))
                                .await
                                .unwrap();
                            websocket.close(None).await.unwrap();
                            break (true, first_payload, second_payload);
                        }
                        Some(_) => continue,
                        None => {
                            let second_payload = accept_successful_websocket_response(
                                &listener,
                                "resp_disabled_pool_second",
                            )
                            .await;
                            break (false, first_payload, second_payload);
                        }
                    }
                }
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let mut second_websocket = accept_async(stream).await.unwrap();
                    let second_message = second_websocket.next().await.unwrap().unwrap();
                    let second_payload = serde_json::from_str::<Value>(
                        &second_message.into_text().unwrap(),
                    )
                    .expect("second websocket payload should be json");
                    second_websocket
                        .send(Message::Text(
                            response_completed_websocket_message("resp_disabled_pool_second")
                                .into(),
                        ))
                        .await
                        .unwrap();
                    second_websocket.close(None).await.unwrap();
                    break (false, first_payload, second_payload);
                }
            }
        }
    });
    let (app, api_key, _pool, _dir) = test_app_with_account_pool_config(base_url, |config| {
        config.ws_pool.enabled = false;
    })
    .await;

    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{
                            "role": "user",
                            "content": "do not reuse this upstream websocket"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert!(response_text(first_response)
        .await
        .contains("\"id\":\"resp_disabled_pool_first\""));

    let second_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "previous_response_id": "resp_disabled_pool_first"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    assert!(response_text(second_response)
        .await
        .contains("\"id\":\"resp_disabled_pool_second\""));
    let (reused_connection, first_payload, second_payload) = upstream.await.unwrap();

    assert!(
        !reused_connection,
        "disabled pool reused the upstream websocket"
    );
    assert_eq!(
        second_payload["prompt_cache_key"], first_payload["prompt_cache_key"],
        "disabling the pool must not change the recorded conversation key"
    );
    assert_eq!(
        second_payload["previous_response_id"],
        "resp_disabled_pool_first"
    );
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn responses_websocket_stream_should_record_metadata_turn_state_for_continuation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_websocket = accept_async(first_stream).await.unwrap();
        let first_message = first_websocket.next().await.unwrap().unwrap();
        let first_payload = serde_json::from_str::<Value>(&first_message.into_text().unwrap())
            .expect("first websocket payload should be json");
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.metadata",
                    "headers": {
                        "x-codex-turn-state": "turn-from-metadata"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        first_websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_metadata_turn_state").into(),
            ))
            .await
            .unwrap();
        first_websocket.close(None).await.unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        let second_headers = Arc::new(Mutex::new(Vec::new()));
        let second_headers_for_callback = Arc::clone(&second_headers);
        let mut second_websocket = accept_hdr_async(
            second_stream,
            move |request: &WsRequest, response: WsResponse| {
                *second_headers_for_callback.lock().unwrap() = request_headers(request);
                Ok(response)
            },
        )
        .await
        .unwrap();
        let second_message = second_websocket.next().await.unwrap().unwrap();
        let second_payload = serde_json::from_str::<Value>(&second_message.into_text().unwrap())
            .expect("second websocket payload should be json");
        second_websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_metadata_turn_state_next").into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
        let second_headers = second_headers.lock().unwrap().clone();
        (first_payload, second_payload, second_headers)
    });
    let (app, api_key, _pool, _dir) = test_app_with_account_pool_config(base_url, |config| {
        config.ws_pool.enabled = false;
    })
    .await;

    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": true,
                        "use_websocket": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = response_text(first_response).await;
    assert!(first_body.contains("\"id\":\"resp_metadata_turn_state\""));
    assert!(!first_body.contains("response.metadata"));

    let second_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "previous_response_id": "resp_metadata_turn_state"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    assert!(response_text(second_response)
        .await
        .contains("\"id\":\"resp_metadata_turn_state_next\""));
    let (first_payload, second_payload, second_headers) = upstream.await.unwrap();

    assert!(first_payload.get("previous_response_id").is_none());
    assert_eq!(
        second_payload["previous_response_id"],
        "resp_metadata_turn_state"
    );
    assert_eq!(
        captured_header(&second_headers, "x-codex-turn-state"),
        Some("turn-from-metadata")
    );
}

#[tokio::test]
async fn responses_websocket_should_implicitly_resume_full_history_with_reasoning_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().to_string(),
        )
        .await;
        let second_payload = accept_followup_websocket_response(
            &listener,
            &mut websocket,
            websocket_completed_response("resp_implicit_resume_second", 4, 1),
        )
        .await;
        let _ = websocket.close(None).await;
        (first_payload, second_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [{"role": "user", "content": "remember this"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [
                    {"role": "user", "content": "remember this"},
                    {"role": "assistant", "content": "cached answer"},
                    {"role": "user", "content": "continue"}
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_payload, mut second_payload) = upstream.await.unwrap();
    assert!(first_payload["prompt_cache_key"].as_str().is_some());
    assert_eq!(
        second_payload["previous_response_id"],
        "resp_implicit_resume_first"
    );
    assert_eq!(
        second_payload["prompt_cache_key"],
        first_payload["prompt_cache_key"]
    );
    assert_eq!(
        second_payload["input"][0]["encrypted_content"],
        "enc_reasoning_replay"
    );
    assert_eq!(second_payload["input"][1]["content"], "continue");
    assert_eq!(second_payload["input"].as_array().unwrap().len(), 2);

    second_payload
        .as_object_mut()
        .unwrap()
        .remove("prompt_cache_key");
    second_payload
        .as_object_mut()
        .unwrap()
        .remove("client_metadata");
    let expected: Value = serde_json::from_str(REASONING_REPLAY_REQUEST_GOLDEN).unwrap();
    assert_eq!(second_payload, expected);
}

#[tokio::test]
async fn responses_websocket_should_not_implicitly_resume_unmatched_function_call_output() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            websocket_completed_function_call_response("resp_call_first", "call_expected"),
        )
        .await;
        let second_payload = accept_followup_websocket_response(
            &listener,
            &mut websocket,
            websocket_completed_response("resp_call_mismatch_second", 4, 1),
        )
        .await;
        let _ = websocket.close(None).await;
        (first_payload, second_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "use the lookup tool",
                "stream": false,
                "input": [{"role": "user", "content": "call the lookup tool"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "use the lookup tool",
                "stream": false,
                "input": [
                    {"role": "user", "content": "call the lookup tool"},
                    {
                        "type": "function_call",
                        "call_id": "call_expected",
                        "name": "lookup",
                        "arguments": "{}"
                    },
                    {
                        "type": "function_call_output",
                        "call_id": "call_missing",
                        "output": "tool output"
                    }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_payload, second_payload) = upstream.await.unwrap();
    assert!(first_payload["prompt_cache_key"].as_str().is_some());
    assert!(second_payload.get("previous_response_id").is_none());
    assert_eq!(second_payload["input"].as_array().unwrap().len(), 3);
    assert_eq!(second_payload["input"][2]["call_id"], "call_missing");
}

#[tokio::test]
async fn responses_websocket_should_implicitly_resume_after_sqlite_affinity_restore() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server_base_url = base_url.clone();
    let upstream = tokio::spawn(async move {
        let first_payload = accept_websocket_response_with_message(
            &listener,
            WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().to_string(),
        )
        .await;
        let second_payload = accept_websocket_response_with_message(
            &listener,
            websocket_completed_response("resp_restored_implicit_resume", 4, 1),
        )
        .await;
        (first_payload, second_payload)
    });
    let (app, api_key, pool, dir) = test_app_with_account_pool_config(base_url, |_| {}).await;

    let first_response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [{"role": "user", "content": "remember this"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let db = dir.path().join("openai-record-affinity.sqlite");
    let restored_state = AppState::with_pool_secret_api_key_hasher_and_installation_id(
        test_config(format!("sqlite://{}", db.display()), server_base_url),
        pool.clone(),
        SecretBox::new([83u8; 32]),
        ApiKeyHasher::new([84u8; 32]),
        TEST_INSTALLATION_ID.to_string(),
    );
    assert_eq!(
        restored_state
            .restore_account_pool_from_repository()
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        restored_state
            .restore_session_affinity_from_repository_now()
            .await
            .unwrap(),
        1
    );
    let restored_app = router::router().with_state(restored_state);

    let second_response = restored_app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [
                    {"role": "user", "content": "remember this"},
                    {"role": "assistant", "content": "cached answer"},
                    {"role": "user", "content": "continue"}
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_payload, second_payload) = upstream.await.unwrap();
    assert_eq!(
        second_payload["prompt_cache_key"],
        first_payload["prompt_cache_key"]
    );
    assert_eq!(
        second_payload["previous_response_id"],
        "resp_implicit_resume_first"
    );
    assert_eq!(second_payload["input"].as_array().unwrap().len(), 1);
    assert_eq!(second_payload["input"][0]["content"], "continue");
}

#[tokio::test]
async fn responses_websocket_pool_should_be_evicted_after_admin_account_status_cycle() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            websocket_completed_response("resp_pool_status_first", 4, 1),
        )
        .await;

        tokio::select! {
            message = websocket.next() => {
                match message {
                    Some(Ok(message)) if message.is_text() => {
                        websocket
                            .send(Message::Text(
                                websocket_completed_response("resp_pool_status_second", 3, 1).into(),
                            ))
                            .await
                            .unwrap();
                        let _ = websocket.close(None).await;
                        true
                    }
                    _ => {
                        accept_websocket_response_with_message(
                            &listener,
                            websocket_completed_response("resp_pool_status_second", 3, 1),
                        )
                        .await;
                        false
                    }
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.unwrap();
                let mut second_websocket = accept_async(stream).await.unwrap();
                send_websocket_response_and_capture_payload(
                    &mut second_websocket,
                    websocket_completed_response("resp_pool_status_second", 3, 1),
                )
                .await;
                second_websocket.close(None).await.unwrap();
                let _ = websocket.close(None).await;
                false
            }
        }
    });
    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(base_url).await;
    seed_openai_admin_session(&pool, "session_status_cycle").await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "prompt_cache_key": "status-cycle"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert!(response_text(first_response)
        .await
        .contains("\"id\":\"resp_pool_status_first\""));

    update_admin_account_status(&app, "acct_chat", "disabled").await;
    update_admin_account_status(&app, "acct_chat", "active").await;

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "previous_response_id": "resp_pool_status_first"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    assert!(response_text(second_response)
        .await
        .contains("\"id\":\"resp_pool_status_second\""));

    let reused_connection = upstream.await.unwrap();
    assert!(
        !reused_connection,
        "admin status lifecycle should evict the old pooled websocket"
    );
}

#[tokio::test]
async fn responses_websocket_should_not_implicitly_resume_self_contained_function_call_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            websocket_completed_function_call_response("resp_self_contained_first", "call_self"),
        )
        .await;
        let second_payload = accept_followup_websocket_response(
            &listener,
            &mut websocket,
            websocket_completed_response("resp_self_contained_second", 4, 1),
        )
        .await;
        let _ = websocket.close(None).await;
        (first_payload, second_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "use the lookup tool",
                "stream": false,
                "input": [{"role": "user", "content": "call the lookup tool"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "use the lookup tool",
                "stream": false,
                "input": [
                    {"role": "user", "content": "call the lookup tool"},
                    {
                        "type": "function_call",
                        "call_id": "call_self",
                        "name": "lookup",
                        "arguments": "{}"
                    },
                    {
                        "type": "function_call_output",
                        "call_id": "call_self",
                        "output": "tool output"
                    }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_payload, second_payload) = upstream.await.unwrap();
    assert!(first_payload["prompt_cache_key"].as_str().is_some());
    assert!(second_payload.get("previous_response_id").is_none());
    assert_eq!(second_payload["input"].as_array().unwrap().len(), 3);
    assert_eq!(second_payload["input"][2]["call_id"], "call_self");
}

#[tokio::test]
async fn responses_websocket_should_not_implicitly_resume_across_codex_windows() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().to_string(),
        )
        .await;
        let second_payload = accept_followup_websocket_response(
            &listener,
            &mut websocket,
            websocket_completed_response("resp_window_b", 8, 2),
        )
        .await;
        let _ = websocket.close(None).await;
        (first_payload, second_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "prompt_cache_key": "shared-variant-session",
                "codexWindowId": "window-a",
                "stream": false,
                "input": [{"role": "user", "content": "remember this"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "prompt_cache_key": "shared-variant-session",
                "codexWindowId": "window-b",
                "stream": false,
                "input": [
                    {"role": "user", "content": "remember this"},
                    {"role": "assistant", "content": "cached answer"},
                    {"role": "user", "content": "continue in another window"}
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_payload, second_payload) = upstream.await.unwrap();
    assert_eq!(
        second_payload["prompt_cache_key"],
        first_payload["prompt_cache_key"]
    );
    assert!(second_payload.get("previous_response_id").is_none());
    assert_eq!(second_payload["input"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn responses_websocket_should_evict_reasoning_replay_after_invalid_encrypted_content() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().to_string(),
        )
        .await;
        let invalid_payload = accept_followup_websocket_response(
            &listener,
            &mut websocket,
            WEBSOCKET_INVALID_ENCRYPTED_CONTENT.trim().to_string(),
        )
        .await;
        let _ = websocket.close(None).await;
        let retried_payload = accept_websocket_response_with_message(
            &listener,
            websocket_completed_response("resp_after_replay_eviction", 4, 1),
        )
        .await;
        (first_payload, invalid_payload, retried_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [{"role": "user", "content": "remember this"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [
                    {"role": "user", "content": "remember this"},
                    {"role": "assistant", "content": "cached answer"},
                    {"role": "user", "content": "continue"}
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_payload, invalid_payload, retried_payload) = upstream.await.unwrap();
    assert_eq!(
        invalid_payload["previous_response_id"],
        "resp_implicit_resume_first"
    );
    assert_eq!(
        invalid_payload["input"][0]["encrypted_content"],
        "enc_reasoning_replay"
    );
    assert_eq!(
        retried_payload["prompt_cache_key"],
        first_payload["prompt_cache_key"]
    );
    assert!(retried_payload.get("previous_response_id").is_none());
    let retried_input = retried_payload["input"].as_array().unwrap();
    assert!(retried_input
        .iter()
        .all(|item| item.get("encrypted_content").is_none()));
    assert_eq!(retried_input.last().unwrap()["content"], "continue");
}

#[tokio::test]
async fn responses_websocket_should_restore_full_history_when_implicit_resume_previous_response_is_missing(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().to_string(),
        )
        .await;
        let implicit_payload = accept_followup_websocket_response(
            &listener,
            &mut websocket,
            WEBSOCKET_PREVIOUS_RESPONSE_NOT_FOUND.trim().to_string(),
        )
        .await;
        let _ = websocket.close(None).await;
        let restored_payload = accept_websocket_response_with_message(
            &listener,
            websocket_completed_response("resp_implicit_resume_restored", 10, 2),
        )
        .await;
        (first_payload, implicit_payload, restored_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [{"role": "user", "content": "remember this"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [
                    {"role": "user", "content": "remember this"},
                    {"role": "assistant", "content": "cached answer"},
                    {"role": "user", "content": "continue"}
                ]
            }),
        ))
        .await
        .unwrap();
    let body = response_json(second_response).await;
    assert_eq!(body["id"], "resp_implicit_resume_restored");

    let (_first_payload, implicit_payload, restored_payload) = upstream.await.unwrap();
    assert_eq!(
        implicit_payload["previous_response_id"],
        "resp_implicit_resume_first"
    );
    assert_eq!(implicit_payload["input"].as_array().unwrap().len(), 2);
    assert!(restored_payload.get("previous_response_id").is_none());
    assert_eq!(restored_payload["input"].as_array().unwrap().len(), 3);
    assert_eq!(restored_payload["input"][0]["role"], "user");
    assert_eq!(restored_payload["input"][1]["role"], "assistant");
    assert_eq!(restored_payload["input"][2]["content"], "continue");
}

#[tokio::test]
async fn responses_websocket_should_route_previous_response_id_to_recorded_account() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (_reused_connection, first_payload, second_payload) =
            accept_two_successful_websocket_responses_with_authorization(
                &listener,
                "Bearer access-primary",
                "resp_affinity_first",
                "resp_affinity_second",
            )
            .await;
        (first_payload, second_payload)
    });
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "input": [{
                    "role": "user",
                    "content": "keep this conversation on the same account"
                }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert!(response_text(first_response)
        .await
        .contains("\"id\":\"resp_affinity_first\""));
    let stored_affinity: (String, String, String, Option<i64>, String) = sqlx::query_as(
        "select account_id, conversation_id, function_call_ids_json, input_tokens, expires_at from session_affinities where response_id = ?",
    )
    .bind("resp_affinity_first")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored_affinity.0, "acct_primary");
    assert!(!stored_affinity.1.is_empty());
    assert_eq!(stored_affinity.2, "[]");
    assert_eq!(stored_affinity.3, Some(3));
    assert!(!stored_affinity.4.is_empty());

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "previous_response_id": "resp_affinity_first"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = response_text(second_response).await;
    assert!(
        second_body.contains("\"id\":\"resp_affinity_second\""),
        "{second_body}"
    );
    let (first_payload, second_payload) = upstream.await.unwrap();

    assert_ne!(first_payload["prompt_cache_key"], Value::Null);
    assert_eq!(
        second_payload["previous_response_id"],
        "resp_affinity_first"
    );
    assert_eq!(
        second_payload["prompt_cache_key"], first_payload["prompt_cache_key"],
        "previous_response_id should inherit the recorded conversation identity"
    );
}

#[tokio::test]
async fn responses_websocket_non_stream_previous_response_not_found_should_strip_history_and_retry_same_account(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let first_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            WEBSOCKET_PREVIOUS_RESPONSE_NOT_FOUND.trim().to_string(),
        )
        .await;
        let second_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            websocket_completed_response("resp_after_history_strip", 3, 1),
        )
        .await;
        (first_payload, second_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": false,
                "previous_response_id": "resp_missing"
            }),
        ))
        .await
        .unwrap();
    let body = response_json(response).await;
    let (first_payload, second_payload) = upstream.await.unwrap();

    assert_eq!(body["id"], "resp_after_history_strip");
    assert_eq!(first_payload["previous_response_id"], "resp_missing");
    assert!(second_payload.get("previous_response_id").is_none());
}

#[tokio::test]
async fn responses_websocket_stream_previous_response_not_found_should_strip_history_and_retry_same_account(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let first_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            WEBSOCKET_PREVIOUS_RESPONSE_NOT_FOUND.trim().to_string(),
        )
        .await;
        let second_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            websocket_completed_response("resp_stream_after_history_strip", 3, 1),
        )
        .await;
        (first_payload, second_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "previous_response_id": "resp_missing"
            }),
        ))
        .await
        .unwrap();
    let body = response_text(response).await;
    let (first_payload, second_payload) = upstream.await.unwrap();

    assert!(body.contains("\"id\":\"resp_stream_after_history_strip\""));
    assert_eq!(first_payload["previous_response_id"], "resp_missing");
    assert!(second_payload.get("previous_response_id").is_none());
}

#[tokio::test]
async fn responses_websocket_non_stream_unanswered_function_call_should_strip_history_and_retry_same_account(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let first_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            WEBSOCKET_UNANSWERED_FUNCTION_CALL.trim().to_string(),
        )
        .await;
        let second_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            websocket_completed_response("resp_after_function_call_strip", 3, 1),
        )
        .await;
        (first_payload, second_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": false,
                "previous_response_id": "resp_with_call"
            }),
        ))
        .await
        .unwrap();
    let body = response_json(response).await;
    let (first_payload, second_payload) = upstream.await.unwrap();

    assert_eq!(body["id"], "resp_after_function_call_strip");
    assert_eq!(first_payload["previous_response_id"], "resp_with_call");
    assert!(second_payload.get("previous_response_id").is_none());
}

#[tokio::test]
async fn responses_websocket_previous_response_id_should_retry_fallback_account_after_429() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-primary",
            429,
            "Too Many Requests",
            Some(77),
            WEBSOCKET_HISTORY_RATE_LIMITED,
        )
        .await;
        accept_successful_websocket_response_with_authorization(
            &listener,
            "Bearer access-secondary",
            "resp_history_fallback",
        )
        .await
    });
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "previous_response_id": "resp_prev"
            }),
        ))
        .await
        .unwrap();
    let body = response_text(response).await;
    let fallback_payload = upstream.await.unwrap();
    let secondary_usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_secondary")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert!(body.contains("\"id\":\"resp_history_fallback\""));
    assert_eq!(fallback_payload["previous_response_id"], "resp_prev");
    assert_eq!(secondary_usage.0, 1);
}

#[tokio::test]
async fn responses_websocket_non_stream_previous_response_id_should_retry_fallback_account_after_429(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-primary",
            429,
            "Too Many Requests",
            Some(77),
            WEBSOCKET_HISTORY_RATE_LIMITED,
        )
        .await;
        accept_successful_websocket_response_with_authorization(
            &listener,
            "Bearer access-secondary",
            "resp_history_fallback_non_stream",
        )
        .await
    });
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": false,
                "previous_response_id": "resp_prev"
            }),
        ))
        .await
        .unwrap();
    let body = response_json(response).await;
    let fallback_payload = upstream.await.unwrap();
    let secondary_usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_secondary")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(body["id"], "resp_history_fallback_non_stream");
    assert_eq!(fallback_payload["previous_response_id"], "resp_prev");
    assert_eq!(secondary_usage.0, 1);
}

#[tokio::test]
async fn responses_websocket_without_history_should_mark_expired_after_fallback_401() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-primary",
            429,
            "Too Many Requests",
            Some(30),
            WEBSOCKET_RATE_LIMITED,
        )
        .await;
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-secondary",
            401,
            "Unauthorized",
            None,
            WEBSOCKET_TOKEN_REVOKED,
        )
        .await;
    });
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "use_websocket": true
            }),
        ))
        .await
        .unwrap();
    let body = response_text(response).await;
    upstream.await.unwrap();
    let secondary_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_secondary")
        .fetch_one(&pool)
        .await
        .unwrap();
    let primary_usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_primary")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(body.contains("event: response.failed"));
    assert!(body.contains("\"type\":\"invalid_request_error\""));
    assert!(body.contains("\"code\":\"authentication_error\""));
    assert!(body.contains("All accounts exhausted"));
    assert!(body.contains("token_revoked"));
    assert_eq!(secondary_status.0, "expired");
    assert_eq!(primary_usage, (1, 0, 0));
}

#[tokio::test]
async fn responses_websocket_without_history_should_return_rate_limit_stream_error_when_fallback_accounts_exhausted(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-primary",
            429,
            "Too Many Requests",
            Some(11),
            WEBSOCKET_FIRST_ACCOUNT_LIMITED,
        )
        .await;
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-secondary",
            429,
            "Too Many Requests",
            Some(22),
            WEBSOCKET_SECOND_ACCOUNT_LIMITED,
        )
        .await;
    });
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "use_websocket": true
            }),
        ))
        .await
        .unwrap();
    let body = response_text(response).await;
    upstream.await.unwrap();
    let primary_usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_primary")
            .fetch_one(&pool)
            .await
            .unwrap();
    let secondary_usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_secondary")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_response_failed_stream(
        &body,
        "rate_limit_error",
        "rate_limit_exceeded",
        &[
            "All accounts exhausted (2 rate-limited)",
            "second account limited",
        ],
    );
    assert_eq!(primary_usage.0, 1);
    assert_eq!(secondary_usage.0, 1);
}

#[tokio::test]
async fn responses_websocket_response_failed_quota_should_retry_fallback_account() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_websocket =
            accept_websocket_with_authorization(first_stream, "Bearer access-primary").await;
        let _first_payload = send_websocket_response_and_capture_payload(
            &mut first_websocket,
            json!({
                "type": "response.failed",
                "response": {
                    "id": "resp_ws_quota_failed",
                    "error": {
                        "code": "insufficient_quota",
                        "message": "quota exhausted"
                    }
                }
            })
            .to_string(),
        )
        .await;

        let (second_stream, _) = listener.accept().await.unwrap();
        let mut second_websocket =
            accept_websocket_with_authorization(second_stream, "Bearer access-secondary").await;
        send_websocket_response_and_capture_payload(
            &mut second_websocket,
            websocket_completed_response("resp_after_ws_quota", 3, 1),
        )
        .await;
        second_websocket.close(None).await.unwrap();
    });
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": false,
                "use_websocket": true
            }),
        ))
        .await
        .unwrap();
    let body = response_json(response).await;
    upstream.await.unwrap();
    let primary_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(body["id"], "resp_after_ws_quota");
    assert_eq!(primary_status.0, "quota_exhausted");
}

#[tokio::test]
async fn responses_websocket_without_history_should_return_quota_stream_error_when_402_has_no_fallback(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-secret",
            402,
            "Payment Required",
            None,
            r#"{"error":{"message":"quota reached"}}"#,
        )
        .await;
    });
    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "use_websocket": true
            }),
        ))
        .await
        .unwrap();
    let body = response_text(response).await;
    upstream.await.unwrap();
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_response_failed_stream(
        &body,
        "invalid_request_error",
        "codex_api_error",
        &[
            "All accounts exhausted (1 quota-exhausted)",
            "quota reached",
        ],
    );
    assert_eq!(account_status.0, "quota_exhausted");
}

#[tokio::test]
async fn responses_websocket_without_history_should_return_model_unsupported_stream_error_when_no_fallback(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-secret",
            400,
            "Bad Request",
            None,
            r#"{"error":{"code":"model_not_available","message":"Model gpt-5.5 is not available on this account plan"}}"#,
        )
        .await;
    });
    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "use_websocket": true
            }),
        ))
        .await
        .unwrap();
    let body = response_text(response).await;
    upstream.await.unwrap();
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_response_failed_stream(
        &body,
        "invalid_request_error",
        "codex_api_error",
        &[
            "No accounts available",
            "model_not_available",
            "not available",
        ],
    );
    assert_eq!(account_status.0, "active");
}

#[tokio::test]
async fn responses_websocket_with_history_should_return_path_block_stream_error_when_no_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-secret",
            404,
            "Not Found",
            None,
            "",
        )
        .await;
    });
    let (app, api_key, _pool, _dir) = test_app_with_account_and_pool(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "previous_response_id": "resp_prev"
            }),
        ))
        .await
        .unwrap();
    let body = response_text(response).await;
    upstream.await.unwrap();

    assert_response_failed_stream(
        &body,
        "server_error",
        "codex_api_error",
        &["No accounts available", "Cloudflare path-block"],
    );
}

#[tokio::test]
async fn responses_should_convert_tuple_schema_before_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_TUPLE_OBJECT_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(tuple_response_request_body(false)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();

    assert_eq!(status, StatusCode::OK);
    let requests = server.received_requests().await.unwrap();
    let upstream_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let schema = &upstream_body["text"]["format"]["schema"];
    assert!(schema["properties"]["point"].get("prefixItems").is_none());
    assert!(schema["properties"]["point"].get("items").is_none());
    assert_eq!(
        schema["properties"]["point"],
        json!({
            "type": "object",
            "properties": {
                "0": {"type": "number"},
                "1": {"type": "number"}
            },
            "required": ["0", "1"],
            "additionalProperties": false
        })
    );
}

#[tokio::test]
async fn responses_should_reconvert_tuple_schema_output_for_client() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_TUPLE_OBJECT_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(tuple_response_request_body(false)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_tuple");
    assert_eq!(body["output_text"], "{\"point\":[1,2]}");
    assert_eq!(body["output"][0]["content"][0]["text"], "{\"point\":[1,2]}");
}

#[tokio::test]
async fn responses_stream_should_reconvert_tuple_schema_output_for_client() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_TUPLE_OBJECT_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(tuple_response_request_body(true)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r#""delta":"{\"point\":[1,2]}""#));
    assert!(body.contains(r#""output_text":"{\"point\":[1,2]}""#));
    assert!(!body.contains(r#""point\":{\"0":1,"1":2}"#));
    assert!(body.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn responses_should_forward_parity_fields_context_headers_and_account_scoped_identity() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-codex-turn-state", "turn-header")
                .header("x-codex-turn-metadata", "meta-header")
                .header("x-codex-beta-features", "beta-header")
                .header("x-responsesapi-include-timing-metrics", "false")
                .header("version", "header-version")
                .header("x-codex-window-id", "window-header")
                .header("x-codex-parent-thread-id", "parent-header")
                .header("x-openai-subagent", "review")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5-high-fast",
                        "stream": false,
                        "use_websocket": false,
                        "input": [],
                        "prompt_cache_key": "pcache",
                        "client_metadata": {"safe": "yes", "drop": 42},
                        "turnState": "turn-body",
                        "turnMetadata": "meta-body",
                        "betaFeatures": "beta-body",
                        "includeTimingMetrics": "true",
                        "version": "2026-06-12",
                        "codexWindowId": "window-body",
                        "parentThreadId": "parent-body",
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_response_1");
    let requests = server.received_requests().await.unwrap();
    let upstream = requests
        .iter()
        .find(|request| request.url.path() == "/codex/responses")
        .expect("responses upstream request should be sent");
    let upstream_body: Value = serde_json::from_slice(&upstream.body).unwrap();
    let identity = build_conversation_identity(Some("pcache"), Some("window-body"), "acct_chat");
    let conversation_id = identity
        .conversation_id
        .as_deref()
        .expect("conversation identity should be scoped");
    let window_id = identity
        .window_id
        .as_deref()
        .expect("window identity should be scoped");
    let upstream_header = |name: &str| {
        upstream
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
    };

    assert_eq!(upstream_body["model"], "gpt-5.5");
    assert_eq!(upstream_body["service_tier"], "priority");
    assert_eq!(
        upstream_body["reasoning"],
        json!({"summary": "auto", "effort": "high"})
    );
    assert_eq!(
        upstream_body["include"],
        json!(["reasoning.encrypted_content"])
    );
    assert_eq!(upstream_body["prompt_cache_key"], conversation_id);
    assert_eq!(upstream_body["client_metadata"]["safe"], "yes");
    assert_eq!(
        upstream_body["client_metadata"]["x-openai-subagent"],
        "review"
    );
    assert_eq!(
        upstream_body["client_metadata"]["x-codex-installation-id"],
        TEST_INSTALLATION_ID
    );
    assert_eq!(
        upstream_body["client_metadata"]["x-codex-window-id"],
        window_id
    );
    assert_eq!(
        upstream_body["client_metadata"]["x-codex-turn-metadata"],
        "meta-body"
    );
    assert_eq!(
        upstream_body["client_metadata"]["x-codex-parent-thread-id"],
        "parent-body"
    );
    assert_eq!(upstream_header("session_id"), Some(conversation_id));
    assert_eq!(upstream_header("x-codex-window-id"), Some(window_id));
    assert_eq!(upstream_header("x-codex-turn-state"), Some("turn-body"));
    assert_eq!(upstream_header("x-codex-turn-metadata"), Some("meta-body"));
    assert_eq!(upstream_header("x-codex-beta-features"), Some("beta-body"));
    assert_eq!(
        upstream_header("x-responsesapi-include-timing-metrics"),
        Some("true")
    );
    assert_eq!(upstream_header("version"), Some("2026-06-12"));
    assert_eq!(
        upstream_header("x-codex-parent-thread-id"),
        Some("parent-body")
    );
    assert_eq!(upstream_header("x-openai-subagent"), Some("review"));
    for local_field in [
        "turnState",
        "turnMetadata",
        "betaFeatures",
        "includeTimingMetrics",
        "version",
        "codexWindowId",
        "parentThreadId",
        "use_websocket",
    ] {
        assert!(upstream_body.get(local_field).is_none());
    }
}

#[tokio::test]
async fn responses_should_preserve_non_empty_include_when_reasoning_defaults_apply() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5-high",
                        "stream": false,
                        "use_websocket": false,
                        "input": [],
                        "include": ["file_search_call.results"],
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = server.received_requests().await.unwrap();
    let upstream_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        upstream_body["reasoning"],
        json!({"summary": "auto", "effort": "high"})
    );
    assert_eq!(
        upstream_body["include"],
        json!(["file_search_call.results"])
    );
}

#[tokio::test]
async fn responses_should_sanitize_reasoning_and_compaction_input_before_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_COMPLETED_USAGE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "stream": false,
                        "use_websocket": false,
                        "input": [
                            {
                                "type": "reasoning",
                                "id": "rs_1",
                                "status": "completed",
                                "summary": [
                                    {"type": "summary_text", "text": "valid summary"},
                                    {"type": "ignored", "text": "drop"}
                                ],
                                "encrypted_content": "enc_reasoning",
                                "content": [
                                    {"type": "reasoning_text", "text": "valid reasoning"},
                                    {"type": "ignored", "text": "drop"}
                                ],
                                "extra": "drop"
                            },
                            {
                                "type": "reasoning",
                                "id": "",
                                "summary": [{"type": "summary_text", "text": "drop"}]
                            },
                            {
                                "type": "compaction",
                                "id": "cmp_1",
                                "encrypted_content": "enc_compaction",
                                "extra": "drop"
                            },
                            {"type": "compaction", "id": "cmp_drop"},
                            {"type": "message", "role": "user", "content": "keep me", "extra": 42}
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = server.received_requests().await.unwrap();
    let upstream_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        upstream_body["input"],
        json!([
            {
                "type": "reasoning",
                "id": "rs_1",
                "summary": [{"type": "summary_text", "text": "valid summary"}],
                "status": "completed",
                "encrypted_content": "enc_reasoning",
                "content": [{"type": "reasoning_text", "text": "valid reasoning"}]
            },
            {
                "type": "compaction",
                "encrypted_content": "enc_compaction",
                "id": "cmp_1"
            },
            {"type": "message", "role": "user", "content": "keep me", "extra": 42}
        ])
    );
}

#[tokio::test]
async fn responses_should_reconstruct_non_stream_output_text_from_sse_deltas() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_TEXT_DELTAS_COMPLETED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;

    assert_eq!(body["id"], "resp_text");
    assert_eq!(body["output"][0]["role"], "assistant");
    assert_eq!(body["output"][0]["content"][0]["text"], "hello from deltas");
    assert_eq!(body["output_text"], "hello from deltas");
}

#[tokio::test]
async fn responses_should_use_done_output_items_when_completed_output_is_empty() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_DONE_ITEM_COMPLETED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;

    assert_eq!(body["id"], "resp_item");
    assert_eq!(body["output"][0]["content"][0]["text"], "from done item");
    assert_eq!(body["output_text"], "from done item");
}

#[tokio::test]
async fn responses_review_route_should_force_review_subagent_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("x-openai-subagent", "review"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/review")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_response_1");
    let requests = server.received_requests().await.unwrap();
    let upstream_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        upstream_body["client_metadata"]["x-openai-subagent"],
        "review"
    );
}

#[tokio::test]
async fn responses_compact_should_post_json_to_codex_compact_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses/compact"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .and(header("content-type", "application/json"))
        .and(header("openai-beta", "responses_websockets=2026-02-06"))
        .and(header("x-openai-internal-codex-residency", "us"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "compacted"}]
                }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_account(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/compact")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5-fast",
                        "instructions": "compress the session",
                        "input": [
                            {"role": "user", "content": "hello"},
                            {
                                "type": "reasoning",
                                "id": "rs_1",
                                "status": "completed",
                                "summary": [{"type": "summary_text", "text": "kept"}],
                                "ignored": "drop"
                            },
                            {"type": "compaction", "encrypted_content": "enc_compact"},
                            {"type": "compaction", "id": "drop_missing_encrypted"}
                        ],
                        "tools": [{"type": "function", "name": "lookup"}],
                        "parallel_tool_calls": false,
                        "reasoning": {"effort": "high", "summary": "auto", "extra": "drop"},
                        "text": {
                            "format": {
                                "type": "json_schema",
                                "name": "Compact",
                                "schema": {"type": "object"},
                                "strict": true
                            }
                        },
                        "stream": true,
                        "use_websocket": false,
                        "store": true,
                        "prompt_cache_key": "must_not_forward"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["output"][0]["content"][0]["text"], "compacted");
    let requests = server.received_requests().await.unwrap();
    let compact_request = requests
        .iter()
        .find(|request| request.url.path() == "/codex/responses/compact")
        .expect("compact upstream request should be sent");
    assert_ne!(
        compact_request
            .headers
            .get("accept")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    assert!(compact_request
        .headers
        .get("x-codex-installation-id")
        .is_some());
    let upstream_body: Value = serde_json::from_slice(&compact_request.body).unwrap();
    assert_eq!(upstream_body["model"], "gpt-5.5");
    assert_eq!(upstream_body["instructions"], "compress the session");
    assert_eq!(upstream_body["parallel_tool_calls"], false);
    assert_eq!(
        upstream_body["reasoning"],
        json!({"effort": "high", "summary": "auto"})
    );
    assert_eq!(
        upstream_body["tools"],
        json!([{"type": "function", "name": "lookup"}])
    );
    assert_eq!(upstream_body["text"]["format"]["type"], "json_schema");
    assert!(upstream_body.get("stream").is_none());
    assert!(upstream_body.get("store").is_none());
    assert!(upstream_body.get("prompt_cache_key").is_none());
    assert_eq!(upstream_body["input"].as_array().unwrap().len(), 3);
    assert!(upstream_body["input"][1].get("ignored").is_none());
    assert_eq!(
        upstream_body["input"][2]["encrypted_content"],
        "enc_compact"
    );
}

#[tokio::test]
async fn responses_compact_should_return_rate_limit_error_when_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses/compact"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "code": "rate_limit_exceeded",
                "message": "compact quota reached"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/compact")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let message = body["error"]["message"].as_str().unwrap_or_default();

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "rate_limit_error");
    assert_eq!(body["error"]["code"], "rate_limit_exceeded");
    assert!(message.contains("All accounts exhausted (1 rate-limited)"));
    assert!(message.contains("compact quota reached"));
}

#[tokio::test]
async fn responses_compact_should_return_responses_error_when_no_accounts_are_available() {
    let server = MockServer::start().await;
    let (app, api_key, _dir) = test_app_without_accounts(server.uri()).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/compact")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "hello"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "server_error");
    assert_eq!(body["error"]["code"], "no_available_accounts");
}

#[tokio::test]
async fn responses_should_use_imported_account_record_usage_cookie_and_event_log() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header(
                    "set-cookie",
                    "cf_clearance=new; Domain=.chatgpt.com; Path=/",
                )
                .set_body_string(RESPONSES_COMPLETED_USAGE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_logging(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-request-id", "req_non_stream_usage_log")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_usage");
    assert_eq!(body["usage"]["input_tokens"], 7);
    let usage: (i64, i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens, cached_tokens from account_usage where account_id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(usage, (1, 7, 4, 2));
    let cookie_header = SqliteCookieStore::new(pool.clone(), SecretBox::new([83u8; 32]))
        .cookie_header("acct_chat", "chatgpt.com")
        .await
        .unwrap();
    assert_eq!(cookie_header.as_deref(), Some("cf_clearance=new"));
    let event = latest_response_event_log(&pool).await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();
    assert_eq!(event.level, "info");
    assert_eq!(
        event.request_id.as_deref(),
        Some("req_non_stream_usage_log")
    );
    assert_eq!(event.account_id.as_deref(), Some("acct_chat"));
    assert_eq!(event.status_code, Some(200));
    assert_eq!(metadata["stream"], false);
    assert_eq!(metadata["usage"]["inputTokens"], 7);
}

#[tokio::test]
async fn responses_should_skip_event_log_when_logging_disabled() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_COMPLETED_USAGE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-request-id", "req_non_stream_no_log")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_event_log_count(&pool).await, 0);
}

#[tokio::test]
async fn responses_should_record_image_generation_usage_when_tool_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_COMPLETED_IMAGE_USAGE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false,
                        "tools": [{"type": "image_generation"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let usage: (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64) =
        sqlx::query_as(
            "select request_count, input_tokens, output_tokens, cached_tokens, image_input_tokens, image_output_tokens, image_request_count, image_request_failed_count, window_image_input_tokens, window_image_output_tokens, window_image_request_count, window_image_request_failed_count from account_usage where account_id = ?",
        )
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(usage, (1, 7, 4, 2, 31, 9, 1, 0, 31, 9, 1, 0));
}

#[tokio::test]
async fn responses_should_record_failed_image_generation_attempt_when_tool_has_no_output() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_COMPLETED_USAGE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false,
                        "tools": [{"type": "image_generation"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let usage: (i64, i64, i64, i64) = sqlx::query_as(
        "select image_request_count, image_request_failed_count, window_image_request_count, window_image_request_failed_count from account_usage where account_id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(usage, (0, 1, 0, 1));
}

#[tokio::test]
async fn responses_should_scope_upstream_cookie_by_codex_response_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_COMPLETED_USAGE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let cookie_store = SqliteCookieStore::new(pool.clone(), SecretBox::new([83u8; 32]));
    cookie_store
        .capture_set_cookie(
            "acct_chat",
            "cf_clearance=root; Domain=.chatgpt.com; Path=/",
        )
        .await
        .unwrap();
    cookie_store
        .capture_set_cookie(
            "acct_chat",
            "cf_clearance=codex; Domain=.chatgpt.com; Path=/codex",
        )
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = server.received_requests().await.unwrap();
    let cookie_header = requests
        .iter()
        .find(|request| request.url.path() == "/codex/responses")
        .and_then(|request| request.headers.get("cookie"))
        .and_then(|value| value.to_str().ok());
    assert_eq!(cookie_header, Some("cf_clearance=codex; cf_clearance=root"));
}

#[tokio::test]
async fn responses_should_passively_cache_rate_limit_headers() {
    let server = MockServer::start().await;
    let reset_at = Utc::now().timestamp() + 300;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-codex-primary-used-percent", "100")
                .insert_header("x-codex-primary-window-minutes", "5")
                .insert_header("x-codex-primary-reset-at", reset_at.to_string())
                .set_body_string(RESPONSES_COMPLETED_USAGE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    sqlx::query(
        r#"update accounts set quota_json = '{"credits":{"has_credits":true,"unlimited":false,"balance":12}}', quota_verify_required = 1 where id = ?"#,
    )
    .bind("acct_chat")
    .execute(&pool)
    .await
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let stored: (String, i64, Option<String>, i64) = sqlx::query_as(
        "select quota_json, quota_limit_reached, quota_cooldown_until, quota_verify_required from accounts where id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();
    let quota: Value = serde_json::from_str(&stored.0).unwrap();
    assert_eq!(quota["rate_limit"]["limit_reached"], true);
    assert_eq!(quota["rate_limit"]["reset_at"], reset_at);
    assert_eq!(quota["credits"]["balance"], 12);
    assert_eq!(stored.1, 1);
    assert!(stored.2.is_some());
    assert_eq!(stored.3, 0);
    let window: (i64, i64, i64, i64, Option<String>, Option<String>, Option<i64>) =
        sqlx::query_as(
            "select window_request_count, window_input_tokens, window_output_tokens, window_cached_tokens, window_started_at, window_reset_at, limit_window_seconds from account_usage where account_id = ?",
        )
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(window.0, 1);
    assert_eq!(window.1, 7);
    assert_eq!(window.2, 4);
    assert_eq!(window.3, 2);
    assert!(window.4.is_some());
    assert_eq!(
        DateTime::parse_from_rfc3339(window.5.as_deref().unwrap())
            .unwrap()
            .timestamp(),
        reset_at
    );
    assert_eq!(window.6, Some(300));
}

#[tokio::test]
async fn responses_should_record_empty_response_attempts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_EMPTY_COMPLETED_SSE),
        )
        .expect(3)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let usage: (i64, i64, i64, i64) = sqlx::query_as(
        "select request_count, empty_response_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(usage, (3, 3, 0, 0));
}

#[tokio::test]
async fn responses_should_fallback_to_next_account_after_rate_limit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "120")
                .set_body_json(json!({
                    "error": {
                        "message": "rate limited",
                        "resets_in_seconds": 120
                    }
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_response_1");
    let primary_usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_primary")
    .fetch_one(&pool)
    .await
    .unwrap();
    let secondary_usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_secondary")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(primary_usage, (1, 0, 0));
    assert_eq!(secondary_usage, (1, 5, 2));
}

#[tokio::test]
async fn responses_should_mark_expired_after_401_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_revoked", "message": "token revoked"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_AFTER_401_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_401");
    assert_eq!(account_status.0, "expired");
}

#[tokio::test]
async fn responses_should_recover_auth_failure_from_sse_failed_event() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_FAILED_AUTH_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_AFTER_401_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_401");
    assert_eq!(account_status.0, "expired");
}

#[tokio::test]
async fn responses_should_return_auth_error_when_401_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_revoked", "message": "token revoked"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let message = body["error"]["message"].as_str().unwrap_or_default();
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(message.contains("All accounts exhausted (1 expired)"));
    assert!(message.contains("token_revoked"));
    assert_eq!(body["error"]["code"], "upstream_error");
    assert_eq!(account_status.0, "expired");
}

#[tokio::test]
async fn responses_should_mark_banned_when_401_says_account_deactivated() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "account deactivated"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(account_status.0, "banned");
}

#[tokio::test]
async fn responses_should_mark_banned_after_403_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"message": "account banned"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_AFTER_403_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_403");
    assert_eq!(account_status.0, "banned");
}

#[tokio::test]
async fn responses_should_return_cloudflare_challenge_error_when_403_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<html><title>Just a moment...</title><body>cf_chl challenge</body></html>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let cookie_store = SqliteCookieStore::new(pool.clone(), SecretBox::new([83u8; 32]));
    cookie_store
        .set_cookie_header("acct_chat", "cf_clearance=old")
        .await
        .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let message = body["error"]["message"].as_str().unwrap_or_default();
    let primary_state: (String, Option<String>) =
        sqlx::query_as("select status, cloudflare_cooldown_until from accounts where id = ?")
            .bind("acct_chat")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(message.contains("All accounts exhausted (1 cloudflare-challenge)"));
    assert!(message.contains("Cloudflare challenge"));
    assert_eq!(body["error"]["code"], "upstream_error");
    assert_eq!(primary_state.0, "active");
    assert!(primary_state.1.is_some());
    assert_eq!(
        cookie_store
            .cookie_header("acct_chat", "chatgpt.com")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn responses_should_cool_down_cloudflare_403_and_fallback() {
    let started_at = Utc::now();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<html><title>Just a moment...</title><body>cf_chl challenge</body></html>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_AFTER_CLOUDFLARE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let cookie_store = SqliteCookieStore::new(pool.clone(), SecretBox::new([83u8; 32]));
    cookie_store
        .set_cookie_header("acct_primary", "cf_clearance=old")
        .await
        .unwrap();
    cookie_store
        .set_cookie_header("acct_secondary", "cf_clearance=keep")
        .await
        .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let primary_state: (String, Option<String>) =
        sqlx::query_as("select status, cloudflare_cooldown_until from accounts where id = ?")
            .bind("acct_primary")
            .fetch_one(&pool)
            .await
            .unwrap();
    let cooldown_until = chrono::DateTime::parse_from_rfc3339(primary_state.1.as_deref().unwrap())
        .unwrap()
        .with_timezone(&Utc);

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_cloudflare");
    assert_eq!(primary_state.0, "active");
    assert!(cooldown_until > started_at);
    assert_eq!(
        cookie_store
            .cookie_header("acct_primary", "chatgpt.com")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        cookie_store
            .cookie_header("acct_secondary", "chatgpt.com")
            .await
            .unwrap()
            .as_deref(),
        Some("cf_clearance=keep")
    );
}

#[tokio::test]
async fn responses_should_clear_cookies_after_cloudflare_path_block_404_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_AFTER_CLOUDFLARE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let cookie_store = SqliteCookieStore::new(pool.clone(), SecretBox::new([83u8; 32]));
    cookie_store
        .set_cookie_header("acct_primary", "cf_clearance=old")
        .await
        .unwrap();
    cookie_store
        .set_cookie_header("acct_secondary", "cf_clearance=keep")
        .await
        .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let primary_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_cloudflare");
    assert_eq!(primary_status.0, "active");
    assert_eq!(
        cookie_store
            .cookie_header("acct_primary", "chatgpt.com")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        cookie_store
            .cookie_header("acct_secondary", "chatgpt.com")
            .await
            .unwrap()
            .as_deref(),
        Some("cf_clearance=keep")
    );
}

#[tokio::test]
async fn responses_should_disable_account_after_three_cloudflare_path_block_404s() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(404))
        .expect(3)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let cookie_store = SqliteCookieStore::new(pool.clone(), SecretBox::new([83u8; 32]));
    cookie_store
        .set_cookie_header("acct_chat", "cf_clearance=old")
        .await
        .unwrap();
    for _ in 0..3 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header("authorization", format!("Bearer {api_key}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "model": "gpt-5.5",
                            "input": [{"role": "user", "content": "Say hello"}],
                            "stream": false,
                        "use_websocket": false
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = response_json(response).await;
        let message = body["error"]["message"].as_str().unwrap_or_default();

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(message.contains("cloudflare-path-block"));
    }
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(account_status.0, "disabled");
    assert_eq!(
        cookie_store
            .cookie_header("acct_chat", "chatgpt.com")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn responses_should_fallback_after_http_model_unsupported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "code": "model_not_supported",
                "message": "Model gpt-5.5 is not supported on this account plan"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_AFTER_MODEL_UNSUPPORTED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let primary_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_model_unsupported");
    assert_eq!(primary_status.0, "active");
}

#[tokio::test]
async fn responses_should_fallback_after_sse_model_unsupported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_FAILED_MODEL_UNSUPPORTED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_AFTER_MODEL_UNSUPPORTED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_model_unsupported");
}

#[tokio::test]
async fn responses_stream_should_fallback_after_sse_model_unsupported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_FAILED_MODEL_UNSUPPORTED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_STREAM_AFTER_MODEL_UNSUPPORTED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("resp_stream_after_model_unsupported"));
}

#[tokio::test]
async fn responses_stream_should_return_model_unsupported_error_when_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "code": "model_not_available",
                "message": "Model gpt-5.5 is not available on this account plan"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("event: response.failed"));
    assert!(body.contains("All accounts exhausted (1 model-unsupported)"));
    assert!(body.contains("model_not_available"));
    assert_eq!(account_status.0, "active");
}

#[tokio::test]
async fn responses_should_strip_history_after_previous_response_not_found() {
    let (base_url, upstream) =
        spawn_websocket_failure_then_websocket_success_upstream(response_failed_websocket_message(
            "resp_previous_missing_failed",
            "previous_response_not_found",
            "Previous response with id resp_stale was not found",
        ))
        .await;
    let (app, api_key, _pool, _dir) = test_app_with_account_and_pool(base_url).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "previous_response_id": "resp_stale",
                        "turnState": "turn-stale",
                        "input": [{"role": "user", "content": "Continue"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let captured = upstream.await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_history_recovery");
    assert_eq!(
        captured_header(&captured.first_ws_headers, "x-codex-turn-state"),
        Some("turn-stale")
    );
    assert!(captured_header(&captured.second_ws_headers, "x-codex-turn-state").is_none());
    assert_eq!(
        captured.first_ws_payload["previous_response_id"],
        "resp_stale"
    );
    assert!(captured
        .second_ws_payload
        .get("previous_response_id")
        .is_none());
}

#[tokio::test]
async fn responses_should_strip_history_after_unanswered_function_call() {
    let (base_url, upstream) =
        spawn_websocket_failure_then_websocket_success_upstream(response_failed_websocket_message(
            "resp_unanswered_call_failed",
            "invalid_request",
            "No tool output found for function call call_1",
        ))
        .await;
    let (app, api_key, _pool, _dir) = test_app_with_account_and_pool(base_url).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "previous_response_id": "resp_stale",
                        "turnState": "turn-stale",
                        "input": [{"role": "user", "content": "Continue"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let captured = upstream.await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_history_recovery");
    assert!(captured_header(&captured.second_ws_headers, "x-codex-turn-state").is_none());
    assert!(captured
        .second_ws_payload
        .get("previous_response_id")
        .is_none());
}

#[tokio::test]
async fn responses_should_strip_history_after_websocket_invalid_encrypted_reasoning_replay() {
    let (base_url, upstream) =
        spawn_websocket_failure_then_websocket_success_upstream(response_failed_websocket_message(
            "resp_invalid_reasoning_failed",
            "invalid_encrypted_content",
            "Invalid encrypted content in reasoning replay",
        ))
        .await;
    let (app, api_key, _pool, _dir) = test_app_with_account_and_pool(base_url).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "previous_response_id": "resp_stale",
                        "turnState": "turn-stale",
                        "input": [{"role": "user", "content": "Continue"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let captured = upstream.await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_history_recovery");
    assert!(captured_header(&captured.second_ws_headers, "x-codex-turn-state").is_none());
    assert!(captured
        .second_ws_payload
        .get("previous_response_id")
        .is_none());
}

#[tokio::test]
async fn responses_should_strip_history_after_sse_invalid_encrypted_reasoning_replay() {
    let (base_url, upstream) =
        spawn_websocket_failure_then_websocket_success_upstream(response_failed_websocket_message(
            "resp_invalid_reasoning_failed",
            "invalid_encrypted_content",
            "Invalid encrypted content in reasoning replay",
        ))
        .await;
    let (app, api_key, _pool, _dir) = test_app_with_account_and_pool(base_url).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "previous_response_id": "resp_stale",
                        "turnState": "turn-stale",
                        "input": [{"role": "user", "content": "Continue"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let captured = upstream.await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_history_recovery");
    assert!(captured_header(&captured.second_ws_headers, "x-codex-turn-state").is_none());
    assert!(captured
        .second_ws_payload
        .get("previous_response_id")
        .is_none());
}

#[tokio::test]
async fn responses_stream_should_strip_history_after_previous_response_not_found() {
    let (base_url, upstream) =
        spawn_websocket_failure_then_websocket_success_upstream(response_failed_websocket_message(
            "resp_previous_missing_failed",
            "previous_response_not_found",
            "Previous response with id resp_stale was not found",
        ))
        .await;
    let (app, api_key, _pool, _dir) = test_app_with_account_and_pool(base_url).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "previous_response_id": "resp_stale",
                        "turnState": "turn-stale",
                        "input": [{"role": "user", "content": "Continue"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_text(response).await;
    let captured = upstream.await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("resp_after_history_recovery"));
    assert_eq!(
        captured_header(&captured.first_ws_headers, "x-codex-turn-state"),
        Some("turn-stale")
    );
    assert_eq!(
        captured.first_ws_payload["previous_response_id"],
        "resp_stale"
    );
    assert!(captured_header(&captured.second_ws_headers, "x-codex-turn-state").is_none());
    assert!(captured
        .second_ws_payload
        .get("previous_response_id")
        .is_none());
}

#[tokio::test]
async fn responses_stream_should_strip_history_after_websocket_invalid_encrypted_reasoning_replay()
{
    let (base_url, upstream) =
        spawn_websocket_failure_then_websocket_success_upstream(response_failed_websocket_message(
            "resp_invalid_reasoning_failed",
            "invalid_encrypted_content",
            "Invalid encrypted content in reasoning replay",
        ))
        .await;
    let (app, api_key, _pool, _dir) = test_app_with_account_and_pool(base_url).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "previous_response_id": "resp_stale",
                        "turnState": "turn-stale",
                        "input": [{"role": "user", "content": "Continue"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_text(response).await;
    let captured = upstream.await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("resp_after_history_recovery"));
    assert_eq!(
        captured_header(&captured.first_ws_headers, "x-codex-turn-state"),
        Some("turn-stale")
    );
    assert_eq!(
        captured.first_ws_payload["previous_response_id"],
        "resp_stale"
    );
    assert!(captured_header(&captured.second_ws_headers, "x-codex-turn-state").is_none());
    assert!(captured
        .second_ws_payload
        .get("previous_response_id")
        .is_none());
}

#[tokio::test]
async fn responses_stream_should_proxy_sse_and_record_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_STREAM_USAGE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("event: response.completed"));
    assert!(body.contains("resp_stream_usage"));
    assert!(
        body.ends_with("data: [DONE]\n\n"),
        "stream responses should terminate clients, body was {body:?}"
    );
    assert_eq!(usage, (1, 3, 5));
}

#[tokio::test]
async fn responses_stream_should_close_http_sse_upstream_when_client_disconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http_request(&mut socket).await;
        write_chunked_http_sse_headers(&mut socket).await;
        write_http_chunk(
            &mut socket,
            b"event: response.output_text.delta\ndata: {\"delta\":\"hello\"}\n\n",
        )
        .await;
        socket.flush().await.unwrap();

        timeout(
            StdDuration::from_secs(2),
            wait_for_http_sse_upstream_disconnect(&mut socket),
        )
        .await
        .is_ok()
    });

    let (app, api_key, _dir) = test_app_with_account(base_url).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let first_chunk = timeout(StdDuration::from_secs(1), body.next())
        .await
        .expect("first SSE chunk should arrive before disconnect")
        .expect("stream should yield a first chunk")
        .expect("chunk should be readable");
    assert!(String::from_utf8(first_chunk.to_vec())
        .unwrap()
        .contains("event: response.output_text.delta"));

    drop(body);
    assert!(
        upstream.await.unwrap(),
        "dropping the downstream stream should close the HTTP SSE upstream socket"
    );
}

#[tokio::test]
async fn responses_stream_should_record_event_log_after_completed_stream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("retry-after", "7")
                .insert_header("x-ratelimit-limit-requests", "99")
                .set_body_string(RESPONSES_STREAM_USAGE_SSE),
        )
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_logging(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-request-id", "req_stream_completed_log")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "stream with log"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    let event = latest_response_event_log(&pool).await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();

    assert!(body.contains("resp_stream_usage"));
    assert_eq!(event.level, "info");
    assert_eq!(
        event.request_id.as_deref(),
        Some("req_stream_completed_log")
    );
    assert_eq!(event.account_id.as_deref(), Some("acct_chat"));
    assert_eq!(event.status_code, Some(200));
    assert_eq!(metadata["stream"], true);
    assert_eq!(metadata["completed"], true);
    assert_eq!(metadata["responseId"], "resp_stream_usage");
    assert_eq!(metadata["usage"]["inputTokens"], 3);
    assert_eq!(metadata["usage"]["outputTokens"], 5);
    assert_rate_limit_header(&metadata, "retry-after", "7");
    assert_rate_limit_header(&metadata, "x-ratelimit-limit-requests", "99");
    assert!(metadata.get("requestBody").is_none());
    assert!(metadata.get("responseBody").is_none());
}

#[tokio::test]
async fn responses_stream_should_preserve_body_metadata_when_capture_body_enabled() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_STREAM_USAGE_SSE),
        )
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) =
        test_app_with_account_pool_and_logging_capture_body(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-request-id", "req_stream_capture_body_log")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "stream body capture"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_text(response).await;
    let event = latest_response_event_log(&pool).await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();

    assert!(body.contains("resp_stream_usage"));
    assert_eq!(event.level, "info");
    assert_eq!(
        event.request_id.as_deref(),
        Some("req_stream_capture_body_log")
    );
    assert_eq!(metadata["requestBody"]["model"], "gpt-5.5");
    assert_eq!(metadata["requestBody"]["stream"], true);
    assert!(metadata["responseBody"]
        .as_str()
        .is_some_and(|body| body.contains("resp_stream_usage")));
}

#[tokio::test]
async fn responses_stream_should_record_event_log_after_late_disconnect() {
    let first_frame = r#"event: response.output_text.delta
data: {"delta":"partial before logged disconnect"}

"#;
    let (base_url, first_chunk_sent, close_upstream) =
        spawn_chunked_sse_upstream_then_clean_close_with_headers(
            first_frame,
            &[
                ("retry-after", "11"),
                ("x-codex-primary-used-percent", "88"),
            ],
        )
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_logging(base_url).await;
    let response_task = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-request-id", "req_stream_disconnect_log")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "stream disconnect log"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
    });
    first_chunk_sent.await.unwrap();
    let response = response_task.await.unwrap();
    close_upstream.send(()).unwrap();
    let body = response_text(response).await;
    let event = latest_response_event_log(&pool).await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();

    assert!(body.contains("stream_disconnected"));
    assert_eq!(event.level, "error");
    assert_eq!(
        event.request_id.as_deref(),
        Some("req_stream_disconnect_log")
    );
    assert_eq!(event.account_id.as_deref(), Some("acct_chat"));
    assert_eq!(event.status_code, Some(502));
    assert_eq!(metadata["stream"], true);
    assert_eq!(metadata["failed"], true);
    assert_eq!(metadata["upstreamCode"], "stream_disconnected");
    assert_rate_limit_header(&metadata, "retry-after", "11");
    assert_rate_limit_header(&metadata, "x-codex-primary-used-percent", "88");
}

#[tokio::test]
async fn responses_stream_should_forward_first_chunk_before_upstream_completes() {
    let first_frame = r#"event: response.output_text.delta
data: {"delta":"live stream hello"}

"#;
    let final_frame = r#"event: response.completed
data: {"response":{"id":"resp_live_stream","object":"response","status":"completed","usage":{"input_tokens":3,"output_tokens":4}}}

"#;
    let (base_url, first_chunk_sent, finish_upstream) =
        spawn_chunked_sse_upstream(first_frame, final_frame).await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(base_url).await;
    let response_task = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
    });

    first_chunk_sent.await.unwrap();
    let response = timeout(StdDuration::from_millis(300), response_task)
        .await
        .expect("stream response should be returned before upstream completes")
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let mut body_stream = response.into_body().into_data_stream();
    let first_chunk = timeout(StdDuration::from_millis(300), body_stream.next())
        .await
        .expect("first proxied SSE chunk should arrive before upstream completes")
        .unwrap()
        .unwrap();
    let first_chunk = String::from_utf8(first_chunk.to_vec()).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(first_chunk.contains("live stream hello"));
    assert!(!first_chunk.contains("resp_live_stream"));

    finish_upstream.send(()).unwrap();
    let mut rest = Vec::new();
    while let Some(chunk) = body_stream.next().await {
        rest.extend_from_slice(&chunk.unwrap());
    }
    let rest = String::from_utf8(rest).unwrap();
    let usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(rest.contains("resp_live_stream"));
    assert!(
        rest.ends_with("data: [DONE]\n\n"),
        "stream responses should terminate clients, body was {rest:?}"
    );
    assert_eq!(usage, (1, 3, 4));
}

#[tokio::test]
async fn responses_stream_should_emit_failed_event_after_upstream_read_error_once_downstream_started(
) {
    let first_frame = r#"event: response.output_text.delta
data: {"delta":"partial before transport failure"}

"#;
    let (base_url, first_chunk_sent, close_upstream) =
        spawn_chunked_sse_upstream_then_abrupt_close(first_frame).await;

    let (app, api_key, _pool, _dir) = test_app_with_account_and_pool(base_url).await;
    let response_task = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Start then fail"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
    });

    first_chunk_sent.await.unwrap();
    let response = timeout(StdDuration::from_millis(300), response_task)
        .await
        .expect("stream response should be returned before upstream closes")
        .unwrap();
    let mut body_stream = response.into_body().into_data_stream();
    let first_chunk = timeout(StdDuration::from_millis(300), body_stream.next())
        .await
        .expect("first proxied SSE chunk should arrive before upstream closes")
        .unwrap()
        .unwrap();
    assert!(String::from_utf8(first_chunk.to_vec())
        .unwrap()
        .contains("partial before transport failure"));

    close_upstream.send(()).unwrap();
    let rest = collect_stream_body(body_stream).await;

    assert!(rest.contains("event: response.failed"));
    assert!(rest.contains("stream_disconnected"));
    assert!(rest.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn responses_stream_should_emit_failed_event_when_upstream_closes_without_completed() {
    let first_frame = r#"event: response.output_text.delta
data: {"delta":"partial before clean close"}

"#;
    let (base_url, first_chunk_sent, close_upstream) =
        spawn_chunked_sse_upstream_then_clean_close(first_frame).await;

    let (app, api_key, _pool, _dir) = test_app_with_account_and_pool(base_url).await;
    let response_task = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Start then close"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
    });

    first_chunk_sent.await.unwrap();
    let response = timeout(StdDuration::from_millis(300), response_task)
        .await
        .expect("stream response should be returned before upstream closes")
        .unwrap();
    let mut body_stream = response.into_body().into_data_stream();
    let first_chunk = timeout(StdDuration::from_millis(300), body_stream.next())
        .await
        .expect("first proxied SSE chunk should arrive before upstream closes")
        .unwrap()
        .unwrap();
    assert!(String::from_utf8(first_chunk.to_vec())
        .unwrap()
        .contains("partial before clean close"));

    close_upstream.send(()).unwrap();
    let rest = collect_stream_body(body_stream).await;

    assert!(rest.contains("event: response.failed"));
    assert!(rest.contains("stream_disconnected"));
    assert!(rest.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn responses_stream_should_fallback_to_next_account_after_rate_limit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "90")
                .set_body_json(json!({
                    "error": {"message": "rate limited"}
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_STREAM_AFTER_429_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let primary_quota_state: (i64, Option<String>) = sqlx::query_as(
        "select quota_limit_reached, quota_cooldown_until from accounts where id = ?",
    )
    .bind("acct_primary")
    .fetch_one(&pool)
    .await
    .unwrap();
    let primary_usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_primary")
    .fetch_one(&pool)
    .await
    .unwrap();
    let secondary_usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_secondary")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("resp_stream_after_429"));
    assert_eq!(primary_quota_state.0, 1);
    assert!(primary_quota_state.1.is_some());
    assert_eq!(primary_usage, (1, 0, 0));
    assert_eq!(secondary_usage, (1, 4, 1));
}

#[tokio::test]
async fn responses_stream_should_mark_expired_after_401_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_revoked", "message": "token revoked"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_STREAM_AFTER_401_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();
    let secondary_usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_secondary")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("resp_stream_after_401"));
    assert_eq!(account_status.0, "expired");
    assert_eq!(secondary_usage, (1, 4, 2));
}

#[tokio::test]
async fn responses_stream_should_return_auth_error_when_401_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": "token_revoked", "message": "token revoked"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("event: response.failed"));
    assert!(body.contains("All accounts exhausted (1 expired)"));
    assert!(body.contains("token_revoked"));
    assert_eq!(account_status.0, "expired");
}

#[tokio::test]
async fn responses_stream_should_mark_banned_after_403_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"message": "account banned"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_STREAM_AFTER_403_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("resp_stream_after_403"));
    assert_eq!(account_status.0, "banned");
}

#[tokio::test]
async fn responses_stream_should_cool_down_cloudflare_403_and_fallback() {
    let started_at = Utc::now();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(403).set_body_string(
            "<html><title>Just a moment...</title><body>cf_chl challenge</body></html>",
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_STREAM_AFTER_CLOUDFLARE_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let cookie_store = SqliteCookieStore::new(pool.clone(), SecretBox::new([83u8; 32]));
    cookie_store
        .set_cookie_header("acct_primary", "cf_clearance=old")
        .await
        .unwrap();
    cookie_store
        .set_cookie_header("acct_secondary", "cf_clearance=keep")
        .await
        .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let primary_state: (String, Option<String>) =
        sqlx::query_as("select status, cloudflare_cooldown_until from accounts where id = ?")
            .bind("acct_primary")
            .fetch_one(&pool)
            .await
            .unwrap();
    let cooldown_until = chrono::DateTime::parse_from_rfc3339(primary_state.1.as_deref().unwrap())
        .unwrap()
        .with_timezone(&Utc);

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("resp_stream_after_cloudflare"));
    assert_eq!(primary_state.0, "active");
    assert!(cooldown_until > started_at);
    assert_eq!(
        cookie_store
            .cookie_header("acct_primary", "chatgpt.com")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        cookie_store
            .cookie_header("acct_secondary", "chatgpt.com")
            .await
            .unwrap()
            .as_deref(),
        Some("cf_clearance=keep")
    );
}

#[tokio::test]
async fn responses_stream_should_return_cloudflare_path_block_error_when_404_fallback_is_exhausted()
{
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let cookie_store = SqliteCookieStore::new(pool.clone(), SecretBox::new([83u8; 32]));
    cookie_store
        .set_cookie_header("acct_chat", "cf_clearance=old")
        .await
        .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("event: response.failed"));
    assert!(body.contains("All accounts exhausted (1 cloudflare-path-block)"));
    assert!(body.contains("Cloudflare path-block"));
    assert_eq!(
        cookie_store
            .cookie_header("acct_chat", "chatgpt.com")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn responses_should_retry_same_account_after_5xx_before_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {"message": "temporary upstream failure"}
        })))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_AFTER_5XX_RETRY_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let authorizations = received_authorizations(&server).await;
    let primary_requests = authorizations
        .iter()
        .filter(|authorization| authorization.as_str() == "Bearer access-primary")
        .count();
    let secondary_requests = authorizations
        .iter()
        .filter(|authorization| authorization.as_str() == "Bearer access-secondary")
        .count();

    assert_eq!(status, StatusCode::OK, "requests: {authorizations:?}");
    assert_eq!(body["id"], "resp_after_5xx_retry");
    assert_eq!(primary_requests, 3, "requests: {authorizations:?}");
    assert_eq!(secondary_requests, 0, "requests: {authorizations:?}");
}

#[tokio::test]
async fn responses_stream_should_retry_same_account_after_5xx_before_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {"message": "temporary upstream failure"}
        })))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_STREAM_AFTER_5XX_RETRY_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let authorizations = received_authorizations(&server).await;
    let primary_requests = authorizations
        .iter()
        .filter(|authorization| authorization.as_str() == "Bearer access-primary")
        .count();
    let secondary_requests = authorizations
        .iter()
        .filter(|authorization| authorization.as_str() == "Bearer access-secondary")
        .count();

    assert_eq!(status, StatusCode::OK, "requests: {authorizations:?}");
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("resp_stream_after_5xx_retry"));
    assert_eq!(primary_requests, 3, "requests: {authorizations:?}");
    assert_eq!(secondary_requests, 0, "requests: {authorizations:?}");
}

#[tokio::test]
async fn responses_should_mark_quota_exhausted_after_402_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "error": {"message": "quota exhausted"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_AFTER_402_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();
    let failed_usage: Option<(i64,)> =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_primary")
            .fetch_optional(&pool)
            .await
            .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_402");
    assert_eq!(account_status.0, "quota_exhausted");
    assert_eq!(failed_usage.map(|row| row.0).unwrap_or_default(), 1);
}

#[tokio::test]
async fn responses_stream_should_mark_quota_exhausted_after_402_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "error": {"message": "quota exhausted"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_STREAM_AFTER_402_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();
    let secondary_usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_secondary")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("resp_stream_after_402"));
    assert_eq!(account_status.0, "quota_exhausted");
    assert_eq!(secondary_usage, (1, 6, 3));
}

#[tokio::test]
async fn responses_should_return_quota_error_when_402_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "error": {"message": "quota reached"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();
    let message = body["error"]["message"].as_str().unwrap_or_default();

    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert!(message.contains("All accounts exhausted (1 quota-exhausted)"));
    assert!(message.contains("quota reached"));
    assert_eq!(body["error"]["code"], "upstream_error");
    assert_eq!(account_status.0, "quota_exhausted");
}

#[tokio::test]
async fn responses_should_return_rate_limit_error_when_429_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {"message": "rate limited"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let quota_state: (i64, Option<String>) = sqlx::query_as(
        "select quota_limit_reached, quota_cooldown_until from accounts where id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();
    let message = body["error"]["message"].as_str().unwrap_or_default();

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(message.contains("All accounts exhausted (1 rate-limited)"));
    assert!(message.contains("rate limited"));
    assert_eq!(body["error"]["code"], "upstream_error");
    assert_eq!(quota_state.0, 1);
    assert!(quota_state.1.is_some());
}

#[tokio::test]
async fn responses_stream_should_return_rate_limit_error_when_429_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {"message": "rate limited"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let quota_state: (i64, Option<String>) = sqlx::query_as(
        "select quota_limit_reached, quota_cooldown_until from accounts where id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("event: response.failed"));
    assert!(body.contains("rate_limit_exceeded"));
    assert!(body.contains("All accounts exhausted (1 rate-limited)"));
    assert!(body.contains("rate limited"));
    assert_eq!(quota_state.0, 1);
    assert!(quota_state.1.is_some());
}

#[tokio::test]
async fn responses_stream_should_return_quota_error_when_402_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "error": {"message": "quota reached"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("event: response.failed"));
    assert!(body.contains("All accounts exhausted (1 quota-exhausted)"));
    assert!(body.contains("quota reached"));
    assert_eq!(account_status.0, "quota_exhausted");
}

#[tokio::test]
async fn responses_should_classify_sse_quota_failure_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_FAILED_QUOTA_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_AFTER_402_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_402");
    assert_eq!(account_status.0, "quota_exhausted");
}

#[tokio::test]
async fn responses_should_record_request_count_when_5xx_retries_are_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {"message": "temporary upstream failure"}
        })))
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let authorizations = received_authorizations(&server).await;
    let failed_usage: Option<(i64,)> =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_chat")
            .fetch_optional(&pool)
            .await
            .unwrap();

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(authorizations.len(), 3, "requests: {authorizations:?}");
    assert_eq!(failed_usage.map(|row| row.0).unwrap_or_default(), 1);
}

#[tokio::test]
async fn responses_should_prefer_session_affinity_account_for_previous_response() {
    let (base_url, upstream) = spawn_single_websocket_completed_upstream("resp_affinity_ws").await;
    let (app, api_key, _dir) = test_app_with_two_accounts_and_affinity(base_url).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "previous_response_id": "resp_previous",
                        "input": [{"role": "user", "content": "Continue"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let captured = upstream.await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        captured_header(&captured.headers, "authorization"),
        Some("Bearer access-affinity")
    );
    assert_eq!(
        captured_header(&captured.headers, "chatgpt-account-id"),
        Some("chatgpt-affinity")
    );
    assert_eq!(captured.payload["previous_response_id"], "resp_previous");
}

#[tokio::test]
async fn responses_with_previous_response_id_should_use_websocket_and_configured_pool() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_async(first_stream).await.unwrap();
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_runtime_pool_first").into(),
            ))
            .await
            .unwrap();

        tokio::select! {
            second_message = first_websocket.next() => {
                let _second_message = second_message.unwrap().unwrap();
                first_websocket
                    .send(Message::Text(
                        response_completed_websocket_message("resp_runtime_pool_second").into(),
                    ))
                    .await
                    .unwrap();
                first_websocket.close(None).await.unwrap();
            }
            accepted = listener.accept() => {
                let (second_stream, _) = accepted.unwrap();
                accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
                let mut second_websocket = accept_async(second_stream).await.unwrap();
                let _second_message = second_websocket.next().await.unwrap().unwrap();
                second_websocket
                    .send(Message::Text(
                        response_completed_websocket_message("resp_runtime_pool_second").into(),
                    ))
                    .await
                    .unwrap();
                second_websocket.close(None).await.unwrap();
                first_websocket.close(None).await.unwrap();
            }
        }
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first = app
        .clone()
        .oneshot(responses_previous_request(
            &api_key,
            "Continue from pooled runtime websocket",
        ))
        .await
        .unwrap();
    let second = app
        .oneshot(responses_previous_request(
            &api_key,
            "Continue again from pooled runtime websocket",
        ))
        .await
        .unwrap();
    server.await.unwrap();

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn responses_stream_with_previous_response_id_should_forward_websocket_chunks_before_completion(
) {
    let (base_url, first_chunk_sent_rx, finish_tx, upstream) =
        spawn_chunked_websocket_upstream().await;
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let response = app
        .oneshot(responses_previous_stream_request(
            &api_key,
            "Continue as a WebSocket stream",
        ))
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    first_chunk_sent_rx
        .await
        .expect("upstream should send the first websocket event");
    let first_chunk = timeout(
        StdDuration::from_secs(1),
        first_response_body_chunk(response),
    )
    .await
    .expect("downstream should receive a websocket-backed SSE chunk before upstream completes")
    .expect("response body should produce a chunk");
    finish_tx
        .send(())
        .expect("test should be able to finish upstream websocket");
    let captured = upstream.await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(first_chunk.contains("event: response.output_text.delta"));
    assert_eq!(
        captured_header(&captured.headers, "authorization"),
        Some("Bearer access-secret")
    );
    assert_eq!(
        captured.payload["previous_response_id"],
        "resp_runtime_pool_previous"
    );
}

#[tokio::test]
async fn responses_stream_with_previous_response_id_should_record_websocket_audit_metadata() {
    let (base_url, first_chunk_sent_rx, finish_tx, upstream) =
        spawn_chunked_websocket_upstream().await;
    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_logging(base_url).await;

    let response = app
        .oneshot(responses_previous_stream_request(
            &api_key,
            "Continue as a logged WebSocket stream",
        ))
        .await
        .unwrap();
    first_chunk_sent_rx
        .await
        .expect("upstream should send the first websocket event");
    finish_tx
        .send(())
        .expect("test should be able to finish upstream websocket");
    let body = response_text(response).await;
    let captured = upstream.await.unwrap();
    let event = latest_response_event_log(&pool).await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();

    assert!(body.contains("resp_live_websocket_stream"));
    assert_eq!(
        captured.payload["previous_response_id"],
        "resp_runtime_pool_previous"
    );
    assert_eq!(event.level, "info");
    assert_eq!(metadata["stream"], true);
    assert_eq!(metadata["transport"], "websocket");
    assert_eq!(metadata["usage"]["inputTokens"], 3);
    assert_eq!(metadata["usage"]["outputTokens"], 1);
    assert_rate_limit_header(&metadata, "x-codex-primary-used-percent", "44");
    assert_rate_limit_header(&metadata, "x-codex-primary-window-minutes", "5");
}

#[tokio::test]
async fn responses_should_record_session_affinity_for_completed_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_SUCCESS_SSE),
        )
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{"role": "user", "content": "Say hello"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let records = SqliteSessionAffinityStore::new(pool)
        .list_active(Utc::now())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].response_id, "resp_response_1");
    assert_eq!(records[0].entry.account_id, "acct_chat");
}

async fn test_app_with_account(base_url: String) -> (axum::Router, String, tempfile::TempDir) {
    test_app_with_account_and_installation_id(base_url, TEST_INSTALLATION_ID.to_string()).await
}

async fn test_app_with_account_and_installation_id(
    base_url: String,
    installation_id: String,
) -> (axum::Router, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("openai-chat-upstream.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let secret_box = SecretBox::new([83u8; 32]);
    let hasher = ApiKeyHasher::new([84u8; 32]);
    let api_key = insert_client_api_key(&pool, &hasher).await;
    insert_account(&pool, &secret_box).await;
    let state = AppState::with_pool_secret_api_key_hasher_and_installation_id(
        test_config(url, base_url),
        pool,
        secret_box,
        hasher,
        installation_id,
    );
    state
        .restore_account_pool_from_repository()
        .await
        .expect("account pool should restore");
    (router::router().with_state(state), api_key, dir)
}

async fn test_app_with_account_and_pool(
    base_url: String,
) -> (axum::Router, String, SqlitePool, tempfile::TempDir) {
    test_app_with_account_pool_config(base_url, |_| {}).await
}

async fn test_app_without_accounts(base_url: String) -> (axum::Router, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("openai-responses-no-accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let secret_box = SecretBox::new([83u8; 32]);
    let hasher = ApiKeyHasher::new([84u8; 32]);
    let api_key = insert_client_api_key(&pool, &hasher).await;
    let state = AppState::with_pool_secret_api_key_hasher_and_installation_id(
        test_config(url, base_url),
        pool,
        secret_box,
        hasher,
        TEST_INSTALLATION_ID.to_string(),
    );
    state
        .restore_account_pool_from_repository()
        .await
        .expect("empty account pool should restore");
    (router::router().with_state(state), api_key, dir)
}

async fn test_app_with_account_pool_and_logging(
    base_url: String,
) -> (axum::Router, String, SqlitePool, tempfile::TempDir) {
    test_app_with_account_pool_config(base_url, |config| {
        config.logging.enabled = true;
    })
    .await
}

async fn test_app_with_account_pool_and_logging_capture_body(
    base_url: String,
) -> (axum::Router, String, SqlitePool, tempfile::TempDir) {
    test_app_with_account_pool_config(base_url, |config| {
        config.logging.enabled = true;
        config.logging.capture_body = true;
    })
    .await
}

async fn test_app_with_account_pool_config(
    base_url: String,
    configure: impl FnOnce(&mut AppConfig),
) -> (axum::Router, String, SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("openai-record-affinity.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let secret_box = SecretBox::new([83u8; 32]);
    let hasher = ApiKeyHasher::new([84u8; 32]);
    let api_key = insert_client_api_key(&pool, &hasher).await;
    insert_account(&pool, &secret_box).await;
    let mut config = test_config(url, base_url);
    configure(&mut config);
    let state = AppState::with_pool_secret_api_key_hasher_and_installation_id(
        config,
        pool.clone(),
        secret_box,
        hasher,
        TEST_INSTALLATION_ID.to_string(),
    );
    state
        .restore_account_pool_from_repository()
        .await
        .expect("account pool should restore");
    (router::router().with_state(state), api_key, pool, dir)
}

async fn test_app_with_restored_pool_then_disabled_account(
    base_url: String,
) -> (axum::Router, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("openai-chat-restored-pool.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let secret_box = SecretBox::new([83u8; 32]);
    let hasher = ApiKeyHasher::new([84u8; 32]);
    let api_key = insert_client_api_key(&pool, &hasher).await;
    insert_account(&pool, &secret_box).await;
    let state = AppState::with_pool_secret_api_key_hasher_and_installation_id(
        test_config(url, base_url),
        pool.clone(),
        secret_box,
        hasher,
        TEST_INSTALLATION_ID.to_string(),
    );
    state
        .restore_account_pool_from_repository()
        .await
        .expect("account pool should restore");
    sqlx::query("update accounts set status = 'disabled' where id = ?")
        .bind("acct_chat")
        .execute(&pool)
        .await
        .unwrap();
    (router::router().with_state(state), api_key, dir)
}

async fn test_app_with_two_accounts_and_affinity(
    base_url: String,
) -> (axum::Router, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("openai-responses-affinity.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let secret_box = SecretBox::new([83u8; 32]);
    let hasher = ApiKeyHasher::new([84u8; 32]);
    let api_key = insert_client_api_key(&pool, &hasher).await;
    insert_named_account(
        &pool,
        &secret_box,
        "acct_a",
        "access-default",
        "chatgpt-default",
    )
    .await;
    insert_named_account(
        &pool,
        &secret_box,
        "acct_z",
        "access-affinity",
        "chatgpt-affinity",
    )
    .await;
    let now = Utc::now();
    SqliteSessionAffinityStore::new(pool.clone())
        .upsert(
            "resp_previous",
            &SessionAffinityEntry {
                account_id: "acct_z".to_string(),
                conversation_id: "conv_affinity".to_string(),
                turn_state: Some("turn_affinity".to_string()),
                instructions_hash: None,
                input_tokens: None,
                function_call_ids: Vec::new(),
                variant_hash: None,
                created_at: now,
            },
            Duration::hours(4),
        )
        .await
        .unwrap();
    let state = AppState::with_pool_secret_api_key_hasher_and_installation_id(
        test_config(url, base_url),
        pool,
        secret_box,
        hasher,
        TEST_INSTALLATION_ID.to_string(),
    );
    state
        .restore_account_pool_from_repository()
        .await
        .expect("account pool should restore");
    state
        .restore_session_affinity_from_repository(now + Duration::minutes(1))
        .await
        .expect("session affinity should restore");
    (router::router().with_state(state), api_key, dir)
}

async fn test_app_with_two_accounts(
    base_url: String,
) -> (axum::Router, String, SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("openai-responses-fallback.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let secret_box = SecretBox::new([83u8; 32]);
    let hasher = ApiKeyHasher::new([84u8; 32]);
    let api_key = insert_client_api_key(&pool, &hasher).await;
    insert_named_account(
        &pool,
        &secret_box,
        "acct_primary",
        "access-primary",
        "chatgpt-primary",
    )
    .await;
    insert_named_account(
        &pool,
        &secret_box,
        "acct_secondary",
        "access-secondary",
        "chatgpt-secondary",
    )
    .await;
    let state = AppState::with_pool_secret_api_key_hasher_and_installation_id(
        test_config(url, base_url),
        pool.clone(),
        secret_box,
        hasher,
        TEST_INSTALLATION_ID.to_string(),
    );
    state
        .restore_account_pool_from_repository()
        .await
        .expect("account pool should restore");
    (router::router().with_state(state), api_key, pool, dir)
}

async fn insert_client_api_key(pool: &SqlitePool, hasher: &ApiKeyHasher) -> String {
    let generated = hasher.generate_client_api_key("test");
    sqlx::query(
        "insert into client_api_keys (id, name, prefix, key_hash, enabled, created_at) values (?, ?, ?, ?, 1, ?)",
    )
    .bind("key_test")
    .bind("test")
    .bind(&generated.prefix)
    .bind(&generated.key_hash)
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
    generated.plaintext
}

async fn seed_openai_admin_session(pool: &SqlitePool, session_id: &str) {
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at) values (?, ?, ?, ?)",
    )
    .bind("admin_openai")
    .bind("hash")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into admin_sessions (id, user_id, expires_at, created_at) values (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind("admin_openai")
    .bind("2999-01-01T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
}

async fn update_admin_account_status(app: &axum::Router, account_id: &str, status: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/accounts/{account_id}/status"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_status_cycle")
                .body(Body::from(json!({"status": status}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn insert_account(pool: &SqlitePool, secret_box: &SecretBox) {
    let access_token = secret_box
        .encrypt(&SecretString::new("access-secret".to_string().into()))
        .unwrap();
    sqlx::query(
        "insert into accounts (id, email, account_id, user_id, access_token_cipher, access_token_expires_at, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_chat")
    .bind("user@example.com")
    .bind("chatgpt-account")
    .bind("chatgpt-user")
    .bind(access_token)
    .bind("2100-01-01T00:00:00Z")
    .bind("active")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_named_account(
    pool: &SqlitePool,
    secret_box: &SecretBox,
    id: &str,
    access_token_plaintext: &str,
    chatgpt_account_id: &str,
) {
    let access_token = secret_box
        .encrypt(&SecretString::new(
            access_token_plaintext.to_string().into(),
        ))
        .unwrap();
    sqlx::query(
        "insert into accounts (id, email, account_id, user_id, access_token_cipher, access_token_expires_at, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(format!("{id}@example.com"))
    .bind(chatgpt_account_id)
    .bind(format!("user-{id}"))
    .bind(access_token)
    .bind("2100-01-01T00:00:00Z")
    .bind("active")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(pool)
    .await
    .unwrap();
}

fn tuple_response_request_body(stream: bool) -> String {
    json!({
        "model": "gpt-5.5",
        "stream": stream,
        "use_websocket": false,
        "input": [],
        "text": {
            "format": {
                "type": "json_schema",
                "name": "TupleAnswer",
                "schema": {
                    "type": "object",
                    "properties": {
                        "point": {
                            "type": "array",
                            "prefixItems": [
                                {"type": "number"},
                                {"type": "number"}
                            ],
                            "items": false
                        }
                    },
                    "required": ["point"]
                },
                "strict": true
            }
        }
    })
    .to_string()
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

struct CapturedWebSocketRequest {
    headers: Vec<(String, String)>,
    payload: Value,
}

struct HistoryRecoveryCapture {
    first_ws_headers: Vec<(String, String)>,
    first_ws_payload: Value,
    second_ws_headers: Vec<(String, String)>,
    second_ws_payload: Value,
}

#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn spawn_single_websocket_completed_upstream(
    response_id: &'static str,
) -> (String, tokio::task::JoinHandle<CapturedWebSocketRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let captured_headers = Arc::new(Mutex::new(Vec::new()));
        let captured_headers_for_callback = Arc::clone(&captured_headers);
        let mut websocket = accept_hdr_async(
            stream,
            move |request: &WsRequest, mut response: WsResponse| {
                *captured_headers_for_callback.lock().unwrap() = request_headers(request);
                response
                    .headers_mut()
                    .insert("x-ratelimit-limit-requests", "55".parse().unwrap());
                response
                    .headers_mut()
                    .insert("x-codex-primary-used-percent", "44".parse().unwrap());
                Ok(response)
            },
        )
        .await
        .unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let payload = serde_json::from_str::<Value>(&message.into_text().unwrap())
            .expect("websocket payload should be json");
        websocket
            .send(Message::Text(
                response_completed_websocket_message(response_id).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        let headers = captured_headers.lock().unwrap().clone();
        CapturedWebSocketRequest { headers, payload }
    });
    (base_url, server)
}

#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn spawn_websocket_failure_then_websocket_success_upstream(
    failure: String,
) -> (String, tokio::task::JoinHandle<HistoryRecoveryCapture>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let first_headers = Arc::new(Mutex::new(Vec::new()));
        let first_headers_for_callback = Arc::clone(&first_headers);
        let mut first_websocket = accept_hdr_async(
            first_stream,
            move |request: &WsRequest, response: WsResponse| {
                *first_headers_for_callback.lock().unwrap() = request_headers(request);
                Ok(response)
            },
        )
        .await
        .unwrap();
        let first_message = first_websocket.next().await.unwrap().unwrap();
        let first_ws_payload = serde_json::from_str::<Value>(&first_message.into_text().unwrap())
            .expect("websocket payload should be json");
        first_websocket
            .send(Message::Text(failure.into()))
            .await
            .unwrap();
        drop(first_websocket);

        let (second_stream, _) = listener.accept().await.unwrap();
        let second_headers = Arc::new(Mutex::new(Vec::new()));
        let second_headers_for_callback = Arc::clone(&second_headers);
        let mut second_websocket = accept_hdr_async(
            second_stream,
            move |request: &WsRequest, response: WsResponse| {
                *second_headers_for_callback.lock().unwrap() = request_headers(request);
                Ok(response)
            },
        )
        .await
        .unwrap();
        let second_message = second_websocket.next().await.unwrap().unwrap();
        let second_ws_payload = serde_json::from_str::<Value>(&second_message.into_text().unwrap())
            .expect("second websocket payload should be json");
        second_websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_after_history_recovery").into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
        let first_ws_headers = first_headers.lock().unwrap().clone();
        let second_ws_headers = second_headers.lock().unwrap().clone();

        HistoryRecoveryCapture {
            first_ws_headers,
            first_ws_payload,
            second_ws_headers,
            second_ws_payload,
        }
    });
    (base_url, server)
}

#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn spawn_chunked_websocket_upstream() -> (
    String,
    oneshot::Receiver<()>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<CapturedWebSocketRequest>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (first_chunk_sent_tx, first_chunk_sent_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let captured_headers = Arc::new(Mutex::new(Vec::new()));
        let captured_headers_for_callback = Arc::clone(&captured_headers);
        let mut websocket =
            accept_hdr_async(stream, move |request: &WsRequest, response: WsResponse| {
                *captured_headers_for_callback.lock().unwrap() = request_headers(request);
                Ok(response)
            })
            .await
            .unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let payload = serde_json::from_str::<Value>(&message.into_text().unwrap())
            .expect("websocket payload should be json");
        websocket
            .send(Message::Text(
                json!({
                    "type": "codex.rate_limits",
                    "rate_limits": {
                        "primary": {
                            "used_percent": 44.0,
                            "window_minutes": 5
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "live websocket hello"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let _ = first_chunk_sent_tx.send(());
        let _ = finish_rx.await;
        websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_live_websocket_stream").into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        let headers = captured_headers.lock().unwrap().clone();
        CapturedWebSocketRequest { headers, payload }
    });
    (base_url, first_chunk_sent_rx, finish_tx, server)
}

async fn accept_successful_websocket_response(listener: &TcpListener, response_id: &str) -> Value {
    let (stream, _) = listener.accept().await.unwrap();
    let mut websocket = accept_async(stream).await.unwrap();
    let message = websocket.next().await.unwrap().unwrap();
    let payload = serde_json::from_str::<Value>(&message.into_text().unwrap())
        .expect("websocket payload should be json");
    websocket
        .send(Message::Text(
            response_completed_websocket_message(response_id).into(),
        ))
        .await
        .unwrap();
    websocket.close(None).await.unwrap();
    payload
}

async fn accept_successful_websocket_response_with_authorization(
    listener: &TcpListener,
    expected_authorization: &'static str,
    response_id: &str,
) -> Value {
    let (stream, _) = listener.accept().await.unwrap();
    let mut websocket = accept_websocket_with_authorization(stream, expected_authorization).await;
    let payload = send_websocket_response_and_capture_payload(
        &mut websocket,
        websocket_completed_response(response_id, 3, 1),
    )
    .await;
    websocket.close(None).await.unwrap();
    payload
}

async fn accept_two_successful_websocket_responses_with_authorization(
    listener: &TcpListener,
    expected_authorization: &'static str,
    first_response_id: &str,
    second_response_id: &str,
) -> (bool, Value, Value) {
    let (stream, _) = listener.accept().await.unwrap();
    let mut websocket = accept_websocket_with_authorization(stream, expected_authorization).await;
    let first_payload = send_websocket_response_and_capture_payload(
        &mut websocket,
        websocket_completed_response(first_response_id, 3, 1),
    )
    .await;

    loop {
        tokio::select! {
            message = websocket.next() => {
                match message {
                    Some(Ok(message)) if message.is_text() => {
                        let second_payload = serde_json::from_str::<Value>(
                            &message.into_text().unwrap(),
                        )
                        .expect("second websocket payload should be json");
                        websocket
                            .send(Message::Text(
                                websocket_completed_response(second_response_id, 3, 1).into(),
                            ))
                            .await
                            .unwrap();
                        websocket.close(None).await.unwrap();
                        break (true, first_payload, second_payload);
                    }
                    Some(_) => continue,
                    None => {
                        let second_payload = accept_successful_websocket_response_with_authorization(
                            listener,
                            expected_authorization,
                            second_response_id,
                        )
                        .await;
                        break (false, first_payload, second_payload);
                    }
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.unwrap();
                let mut second_websocket =
                    accept_websocket_with_authorization(stream, expected_authorization).await;
                let second_payload = send_websocket_response_and_capture_payload(
                    &mut second_websocket,
                    websocket_completed_response(second_response_id, 3, 1),
                )
                .await;
                second_websocket.close(None).await.unwrap();
                break (false, first_payload, second_payload);
            }
        }
    }
}

async fn accept_websocket_response_with_authorization_and_message(
    listener: &TcpListener,
    expected_authorization: &'static str,
    response_message: String,
) -> Value {
    let (stream, _) = listener.accept().await.unwrap();
    let mut websocket = accept_websocket_with_authorization(stream, expected_authorization).await;
    let payload =
        send_websocket_response_and_capture_payload(&mut websocket, response_message).await;
    websocket.close(None).await.unwrap();
    payload
}

#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn accept_websocket_with_authorization(
    stream: TcpStream,
    expected_authorization: &'static str,
) -> tokio_tungstenite::WebSocketStream<TcpStream> {
    accept_hdr_async(stream, move |request: &WsRequest, response: WsResponse| {
        assert_eq!(
            request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some(expected_authorization)
        );
        Ok(response)
    })
    .await
    .unwrap()
}

async fn reject_next_websocket_upgrade(
    listener: &TcpListener,
    expected_authorization: &str,
    status: u16,
    reason: &str,
    retry_after_seconds: Option<u64>,
    body: &str,
) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let request = read_http_upgrade_request(&mut stream).await;
    assert!(request.starts_with("GET /codex/responses HTTP/1.1"));
    let lower_request = request.to_ascii_lowercase();
    assert!(
        lower_request.contains(&format!(
            "authorization: {}",
            expected_authorization.to_ascii_lowercase()
        )),
        "unexpected websocket authorization header in request:\n{request}"
    );
    let retry_after = retry_after_seconds
        .map(|seconds| format!("retry-after: {seconds}\r\n"))
        .unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n{retry_after}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
}

async fn accept_websocket_response_with_message(
    listener: &TcpListener,
    response_message: String,
) -> Value {
    let (stream, _) = listener.accept().await.unwrap();
    let mut websocket = accept_async(stream).await.unwrap();
    let payload =
        send_websocket_response_and_capture_payload(&mut websocket, response_message).await;
    websocket.close(None).await.unwrap();
    payload
}

async fn accept_followup_websocket_response(
    listener: &TcpListener,
    websocket: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
    response_message: String,
) -> Value {
    tokio::select! {
        message = websocket.next() => {
            match message {
                Some(Ok(message)) if message.is_text() => {
                    let payload = serde_json::from_str::<Value>(&message.into_text().unwrap())
                        .expect("websocket payload should be json");
                    websocket
                        .send(Message::Text(response_message.into()))
                        .await
                        .unwrap();
                    payload
                }
                _ => accept_websocket_response_with_message(listener, response_message).await,
            }
        }
        accepted = listener.accept() => {
            let (stream, _) = accepted.unwrap();
            let mut followup = accept_async(stream).await.unwrap();
            let payload =
                send_websocket_response_and_capture_payload(&mut followup, response_message).await;
            followup.close(None).await.unwrap();
            payload
        }
    }
}

async fn send_websocket_response_and_capture_payload(
    websocket: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
    response_message: String,
) -> Value {
    let message = websocket.next().await.unwrap().unwrap();
    let payload = serde_json::from_str::<Value>(&message.into_text().unwrap())
        .expect("websocket payload should be json");
    websocket
        .send(Message::Text(response_message.into()))
        .await
        .unwrap();
    payload
}

fn request_headers(request: &WsRequest) -> Vec<(String, String)> {
    request
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

fn captured_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

async fn read_http_upgrade_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await.unwrap();
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(request).unwrap()
}

fn assert_response_failed_stream(
    body: &str,
    expected_error_type: &str,
    expected_code: &str,
    expected_fragments: &[&str],
) {
    assert!(body.contains("event: response.failed"));
    assert!(
        body.contains(&format!("\"type\":\"{expected_error_type}\""))
            || body.contains(&format!("\"type\": \"{expected_error_type}\"")),
        "missing error type {expected_error_type} in {body}"
    );
    assert!(
        body.contains(&format!("\"code\":\"{expected_code}\""))
            || body.contains(&format!("\"code\": \"{expected_code}\"")),
        "missing error code {expected_code} in {body}"
    );
    for fragment in expected_fragments {
        assert!(
            body.contains(fragment),
            "missing fragment {fragment:?} in {body}"
        );
    }
}

fn responses_http_sse_request(api_key: &str, request_id: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .header("x-request-id", request_id)
        .body(Body::from(
            json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": false,
                "use_websocket": false
            })
            .to_string(),
        ))
        .unwrap()
}

fn responses_json_request(api_key: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn responses_previous_request(api_key: &str, content: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-5.5",
                "previous_response_id": "resp_runtime_pool_previous",
                "input": [{"role": "user", "content": content}],
                "stream": false
            })
            .to_string(),
        ))
        .unwrap()
}

fn response_completed_websocket_message(response_id: &str) -> String {
    json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "object": "response",
            "status": "completed",
            "usage": {
                "input_tokens": 3,
                "output_tokens": 1,
                "total_tokens": 4
            }
        }
    })
    .to_string()
}

fn websocket_completed_response(
    response_id: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> String {
    json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "object": "response",
            "status": "completed",
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "total_tokens": input_tokens + output_tokens
            }
        }
    })
    .to_string()
}

fn websocket_completed_function_call_response(response_id: &str, call_id: &str) -> String {
    json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "object": "response",
            "status": "completed",
            "output": [{
                "type": "function_call",
                "id": format!("fc_{call_id}"),
                "call_id": call_id,
                "name": "lookup",
                "arguments": "{}"
            }],
            "usage": {
                "input_tokens": 6,
                "output_tokens": 2,
                "total_tokens": 8
            }
        }
    })
    .to_string()
}

fn responses_previous_stream_request(api_key: &str, content: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "model": "gpt-5.5",
                "previous_response_id": "resp_runtime_pool_previous",
                "input": [{"role": "user", "content": content}],
                "stream": true
            })
            .to_string(),
        ))
        .unwrap()
}

fn response_failed_websocket_message(response_id: &str, code: &str, message: &str) -> String {
    json!({
        "type": "response.failed",
        "response": {
            "id": response_id,
            "object": "response",
            "status": "failed",
            "error": {
                "code": code,
                "message": message
            }
        }
    })
    .to_string()
}

async fn first_response_body_chunk(response: axum::response::Response) -> Option<String> {
    let mut body_stream = response.into_body().into_data_stream();
    let chunk = body_stream.next().await?.ok()?;
    String::from_utf8(chunk.to_vec()).ok()
}

async fn response_text(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

struct ResponseEventLog {
    level: String,
    request_id: Option<String>,
    account_id: Option<String>,
    status_code: Option<i64>,
    metadata_json: String,
}

async fn latest_response_event_log(pool: &SqlitePool) -> ResponseEventLog {
    let page = SqliteEventLogStore::new(pool.clone())
        .list(
            EventLogFilter {
                kind: Some("v1.response".to_string()),
                ..EventLogFilter::default()
            },
            None,
            1,
        )
        .await
        .unwrap();
    let event = page
        .items
        .into_iter()
        .next()
        .expect("expected a v1.response event log");
    ResponseEventLog {
        level: event_level_name(event.level).to_string(),
        request_id: event.request_id,
        account_id: event.account_id,
        status_code: event.status_code,
        metadata_json: event.metadata.to_string(),
    }
}

async fn response_event_log_count(pool: &SqlitePool) -> i64 {
    let (count,): (i64,) = sqlx::query_as("select count(*) from event_logs where kind = ?")
        .bind("v1.response")
        .fetch_one(pool)
        .await
        .unwrap();
    count
}

fn event_level_name(level: EventLevel) -> &'static str {
    match level {
        EventLevel::Debug => "debug",
        EventLevel::Info => "info",
        EventLevel::Warn => "warn",
        EventLevel::Error => "error",
    }
}

fn assert_rate_limit_header(metadata: &Value, name: &str, value: &str) {
    let headers = metadata["rateLimitHeaders"]
        .as_array()
        .expect("rateLimitHeaders should be an array");
    let expected_name = name.to_ascii_lowercase();
    assert!(
        headers.iter().any(|entry| {
            let Some(entry) = entry.as_array() else {
                return false;
            };
            let Some(header_name) = entry.first().and_then(Value::as_str) else {
                return false;
            };
            let Some(header_value) = entry.get(1).and_then(Value::as_str) else {
                return false;
            };
            header_name.eq_ignore_ascii_case(&expected_name) && header_value == value
        }),
        "expected {name}: {value} in rateLimitHeaders, got {headers:?}"
    );
}

async fn spawn_chunked_sse_upstream(
    first_frame: &'static str,
    final_frame: &'static str,
) -> (String, oneshot::Receiver<()>, oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (first_chunk_sent_tx, first_chunk_sent_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http_request(&mut socket).await;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        write_http_chunk(&mut socket, first_frame.as_bytes()).await;
        socket.flush().await.unwrap();
        let _ = first_chunk_sent_tx.send(());
        let _ = finish_rx.await;
        write_http_chunk(&mut socket, final_frame.as_bytes()).await;
        socket.write_all(b"0\r\n\r\n").await.unwrap();
        socket.flush().await.unwrap();
    });

    (base_url, first_chunk_sent_rx, finish_tx)
}

async fn spawn_chunked_sse_upstream_then_abrupt_close(
    first_frame: &'static str,
) -> (String, oneshot::Receiver<()>, oneshot::Sender<()>) {
    spawn_chunked_sse_upstream_then_close(first_frame, ChunkedSseCloseMode::Abrupt).await
}

async fn spawn_chunked_sse_upstream_then_clean_close(
    first_frame: &'static str,
) -> (String, oneshot::Receiver<()>, oneshot::Sender<()>) {
    spawn_chunked_sse_upstream_then_close(first_frame, ChunkedSseCloseMode::Clean).await
}

async fn spawn_chunked_sse_upstream_then_clean_close_with_headers(
    first_frame: &'static str,
    headers: &'static [(&'static str, &'static str)],
) -> (String, oneshot::Receiver<()>, oneshot::Sender<()>) {
    spawn_chunked_sse_upstream_then_close_with_headers(
        first_frame,
        ChunkedSseCloseMode::Clean,
        headers,
    )
    .await
}

enum ChunkedSseCloseMode {
    Abrupt,
    Clean,
}

async fn spawn_chunked_sse_upstream_then_close(
    first_frame: &'static str,
    close_mode: ChunkedSseCloseMode,
) -> (String, oneshot::Receiver<()>, oneshot::Sender<()>) {
    spawn_chunked_sse_upstream_then_close_with_headers(first_frame, close_mode, &[]).await
}

async fn spawn_chunked_sse_upstream_then_close_with_headers(
    first_frame: &'static str,
    close_mode: ChunkedSseCloseMode,
    headers: &'static [(&'static str, &'static str)],
) -> (String, oneshot::Receiver<()>, oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (first_chunk_sent_tx, first_chunk_sent_rx) = oneshot::channel();
    let (close_tx, close_rx) = oneshot::channel();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_http_request(&mut socket).await;
        let extra_headers = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect::<String>();
        let response_head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n{extra_headers}\r\n"
        );
        socket.write_all(response_head.as_bytes()).await.unwrap();
        write_http_chunk(&mut socket, first_frame.as_bytes()).await;
        socket.flush().await.unwrap();
        let _ = first_chunk_sent_tx.send(());
        let _ = close_rx.await;
        if matches!(close_mode, ChunkedSseCloseMode::Clean) {
            socket.write_all(b"0\r\n\r\n").await.unwrap();
            socket.flush().await.unwrap();
        }
    });

    (base_url, first_chunk_sent_rx, close_tx)
}

async fn collect_stream_body<S, E>(mut body_stream: S) -> String
where
    S: futures::Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Debug,
{
    let mut body = Vec::new();
    while let Some(chunk) = body_stream.next().await {
        let chunk = chunk.expect("late upstream failures should be converted into SSE frames");
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).unwrap()
}

async fn read_http_request(socket: &mut TcpStream) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = socket.read(&mut chunk).await.unwrap();
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }

    let Some(header_end) = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
    else {
        return;
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let content_length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            value.trim().parse::<usize>().ok()
        } else {
            None
        }
    });
    let Some(content_length) = content_length else {
        return;
    };
    let already_read_body = buffer.len().saturating_sub(header_end);
    let remaining = content_length.saturating_sub(already_read_body);
    if remaining > 0 {
        let mut discard = vec![0u8; remaining];
        socket.read_exact(&mut discard).await.unwrap();
    }
}

async fn write_http_sse_response(socket: &mut TcpStream, body: &str) {
    socket
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}

async fn write_chunked_http_sse_headers(socket: &mut TcpStream) {
    socket
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
}

async fn write_http_chunk(socket: &mut TcpStream, bytes: &[u8]) {
    socket
        .write_all(format!("{:X}\r\n", bytes.len()).as_bytes())
        .await
        .unwrap();
    socket.write_all(bytes).await.unwrap();
    socket.write_all(b"\r\n").await.unwrap();
}

async fn wait_for_http_sse_upstream_disconnect(socket: &mut TcpStream) {
    let mut buffer = [0u8; 1024];
    loop {
        match socket.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

async fn received_authorizations(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .collect()
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
        ws_pool: WebSocketPoolConfig::default(),
        admin: AdminConfig {
            session_ttl_minutes: 1440,
            session_cleanup_interval_secs: 3600,
            default_username: "admin".to_string(),
            default_password: "admin".to_string(),
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
