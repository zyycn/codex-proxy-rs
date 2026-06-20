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
    accept_hdr_async_with_config,
    tungstenite::{
        extensions::{compression::deflate::DeflateConfig, ExtensionsConfig},
        handshake::server::{
            Callback, ErrorResponse, Request as WsRequest, Response as WsResponse,
        },
        protocol::WebSocketConfig,
        Error as WsError, Message,
    },
    WebSocketStream,
};
use tower::util::ServiceExt;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

const TEST_INSTALLATION_ID: &str = "b4f9d503-07b1-457b-a0da-87e6836b1c43";

fn websocket_accept_config() -> WebSocketConfig {
    let mut extensions = ExtensionsConfig::default();
    extensions.permessage_deflate = Some(DeflateConfig::default());

    let mut config = WebSocketConfig::default();
    config.extensions = extensions;
    config
}

async fn accept_async(stream: TcpStream) -> Result<WebSocketStream<TcpStream>, WsError> {
    accept_hdr_async(stream, |_request, _response| {}).await
}

struct TestWebSocketCallback<F>(F);

impl<F> Callback for TestWebSocketCallback<F>
where
    F: FnOnce(&WsRequest, &mut WsResponse) + Unpin,
{
    fn on_request(
        self,
        request: &WsRequest,
        mut response: WsResponse,
    ) -> Result<WsResponse, ErrorResponse> {
        (self.0)(request, &mut response);
        Ok(response)
    }
}

async fn accept_hdr_async<F>(
    stream: TcpStream,
    callback: F,
) -> Result<WebSocketStream<TcpStream>, WsError>
where
    F: FnOnce(&WsRequest, &mut WsResponse) + Unpin,
{
    accept_hdr_async_with_config(
        stream,
        TestWebSocketCallback(callback),
        Some(websocket_accept_config()),
    )
    .await
}

const CHAT_SUCCESS_SSE: &str = concat!(include_str!("fixtures/chat/success.sse"), "\n");
const RESPONSES_SUCCESS_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/success.sse"),
    "\n"
);
const RESPONSES_COMPLETED_USAGE_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/completed_usage.sse"),
    "\n"
);
const RESPONSES_COMPLETED_IMAGE_USAGE_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/completed_image_usage.sse"),
    "\n"
);
const RESPONSES_EMPTY_COMPLETED_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/empty_completed.sse"),
    "\n"
);
const RESPONSES_TEXT_DELTAS_COMPLETED_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/text_deltas_completed.sse"),
    "\n"
);
const RESPONSES_DONE_ITEM_COMPLETED_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/done_item_completed.sse"),
    "\n"
);
const RESPONSES_TUPLE_OBJECT_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/tuple_object.sse"),
    "\n"
);
const RESPONSES_AFTER_5XX_RETRY_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/after_5xx_retry.sse"),
    "\n"
);
const RESPONSES_AFTER_402_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/after_402.sse"),
    "\n"
);
const RESPONSES_FAILED_QUOTA_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/failed_quota.sse"),
    "\n"
);
const RESPONSES_FAILED_AUTH_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/failed_auth.sse"),
    "\n"
);
const RESPONSES_AFTER_401_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/after_401.sse"),
    "\n"
);
const RESPONSES_AFTER_403_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/after_403.sse"),
    "\n"
);
const RESPONSES_STREAM_USAGE_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/stream_usage.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_429_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/stream_after_429.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_5XX_RETRY_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/stream_after_5xx_retry.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_402_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/stream_after_402.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_401_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/stream_after_401.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_403_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/stream_after_403.sse"),
    "\n"
);
const RESPONSES_AFTER_CLOUDFLARE_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/after_cloudflare.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_CLOUDFLARE_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/stream_after_cloudflare.sse"),
    "\n"
);
const RESPONSES_AFTER_MODEL_UNSUPPORTED_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/after_model_unsupported.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_MODEL_UNSUPPORTED_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/stream_after_model_unsupported.sse"),
    "\n"
);
const RESPONSES_FAILED_MODEL_UNSUPPORTED_SSE: &str = concat!(
    include_str!("fixtures/responses/http_sse/failed_model_unsupported.sse"),
    "\n"
);

const WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY: &str =
    include_str!("fixtures/responses/websocket/completed_with_reasoning_replay.json");
const WEBSOCKET_HISTORY_RATE_LIMITED: &str =
    include_str!("fixtures/responses/websocket/history_rate_limited.json");
const WEBSOCKET_RATE_LIMITED: &str = include_str!("fixtures/responses/websocket/rate_limited.json");
const WEBSOCKET_TOKEN_REVOKED: &str =
    include_str!("fixtures/responses/websocket/token_revoked.json");
const WEBSOCKET_FIRST_ACCOUNT_LIMITED: &str =
    include_str!("fixtures/responses/websocket/first_account_limited.json");
const WEBSOCKET_SECOND_ACCOUNT_LIMITED: &str =
    include_str!("fixtures/responses/websocket/second_account_limited.json");
const WEBSOCKET_INVALID_ENCRYPTED_CONTENT: &str =
    include_str!("fixtures/responses/websocket/invalid_encrypted_content.json");
const WEBSOCKET_PREVIOUS_RESPONSE_NOT_FOUND: &str =
    include_str!("fixtures/responses/websocket/previous_response_not_found.json");
const WEBSOCKET_UNANSWERED_FUNCTION_CALL: &str =
    include_str!("fixtures/responses/websocket/unanswered_function_call.json");
const REASONING_REPLAY_REQUEST_GOLDEN: &str =
    include_str!("fixtures/responses/golden/reasoning_replay_request.json");

#[path = "openai_chat_upstream/openai_chat_routes.rs"]
mod openai_chat_routes;
#[path = "openai_chat_upstream/openai_compact_routes.rs"]
mod openai_compact_routes;
#[path = "openai_chat_upstream/openai_responses_http.rs"]
mod openai_responses_http;
#[path = "openai_chat_upstream/openai_responses_recovery.rs"]
mod openai_responses_recovery;
#[path = "openai_chat_upstream/openai_responses_websocket.rs"]
mod openai_responses_websocket;
#[path = "openai_chat_upstream/openai_usage_logging.rs"]
mod openai_usage_logging;

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
        "insert into accounts (id, email, chatgpt_account_id, chatgpt_user_id, access_token_cipher, access_token_expires_at, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        "insert into accounts (id, email, chatgpt_account_id, chatgpt_user_id, access_token_cipher, access_token_expires_at, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?, ?)",
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

async fn spawn_single_websocket_completed_upstream(
    response_id: &'static str,
) -> (String, tokio::task::JoinHandle<CapturedWebSocketRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let captured_headers = Arc::new(Mutex::new(Vec::new()));
        let captured_headers_for_callback = Arc::clone(&captured_headers);
        let mut websocket = accept_hdr_async(stream, move |request, response| {
            *captured_headers_for_callback.lock().unwrap() = request_headers(request);
            response
                .headers_mut()
                .insert("x-ratelimit-limit-requests", "55".parse().unwrap());
            response
                .headers_mut()
                .insert("x-codex-primary-used-percent", "44".parse().unwrap());
        })
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

async fn spawn_websocket_failure_then_websocket_success_upstream(
    failure: String,
) -> (String, tokio::task::JoinHandle<HistoryRecoveryCapture>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let first_headers = Arc::new(Mutex::new(Vec::new()));
        let first_headers_for_callback = Arc::clone(&first_headers);
        let mut first_websocket = accept_hdr_async(first_stream, move |request, _response| {
            *first_headers_for_callback.lock().unwrap() = request_headers(request);
        })
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
        let mut second_websocket = accept_hdr_async(second_stream, move |request, _response| {
            *second_headers_for_callback.lock().unwrap() = request_headers(request);
        })
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
        let mut websocket = accept_hdr_async(stream, move |request, _response| {
            *captured_headers_for_callback.lock().unwrap() = request_headers(request);
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

async fn accept_websocket_with_authorization(
    stream: TcpStream,
    expected_authorization: &'static str,
) -> tokio_tungstenite::WebSocketStream<TcpStream> {
    accept_hdr_async(stream, move |request, _response| {
        assert_eq!(
            request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some(expected_authorization)
        );
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

async fn wait_for_session_affinity_turn_state(
    pool: &SqlitePool,
    response_id: &str,
) -> Option<String> {
    timeout(StdDuration::from_secs(1), async {
        loop {
            if let Some(turn_state) = sqlx::query_scalar::<_, Option<String>>(
                "select turn_state from session_affinities where response_id = ?",
            )
            .bind(response_id)
            .fetch_optional(pool)
            .await
            .unwrap()
            .flatten()
            {
                return Some(turn_state);
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .ok()
    .flatten()
}

struct ResponseEventLog {
    level: String,
    request_id: Option<String>,
    account_id: Option<String>,
    route: Option<String>,
    status_code: Option<i64>,
    response_id: Option<String>,
    metadata_json: String,
}

async fn latest_response_event_log(pool: &SqlitePool) -> ResponseEventLog {
    latest_event_log(pool, "v1.response").await
}

async fn latest_event_log(pool: &SqlitePool, kind: &str) -> ResponseEventLog {
    let page = SqliteEventLogStore::new(pool.clone())
        .list(
            EventLogFilter {
                kind: Some(kind.to_string()),
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
        .unwrap_or_else(|| panic!("expected a {kind} event log"));
    ResponseEventLog {
        level: event_level_name(event.level).to_string(),
        request_id: event.request_id,
        account_id: event.account_id,
        route: event.route,
        status_code: event.status_code,
        response_id: event.response_id,
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
        fingerprint: Default::default(),
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
