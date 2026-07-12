use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration as StdDuration, Instant},
};

use axum::{
    body::{Body, Bytes, to_bytes},
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use codex_proxy_rs::{
    api::AppState,
    api::router,
    bootstrap::config::AppConfig,
    bootstrap::services::{Services, UsageRecordOptions},
    dispatch::affinity::SessionAffinityEntry,
    fleet::{account::AccountStatus, cookies::PgCookieStore},
    infra::identity::hash_credential,
    telemetry::usage::store::{PgUsageRecordStore, UsageRecordFilter},
};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async_with_config,
    tungstenite::{
        Error as WsError, Message,
        extensions::{ExtensionsConfig, compression::deflate::DeflateConfig},
        handshake::server::{
            Callback, ErrorResponse, Request as WsRequest, Response as WsResponse,
        },
        protocol::WebSocketConfig,
    },
};
use tower::util::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

use crate::support::{
    client_keys::insert_client_api_key,
    config::test_config as base_test_config,
    http::response_json,
    storage::{background_task_stores, create_test_redis, init_test_db, test_database_url},
};

fn websocket_accept_config() -> WebSocketConfig {
    let mut extensions = ExtensionsConfig::default();
    extensions.permessage_deflate = Some(DeflateConfig::default());

    let mut config = WebSocketConfig::default();
    config.extensions = extensions;
    config
}

pub(crate) async fn accept_async(stream: TcpStream) -> Result<WebSocketStream<TcpStream>, WsError> {
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

const RESPONSES_SUCCESS_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/success.sse"),
    "\n"
);
const RESPONSES_COMPLETED_USAGE_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/completed_usage.sse"),
    "\n"
);
const RESPONSES_COMPLETED_IMAGE_USAGE_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/completed_image_usage.sse"),
    "\n"
);
const RESPONSES_EMPTY_COMPLETED_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/empty_completed.sse"),
    "\n"
);
const RESPONSES_TEXT_DELTAS_COMPLETED_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/text_deltas_completed.sse"),
    "\n"
);
const RESPONSES_DONE_ITEM_COMPLETED_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/done_item_completed.sse"),
    "\n"
);
const RESPONSES_TUPLE_OBJECT_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/tuple_object.sse"),
    "\n"
);
const RESPONSES_AFTER_5XX_RETRY_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/after_5xx_retry.sse"),
    "\n"
);
const RESPONSES_AFTER_402_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/after_402.sse"),
    "\n"
);
const RESPONSES_FAILED_QUOTA_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/failed_quota.sse"),
    "\n"
);
const RESPONSES_FAILED_AUTH_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/failed_auth.sse"),
    "\n"
);
const RESPONSES_AFTER_401_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/after_401.sse"),
    "\n"
);
const RESPONSES_AFTER_403_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/after_403.sse"),
    "\n"
);
const RESPONSES_STREAM_USAGE_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/stream_usage.sse"),
    "\n"
);
const RESPONSES_INCOMPLETE_USAGE_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/chat_delta_incomplete_usage.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_429_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/stream_after_429.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_5XX_RETRY_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/stream_after_5xx_retry.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_402_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/stream_after_402.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_401_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/stream_after_401.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_403_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/stream_after_403.sse"),
    "\n"
);
const RESPONSES_AFTER_CLOUDFLARE_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/after_cloudflare.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_CLOUDFLARE_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/stream_after_cloudflare.sse"),
    "\n"
);
const RESPONSES_AFTER_MODEL_UNSUPPORTED_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/after_model_unsupported.sse"),
    "\n"
);
const RESPONSES_STREAM_AFTER_MODEL_UNSUPPORTED_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/stream_after_model_unsupported.sse"),
    "\n"
);
const RESPONSES_FAILED_MODEL_UNSUPPORTED_SSE: &str = concat!(
    include_str!("../fixtures/responses/http_sse/failed_model_unsupported.sse"),
    "\n"
);

const WEBSOCKET_HISTORY_RATE_LIMITED: &str =
    include_str!("../fixtures/responses/websocket/history_rate_limited.json");
const WEBSOCKET_RATE_LIMITED: &str =
    include_str!("../fixtures/responses/websocket/rate_limited.json");
const WEBSOCKET_TOKEN_REVOKED: &str =
    include_str!("../fixtures/responses/websocket/token_revoked.json");
const WEBSOCKET_FIRST_ACCOUNT_LIMITED: &str =
    include_str!("../fixtures/responses/websocket/first_account_limited.json");
const WEBSOCKET_SECOND_ACCOUNT_LIMITED: &str =
    include_str!("../fixtures/responses/websocket/second_account_limited.json");
mod responses_http;
mod responses_recovery;
mod responses_websocket;
mod usage_logging;

async fn test_app_state_with_pool(config: &AppConfig, pool: PgPool) -> AppState {
    test_app_state_with_pool_and_usage_record_options(
        config.clone(),
        pool,
        UsageRecordOptions::from_config(config),
    )
    .await
}

async fn test_app_state_with_pool_and_usage_record_options(
    config: AppConfig,
    pool: PgPool,
    usage_record_options: UsageRecordOptions,
) -> AppState {
    let redis = create_test_redis("chat-upstream").await;
    test_app_state_with_storage(config, pool, redis, usage_record_options).await
}

async fn test_app_state_with_storage(
    config: AppConfig,
    pool: PgPool,
    redis: codex_proxy_rs::infra::redis::RedisConnection,
    usage_record_options: UsageRecordOptions,
) -> AppState {
    insert_model_snapshot(&redis).await;
    let stores = background_task_stores(pool, redis);
    let services = Services::try_with_usage_record_options(
        &config,
        stores,
        crate::support::fingerprint::runtime_test_fingerprint(),
        usage_record_options,
    )
    .expect("failed to build runtime services with configured TLS transport");
    services
        .initialize_hot_path_state()
        .await
        .expect("hot path state should initialize");
    AppState::from(&services)
}

pub(crate) async fn test_app_with_account(
    base_url: String,
) -> (
    axum::Router,
    String,
    crate::support::storage::TestDatabaseGuard,
) {
    let (pool, dir) = init_test_db("openai-chat-upstream").await;
    let api_key = insert_client_api_key(&pool).await;
    insert_account(&pool).await;
    let state = test_app_state_with_pool(&test_config(test_database_url(), base_url), pool).await;
    state
        .services
        .account_pool
        .restore_from_store()
        .await
        .expect("account pool should restore");
    (router::router().with_state(state), api_key, dir)
}

async fn test_app_with_account_and_pool(
    base_url: String,
) -> (
    axum::Router,
    String,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    test_app_with_account_pool_config(base_url, |_| {}).await
}

async fn test_app_with_account_pool_and_affinity(
    base_url: String,
    response_id: &str,
) -> (
    axum::Router,
    String,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    test_app_with_account_pool_config_and_affinity(base_url, |_| {}, Some(response_id)).await
}

async fn test_app_without_accounts(
    base_url: String,
) -> (
    axum::Router,
    String,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    let (pool, dir) = init_test_db("openai-responses-no-accounts").await;
    let api_key = insert_client_api_key(&pool).await;
    let state =
        test_app_state_with_pool(&test_config(test_database_url(), base_url), pool.clone()).await;
    state
        .services
        .account_pool
        .restore_from_store()
        .await
        .expect("empty account pool should restore");
    (router::router().with_state(state), api_key, pool, dir)
}

async fn test_app_with_account_pool_and_telemetry(
    base_url: String,
) -> (
    axum::Router,
    String,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    test_app_with_account_pool_config(base_url, |config| {
        config.telemetry.enabled = true;
    })
    .await
}

async fn test_app_with_account_pool_and_disabled_telemetry(
    base_url: String,
) -> (
    axum::Router,
    String,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    let (pool, dir) = init_test_db("openai-record-disabled-logging").await;
    let api_key = insert_client_api_key(&pool).await;
    insert_account(&pool).await;
    let state =
        test_app_state_with_pool(&test_config(test_database_url(), base_url), pool.clone()).await;
    state
        .services
        .account_pool
        .restore_from_store()
        .await
        .expect("account pool should restore");
    (router::router().with_state(state), api_key, pool, dir)
}

async fn test_app_with_account_pool_and_telemetry_capture_body(
    base_url: String,
) -> (
    axum::Router,
    String,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    let (pool, dir) = init_test_db("openai-record-capture-body").await;
    let api_key = insert_client_api_key(&pool).await;
    insert_account(&pool).await;
    let mut config = test_config(test_database_url(), base_url);
    config.telemetry.enabled = true;
    let usage_record_options = UsageRecordOptions {
        enabled: true,
        capture_body: true,
    };
    let state = test_app_state_with_pool_and_usage_record_options(
        config,
        pool.clone(),
        usage_record_options,
    )
    .await;
    state
        .services
        .account_pool
        .restore_from_store()
        .await
        .expect("account pool should restore");
    (router::router().with_state(state), api_key, pool, dir)
}

async fn test_app_with_account_pool_config(
    base_url: String,
    configure: impl FnOnce(&mut AppConfig),
) -> (
    axum::Router,
    String,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    test_app_with_account_pool_config_and_affinity(base_url, configure, None).await
}

async fn test_app_with_account_pool_config_and_affinity(
    base_url: String,
    configure: impl FnOnce(&mut AppConfig),
    affinity_response_id: Option<&str>,
) -> (
    axum::Router,
    String,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    let (pool, dir) = init_test_db("openai-record-affinity").await;
    let api_key = insert_client_api_key(&pool).await;
    insert_account(&pool).await;
    let mut config = test_config(test_database_url(), base_url);
    configure(&mut config);
    let state = test_app_state_with_pool(&config, pool.clone()).await;
    if let Some(response_id) = affinity_response_id {
        insert_session_affinity(&state.services.session_affinity, response_id, "acct_chat").await;
    }
    state
        .services
        .account_pool
        .restore_from_store()
        .await
        .expect("account pool should restore");
    (router::router().with_state(state), api_key, pool, dir)
}

async fn test_app_with_two_accounts_and_affinity(
    base_url: String,
) -> (
    axum::Router,
    String,
    crate::support::storage::TestDatabaseGuard,
) {
    let (pool, dir) = init_test_db("openai-responses-affinity").await;
    let api_key = insert_client_api_key(&pool).await;
    insert_named_account(&pool, "acct_a", "access-default", "chatgpt-default").await;
    insert_named_account(&pool, "acct_z", "access-affinity", "chatgpt-affinity").await;
    let now = Utc::now();
    let state = test_app_state_with_pool(&test_config(test_database_url(), base_url), pool).await;
    state
        .services
        .session_affinity
        .record(
            "resp_previous".to_string(),
            SessionAffinityEntry {
                account_id: "acct_z".to_string(),
                conversation_id: "conv_affinity".to_string(),
                turn_state: Some("turn_affinity".to_string()),
                instructions_hash: None,
                input_tokens: None,
                function_call_ids: Vec::new(),
                variant_hash: None,
                continuation_scope: codex_proxy_rs::upstream::openai::protocol::responses::PreviousResponseScope::Persisted,
                created_at: now,
            },
        )
        .await
        .unwrap();
    state
        .services
        .account_pool
        .restore_from_store()
        .await
        .expect("account pool should restore");
    (router::router().with_state(state), api_key, dir)
}

async fn test_app_with_two_accounts(
    base_url: String,
) -> (
    axum::Router,
    String,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    let (app, _state, api_key, pool, dir) = test_app_with_two_accounts_and_state(base_url).await;
    (app, api_key, pool, dir)
}

async fn test_app_with_two_accounts_and_telemetry(
    base_url: String,
) -> (
    axum::Router,
    String,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    let (app, _state, api_key, pool, dir) =
        test_app_with_two_accounts_and_state_config(base_url, |config| {
            config.telemetry.enabled = true;
        })
        .await;
    (app, api_key, pool, dir)
}

async fn test_app_with_two_accounts_and_state(
    base_url: String,
) -> (
    axum::Router,
    AppState,
    String,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    test_app_with_two_accounts_and_state_config(base_url, |_| {}).await
}

async fn test_app_with_two_accounts_and_state_config(
    base_url: String,
    configure: impl FnOnce(&mut AppConfig),
) -> (
    axum::Router,
    AppState,
    String,
    PgPool,
    crate::support::storage::TestDatabaseGuard,
) {
    let (pool, dir) = init_test_db("openai-responses-fallback").await;
    let api_key = insert_client_api_key(&pool).await;
    insert_named_account(&pool, "acct_primary", "access-primary", "chatgpt-primary").await;
    insert_named_account(
        &pool,
        "acct_secondary",
        "access-secondary",
        "chatgpt-secondary",
    )
    .await;
    let mut config = test_config(test_database_url(), base_url);
    configure(&mut config);
    let state = test_app_state_with_pool(&config, pool.clone()).await;
    state
        .services
        .account_pool
        .restore_from_store()
        .await
        .expect("account pool should restore");
    (
        router::router().with_state(state.clone()),
        state,
        api_key,
        pool,
        dir,
    )
}

async fn test_app_with_ranked_accounts(
    base_url: String,
    account_count: usize,
) -> (
    axum::Router,
    String,
    crate::support::storage::TestDatabaseGuard,
) {
    let (pool, dir) = init_test_db("openai-responses-full-candidate-ledger").await;
    let api_key = insert_client_api_key(&pool).await;
    for index in 0..account_count {
        let account_id = format!("acct-{index}");
        insert_named_account(
            &pool,
            &account_id,
            &format!("access-{index}"),
            &format!("chatgpt-{index}"),
        )
        .await;
        sqlx::query(
            "insert into account_usage (account_id, request_count, window_request_count) values ($1, $2, $2)",
        )
        .bind(&account_id)
        .bind(i64::try_from(index).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    }
    let mut config = test_config(test_database_url(), base_url);
    config.auth.rotation_strategy = "quota_reset_priority".to_string();
    let state = test_app_state_with_pool(&config, pool).await;
    state
        .services
        .account_pool
        .restore_from_store()
        .await
        .expect("account pool should restore");
    (router::router().with_state(state), api_key, dir)
}

async fn seed_openai_admin_key(pool: &PgPool, key: &str) {
    sqlx::query(
        "insert into runtime_settings (
           id, refresh_margin_seconds, refresh_concurrency,
           max_concurrent_per_account, request_interval_ms,
           rotation_strategy, admin_api_key_hash, updated_at
         ) values (1, 3600, 2, 3, 0, 'smart', $1, now())
         on conflict (id) do update
         set admin_api_key_hash = excluded.admin_api_key_hash,
             updated_at = excluded.updated_at",
    )
    .bind(hash_credential(key))
    .execute(pool)
    .await
    .unwrap();
}

async fn update_admin_account_status(app: &axum::Router, account_id: &str, status: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/update")
                .header("content-type", "application/json")
                .header("x-api-key", "admin-status-cycle")
                .body(Body::from(
                    json!({"id": account_id, "status": status}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn insert_account(pool: &PgPool) {
    let now = Utc::now();
    sqlx::query(
        "insert into accounts (id, email, chatgpt_account_id, chatgpt_user_id, access_token, access_token_expires_at, status, added_at, updated_at) values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind("acct_chat")
    .bind("user@example.com")
    .bind("chatgpt-account")
    .bind("chatgpt-user")
    .bind("access-secret")
    .bind(now + Duration::days(36500))
    .bind("active")
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_model_snapshot(redis: &codex_proxy_rs::infra::redis::RedisConnection) {
    let models = json!([
        {
            "id": "gpt-5.5",
            "displayName": "GPT 5.5",
            "description": "Test model",
            "isDefault": false,
            "supportedReasoningEfforts": [
                {"reasoningEffort": "low", "description": "low"},
                {"reasoningEffort": "medium", "description": "medium"},
                {"reasoningEffort": "high", "description": "high"}
            ],
            "defaultReasoningEffort": "medium",
            "inputModalities": ["text", "image"],
            "outputModalities": ["text"],
            "supportsPersonality": false,
            "upgrade": null,
            "source": "test"
        }
    ]);
    let value = json!({"models": models, "fetchedAt": Utc::now()});
    let mut connection = redis.manager();
    let _: usize = redis::cmd("HSET")
        .arg(redis.key("models:plan_snapshots"))
        .arg("plus")
        .arg(value.to_string())
        .query_async(&mut connection)
        .await
        .unwrap();
}

async fn insert_session_affinity(
    service: &codex_proxy_rs::dispatch::affinity::SessionAffinityService,
    response_id: &str,
    account_id: &str,
) {
    service
        .record(
            response_id.to_string(),
            SessionAffinityEntry {
                account_id: account_id.to_string(),
                conversation_id: format!("conv_{response_id}"),
                turn_state: Some("turn-stale".to_string()),
                instructions_hash: None,
                input_tokens: None,
                function_call_ids: Vec::new(),
                variant_hash: None,
                continuation_scope: codex_proxy_rs::upstream::openai::protocol::responses::PreviousResponseScope::Persisted,
                created_at: Utc::now(),
            },
        )
        .await
        .unwrap();
}

async fn insert_named_account(
    pool: &PgPool,
    id: &str,
    access_token: &str,
    chatgpt_account_id: &str,
) {
    let now = Utc::now();
    sqlx::query(
        "insert into accounts (id, email, chatgpt_account_id, chatgpt_user_id, access_token, access_token_expires_at, status, added_at, updated_at) values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(format!("{id}@example.com"))
    .bind(chatgpt_account_id)
    .bind(format!("user-{id}"))
    .bind(access_token)
    .bind(now + Duration::days(36500))
    .bind("active")
    .bind(now)
    .bind(now)
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

struct CapturedWebSocketRequest {
    headers: Vec<(String, String)>,
    payload: Value,
}

struct WebSocketSequenceCapture {
    payload: Value,
    retry_attempted: bool,
}

async fn spawn_single_websocket_sequence_upstream(
    messages: Vec<String>,
) -> (String, tokio::task::JoinHandle<WebSocketSequenceCapture>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let payload = websocket.next().await.unwrap().unwrap();
        let payload = serde_json::from_str::<Value>(&payload.into_text().unwrap())
            .expect("websocket payload should be json");

        send_websocket_messages(&mut websocket, &messages).await;
        drop(websocket);

        let retry_attempted = timeout(StdDuration::from_millis(500), listener.accept())
            .await
            .is_ok();
        WebSocketSequenceCapture {
            payload,
            retry_attempted,
        }
    });
    (base_url, server)
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
                    Some(_) => {}
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

async fn send_websocket_messages(
    websocket: &mut tokio_tungstenite::WebSocketStream<TcpStream>,
    messages: &[String],
) {
    for (index, message) in messages.iter().enumerate() {
        websocket
            .send(Message::Text(message.clone().into()))
            .await
            .unwrap();
        if index + 1 < messages.len() {
            tokio::time::sleep(StdDuration::from_millis(25)).await;
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

fn assert_openai_error_body(
    body: &str,
    expected_error_type: &str,
    expected_code: &str,
    expected_fragments: &[&str],
) {
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

fn responses_json_request(api_key: &str, body: &Value) -> Request<Body> {
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

struct ResponseUsageRecordSnapshot {
    level: String,
    request_id: Option<String>,
    account_id: Option<String>,
    route: Option<String>,
    model: Option<String>,
    requested_model: Option<String>,
    upstream_model: Option<String>,
    transport: Option<String>,
    status_code: Option<i64>,
    client_status_code: Option<i64>,
    upstream_status_code: Option<i64>,
    failure_class: Option<String>,
    attempt_index: Option<i64>,
    response_id: Option<String>,
    first_token_ms: Option<i64>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    metadata_json: String,
}

#[derive(sqlx::FromRow)]
struct OpsErrorLogSnapshot {
    request_id: Option<String>,
    account_id: Option<String>,
    route: Option<String>,
    model: Option<String>,
    transport: Option<String>,
    status_code: Option<i32>,
    client_status_code: Option<i32>,
    upstream_status_code: Option<i32>,
    failure_class: Option<String>,
    attempt_index: Option<i64>,
    response_id: Option<String>,
    metadata_json: String,
}

impl From<OpsErrorLogSnapshot> for ResponseUsageRecordSnapshot {
    fn from(snapshot: OpsErrorLogSnapshot) -> Self {
        Self {
            level: "error".to_string(),
            request_id: snapshot.request_id,
            account_id: snapshot.account_id,
            route: snapshot.route,
            model: snapshot.model,
            requested_model: None,
            upstream_model: None,
            transport: snapshot.transport,
            status_code: snapshot.status_code.map(i64::from),
            client_status_code: snapshot.client_status_code.map(i64::from),
            upstream_status_code: snapshot.upstream_status_code.map(i64::from),
            failure_class: snapshot.failure_class,
            attempt_index: snapshot.attempt_index,
            response_id: snapshot.response_id,
            first_token_ms: None,
            input_tokens: None,
            output_tokens: None,
            metadata_json: snapshot.metadata_json,
        }
    }
}

async fn latest_response_usage_record(pool: &PgPool) -> ResponseUsageRecordSnapshot {
    latest_usage_record(pool, "v1.response").await
}

async fn latest_response_ops_error_log(pool: &PgPool) -> ResponseUsageRecordSnapshot {
    latest_response_ops_error_log_with_message(pool, None).await
}

async fn latest_response_upstream_ops_error_log(pool: &PgPool) -> ResponseUsageRecordSnapshot {
    latest_response_ops_error_log_with_message(pool, Some("v1 responses upstream request failed"))
        .await
}

async fn latest_response_ops_error_log_with_message(
    pool: &PgPool,
    message: Option<&str>,
) -> ResponseUsageRecordSnapshot {
    let row = sqlx::query_as::<_, OpsErrorLogSnapshot>(
        "select request_id, account_id, route, model, transport, status_code,
                client_status_code, upstream_status_code, failure_class, attempt_index, response_id,
                metadata_json::text as metadata_json
         from ops_error_logs
         where kind = $1 and ($2::text is null or message = $2)
         order by created_at desc, id desc
         limit 1",
    )
    .bind("v1.response")
    .bind(message)
    .fetch_optional(pool)
    .await
    .unwrap();
    row.unwrap_or_else(|| panic!("expected a v1.response ops error log"))
        .into()
}

async fn latest_usage_record(pool: &PgPool, kind: &str) -> ResponseUsageRecordSnapshot {
    let events = PgUsageRecordStore::new(pool.clone())
        .list_recent(
            UsageRecordFilter {
                kind: Some(kind.to_string()),
                ..UsageRecordFilter::default()
            },
            1,
        )
        .await
        .unwrap();
    let event = events
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("expected a {kind} usage record"));
    ResponseUsageRecordSnapshot {
        level: "info".to_string(),
        request_id: event.request_id,
        account_id: Some(event.account_id),
        route: event.route,
        model: Some(event.model),
        requested_model: event.requested_model,
        upstream_model: event.upstream_model,
        transport: event.transport,
        status_code: Some(event.status_code),
        client_status_code: None,
        upstream_status_code: None,
        failure_class: None,
        attempt_index: event.attempt_index,
        response_id: event.response_id,
        first_token_ms: event.first_token_ms,
        input_tokens: event.input_tokens,
        output_tokens: event.output_tokens,
        metadata_json: event.metadata.to_string(),
    }
}

async fn response_usage_record_count(pool: &PgPool) -> i64 {
    let (count,): (i64,) = sqlx::query_as("select count(*) from usage_records where kind = $1")
        .bind("v1.response")
        .fetch_one(pool)
        .await
        .unwrap();
    count
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
        let mut extra_headers = String::new();
        for (name, value) in headers {
            extra_headers.push_str(name);
            extra_headers.push_str(": ");
            extra_headers.push_str(value);
            extra_headers.push_str("\r\n");
        }
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
    let mut config = base_test_config(database_url);
    config.api.base_url = base_url;
    config
}
