use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, Mutex},
};
use tower::ServiceExt;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::{
    codex::accounts::cookies::repository::CookieRepository,
    codex::gateway::conversation_identity::build_conversation_identity,
};

use crate::support::{
    response_json, response_text,
    upstream::{
        build_imported_app, build_imported_app_with_accounts_and_config, enable_runtime_logging,
        fetch_v1_event_log, ImportAccount,
    },
};

const COMPLETED_USAGE_SSE: &str =
    include_str!("../fixtures/responses/http_sse/completed_usage.sse");
const COMPLETED_IMAGE_USAGE_SSE: &str =
    include_str!("../fixtures/responses/http_sse/completed_image_usage.sse");
const COMPLETED_FIELDS_SSE: &str =
    include_str!("../fixtures/responses/http_sse/completed_fields.sse");
const COMPLETED_REASONING_INCLUDE_SSE: &str =
    include_str!("../fixtures/responses/http_sse/completed_reasoning_include.sse");
const TEXT_DELTAS_COMPLETED_SSE: &str =
    include_str!("../fixtures/responses/http_sse/text_deltas_completed.sse");
const DONE_ITEM_COMPLETED_SSE: &str =
    include_str!("../fixtures/responses/http_sse/done_item_completed.sse");
const STREAM_USAGE_SSE: &str = include_str!("../fixtures/responses/http_sse/stream_usage.sse");
const DEFAULT_STREAM_SSE: &str = include_str!("../fixtures/responses/http_sse/default_stream.sse");
const EMPTY_COMPLETED_SSE: &str =
    include_str!("../fixtures/responses/http_sse/empty_completed.sse");
const COMPLETED_TUPLE_OBJECT_SSE: &str =
    include_str!("../fixtures/responses/http_sse/completed_tuple_object.sse");
const TUPLE_RECONVERT_RESPONSE_GOLDEN: &str =
    include_str!("../fixtures/responses/golden/tuple_reconvert_response.json");
const TUPLE_RECONVERT_STREAM_GOLDEN: &str = concat!(
    include_str!("../fixtures/responses/golden/tuple_reconvert_stream.sse"),
    "\n"
);

#[tokio::test]
async fn v1_responses_should_reject_invalid_json_without_upstream_request() {
    let server = MockServer::start().await;
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
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "invalid_request");
    let requests = server.received_requests().await.unwrap();
    assert!(requests.is_empty());
}

#[tokio::test]
async fn v1_responses_should_reject_non_object_json_without_upstream_request() {
    let server = MockServer::start().await;
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
                .body(Body::from("[]"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "invalid_request");
    let requests = server.received_requests().await.unwrap();
    assert!(requests.is_empty());
}

#[tokio::test]
async fn v1_responses_should_return_responses_error_when_no_accounts_are_available() {
    let server = MockServer::start().await;
    let imported = build_imported_app_with_accounts_and_config(server.uri(), &[], |_| {}).await;

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
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response_json(response).await;
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "server_error");
    assert_eq!(body["error"]["code"], "no_available_accounts");
}

#[tokio::test]
async fn v1_responses_should_skip_event_log_when_logging_disabled() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_USAGE_SSE),
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
                .header("x-request-id", "req_logs_disabled")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let stored_count: (i64,) = sqlx::query_as(
        "select count(*) from event_logs where request_id = ? and kind = 'v1.response'",
    )
    .bind("req_logs_disabled")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    assert_eq!(stored_count.0, 0);
}

#[tokio::test]
async fn v1_responses_should_honor_explicit_http_sse_transport() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(DEFAULT_STREAM_SSE),
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
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"use_websocket":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert!(body.contains("event: response.output_text.delta"));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "POST");
    let upstream_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(upstream_body["model"], "gpt-5.5");
    assert!(upstream_body.get("use_websocket").is_none());
}

#[tokio::test]
async fn v1_responses_should_stagger_same_account_requests_before_sending_upstream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let request_times = Arc::new(Mutex::new(Vec::new()));
    let request_times_for_server = Arc::clone(&request_times);
    let (first_seen_tx, first_seen_rx) = oneshot::channel();
    let (release_first_tx, release_first_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut first_stream, _) = listener.accept().await.unwrap();
        request_times_for_server.lock().await.push(Instant::now());
        first_seen_tx.send(()).unwrap();
        let _first_request = read_http_request(&mut first_stream).await;

        let (mut second_stream, _) = listener.accept().await.unwrap();
        request_times_for_server.lock().await.push(Instant::now());
        let _second_request = read_http_request(&mut second_stream).await;
        write_http_sse_response(&mut second_stream, COMPLETED_USAGE_SSE).await;

        release_first_rx.await.unwrap();
        write_http_sse_response(&mut first_stream, COMPLETED_USAGE_SSE).await;
    });
    let imported = build_imported_app_with_accounts_and_config(
        format!("http://{addr}"),
        &[ImportAccount {
            id: "acct_stagger",
            account_id: "chatgpt-stagger",
            token: "access-stagger",
            refresh_token: "refresh-stagger",
        }],
        |config| {
            config.auth.max_concurrent_per_account = 2;
            config.auth.request_interval_ms = 300;
        },
    )
    .await;

    let first_app = imported.app.clone();
    let first_api_key = imported.client_api_key.clone();
    let first_response = tokio::spawn(async move {
        first_app
            .oneshot(v1_response_request(&first_api_key, "req_stagger_first"))
            .await
            .unwrap()
    });
    first_seen_rx.await.unwrap();

    let second = imported
        .app
        .clone()
        .oneshot(v1_response_request(
            &imported.client_api_key,
            "req_stagger_second",
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    release_first_tx.send(()).unwrap();
    let first = first_response.await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    server.await.unwrap();

    let times = request_times.lock().await;
    assert_eq!(times.len(), 2);
    let elapsed = times[1].duration_since(times[0]);
    assert!(
        elapsed >= Duration::from_millis(180),
        "second upstream request was sent too early: {elapsed:?}"
    );
}

#[tokio::test]
async fn v1_responses_should_use_imported_account_and_record_usage() {
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
                .set_body_string(COMPLETED_USAGE_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;
    enable_runtime_logging(&imported.app).await;

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
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false}"#,
                ))
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
async fn v1_responses_should_record_image_generation_usage_when_tool_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_IMAGE_USAGE_SSE),
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
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false,"tools":[{"type":"image_generation"}]}"#,
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
        .bind("acct_imported")
        .fetch_one(&imported.pool)
        .await
        .unwrap();

    assert_eq!(usage, (1, 7, 4, 2, 31, 9, 1, 0, 31, 9, 1, 0));
}

#[tokio::test]
async fn v1_responses_should_record_failed_image_generation_attempt_when_tool_has_no_output() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_USAGE_SSE),
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
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false,"tools":[{"type":"image_generation"}]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let usage: (i64, i64, i64, i64) = sqlx::query_as(
        "select image_request_count, image_request_failed_count, window_image_request_count, window_image_request_failed_count from account_usage where account_id = ?",
    )
    .bind("acct_imported")
    .fetch_one(&imported.pool)
    .await
    .unwrap();

    assert_eq!(usage, (0, 1, 0, 1));
}

#[tokio::test]
async fn v1_responses_should_scope_upstream_cookie_by_codex_response_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_USAGE_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;
    let cookie_domain = reqwest::Url::parse(&server.uri())
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();
    let cookie_repo = CookieRepository::new(imported.pool.clone(), imported.secret_box.clone());
    cookie_repo
        .capture_set_cookie(
            "acct_imported",
            &format!("cf_clearance=root; Domain={cookie_domain}; Path=/"),
        )
        .await
        .unwrap();
    cookie_repo
        .capture_set_cookie(
            "acct_imported",
            &format!("cf_clearance=codex; Domain={cookie_domain}; Path=/codex"),
        )
        .await
        .unwrap();

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
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false,"use_websocket":false}"#,
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
async fn v1_responses_should_passively_cache_rate_limit_headers() {
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
                .set_body_string(COMPLETED_USAGE_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;
    sqlx::query(
        r#"update accounts set quota_json = '{"credits":{"has_credits":true,"unlimited":false,"balance":12}}', quota_verify_required = 1 where id = ?"#,
    )
    .bind("acct_imported")
    .execute(&imported.pool)
    .await
    .unwrap();

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
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let stored: (String, i64, Option<String>, i64) = sqlx::query_as(
        "select quota_json, quota_limit_reached, quota_cooldown_until, quota_verify_required from accounts where id = ?",
    )
    .bind("acct_imported")
    .fetch_one(&imported.pool)
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
        .bind("acct_imported")
        .fetch_one(&imported.pool)
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
async fn v1_responses_should_record_empty_response_attempts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(EMPTY_COMPLETED_SSE),
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
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let usage: (i64, i64, i64, i64) = sqlx::query_as(
        "select request_count, empty_response_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_imported")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    assert_eq!(usage, (3, 3, 0, 0));
}

#[tokio::test]
async fn v1_responses_should_forward_parity_fields_and_context_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .and(header("x-codex-turn-state", "turn-body"))
        .and(header("x-codex-turn-metadata", "meta-body"))
        .and(header("x-codex-beta-features", "beta-body"))
        .and(header("x-responsesapi-include-timing-metrics", "true"))
        .and(header("version", "2026-06-12"))
        .and(header("x-codex-parent-thread-id", "parent-body"))
        .and(header("x-openai-subagent", "review"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_FIELDS_SSE),
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
                .header("x-codex-turn-state", "turn-header")
                .header("x-codex-turn-metadata", "meta-header")
                .header("x-codex-beta-features", "beta-header")
                .header("x-codex-window-id", "window-header")
                .header("x-codex-parent-thread-id", "parent-header")
                .header("x-openai-subagent", "review")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5-fast",
                        "stream": false,
                        "input": [],
                        "reasoning": {"effort": "high"},
                        "service_tier": "fast",
                        "tool_choice": {
                            "type": "function",
                            "function": {"name": "lookup"}
                        },
                        "parallel_tool_calls": true,
                        "text": {
                            "format": {
                                "type": "json_schema",
                                "name": "Answer",
                                "schema": {"type": "object"},
                                "strict": true
                            }
                        },
                        "prompt_cache_key": "pcache",
                        "include": ["reasoning.encrypted_content"],
                        "client_metadata": {
                            "safe": "yes"
                        },
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

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_fields");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let upstream_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let identity =
        build_conversation_identity(Some("pcache"), Some("window-body"), "acct_imported");
    assert_eq!(
        upstream_body["prompt_cache_key"],
        identity.conversation_id.unwrap()
    );
    assert_eq!(upstream_body["model"], "gpt-5.5");
    assert_eq!(upstream_body["service_tier"], "priority");
    assert_eq!(upstream_body["client_metadata"]["safe"], "yes");
    assert_eq!(
        upstream_body["client_metadata"]["x-openai-subagent"],
        "review"
    );
    assert_eq!(
        upstream_body["client_metadata"]["x-codex-window-id"],
        identity.window_id.unwrap()
    );
    assert_eq!(
        upstream_body["client_metadata"]["x-codex-turn-metadata"],
        "meta-body"
    );
    assert_eq!(
        upstream_body["client_metadata"]["x-codex-parent-thread-id"],
        "parent-body"
    );
    let installation_id = upstream_body["client_metadata"]["x-codex-installation-id"]
        .as_str()
        .unwrap();
    assert!(uuid::Uuid::parse_str(installation_id).is_ok());
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
async fn v1_responses_should_convert_tuple_schema_before_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_FIELDS_SSE),
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
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "stream": false,
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
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = server.received_requests().await.unwrap();
    let upstream_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let schema = &upstream_body["text"]["format"]["schema"];
    assert!(schema["properties"]["point"].get("prefixItems").is_none());
    assert!(schema["properties"]["point"].get("items").is_none());
    assert_eq!(schema["additionalProperties"], false);
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
async fn v1_responses_should_reconvert_tuple_schema_output_for_client() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_TUPLE_OBJECT_SSE),
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
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "stream": false,
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
    let expected: Value = serde_json::from_str(TUPLE_RECONVERT_RESPONSE_GOLDEN).unwrap();
    assert_eq!(body, expected);
}

#[tokio::test]
async fn v1_responses_stream_should_reconvert_tuple_schema_output_for_client() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_TUPLE_OBJECT_SSE),
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
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "stream": true,
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
    assert_eq!(body, TUPLE_RECONVERT_STREAM_GOLDEN);
}

#[tokio::test]
async fn v1_responses_review_route_should_force_review_subagent_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("x-openai-subagent", "review"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_FIELDS_SSE),
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
                .uri("/v1/responses/review")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "stream": false,
                        "use_websocket": false,
                        "input": []
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let requests = server.received_requests().await.unwrap();
    let post_request = requests
        .iter()
        .find(|request| request.method.as_str() == "POST" && !request.body.is_empty())
        .unwrap();
    let upstream_body: Value = serde_json::from_slice(&post_request.body).unwrap();
    assert_eq!(
        upstream_body["client_metadata"]["x-openai-subagent"],
        "review"
    );
}

#[tokio::test]
async fn v1_responses_compact_should_post_json_to_codex_compact_upstream() {
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
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/compact")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
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
                        "store": true,
                        "prompt_cache_key": "must_not_forward"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["output"][0]["content"][0]["text"], "compacted");

    let requests = server.received_requests().await.unwrap();
    let compact_request = requests
        .iter()
        .find(|request| request.url.path() == "/codex/responses/compact")
        .unwrap();
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
async fn v1_responses_compact_should_return_rate_limit_error_when_fallback_is_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses/compact"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "code": "rate_limit_exceeded",
                "message": "compact quota reached"
            }
        })))
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/compact")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
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

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = response_json(response).await;
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "rate_limit_error");
    assert_eq!(body["error"]["code"], "rate_limit_exceeded");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("compact quota reached"));
}

#[tokio::test]
async fn v1_responses_compact_should_return_responses_error_when_no_accounts_are_available() {
    let server = MockServer::start().await;
    let imported = build_imported_app_with_accounts_and_config(server.uri(), &[], |_| {}).await;

    let response = imported
        .app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/compact")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
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

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response_json(response).await;
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "server_error");
    assert_eq!(body["error"]["code"], "no_available_accounts");
}

#[tokio::test]
async fn v1_responses_should_include_encrypted_reasoning_by_default() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_REASONING_INCLUDE_SSE),
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
                .body(Body::from(
                    r#"{"model":"gpt-5.5","stream":false,"input":[],"reasoning":{"effort":"high"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_reasoning_include");
    let requests = server.received_requests().await.unwrap();
    let post_request = requests
        .iter()
        .find(|request| request.method.as_str() == "POST")
        .unwrap();
    let upstream_body: Value = serde_json::from_slice(&post_request.body).unwrap();
    assert_eq!(
        upstream_body["include"],
        json!(["reasoning.encrypted_content"])
    );
    assert_eq!(upstream_body["reasoning"]["summary"], "auto");
}

#[tokio::test]
async fn v1_responses_should_not_add_reasoning_include_when_client_include_is_non_empty() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_REASONING_INCLUDE_SSE),
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
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "stream": false,
                        "use_websocket": false,
                        "input": [],
                        "reasoning": {"effort": "high"},
                        "include": ["file_search_call.results"]
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
        upstream_body["include"],
        json!(["file_search_call.results"])
    );
}

#[tokio::test]
async fn v1_responses_should_sanitize_reasoning_and_compaction_input_before_upstream() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(COMPLETED_USAGE_SSE),
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
async fn v1_responses_should_reconstruct_non_stream_output_text_from_sse_deltas() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(TEXT_DELTAS_COMPLETED_SSE),
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
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_text");
    assert_eq!(
        body["output"][0]["content"][0]["text"],
        "你好，我是一个中文助手。"
    );
    assert_eq!(body["output_text"], "你好，我是一个中文助手。");
    assert_eq!(body["output"][0]["role"], "assistant");
}

#[tokio::test]
async fn v1_responses_should_use_done_output_items_when_completed_output_is_empty() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(DONE_ITEM_COMPLETED_SSE),
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
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_item");
    assert_eq!(body["output"][0]["content"][0]["text"], "来自 done item");
    assert_eq!(body["output_text"], "来自 done item");
}

#[tokio::test]
async fn v1_responses_should_passthrough_stream_and_record_usage_and_log() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(STREAM_USAGE_SSE),
        )
        .mount(&server)
        .await;
    let imported = build_imported_app(server.uri()).await;
    enable_runtime_logging(&imported.app).await;

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

#[tokio::test]
async fn v1_responses_stream_should_close_http_sse_upstream_when_client_disconnects() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _request = read_http_request(&mut stream).await;
        write_chunked_http_sse_headers(&mut stream).await;
        write_http_chunk(
            &mut stream,
            b"event: response.output_text.delta\ndata: {\"delta\":\"hello\"}\n\n",
        )
        .await;

        tokio::time::timeout(
            Duration::from_secs(2),
            wait_for_http_sse_upstream_disconnect(&mut stream),
        )
        .await
        .is_ok()
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

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
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":true,"use_websocket":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let first_chunk = tokio::time::timeout(Duration::from_secs(1), body.next())
        .await
        .expect("first SSE chunk should arrive before disconnect")
        .expect("stream should yield a first chunk")
        .expect("chunk should be readable");
    let first_sse = String::from_utf8(first_chunk.to_vec()).unwrap();
    assert!(first_sse.contains("event: response.output_text.delta"));

    drop(body);
    assert!(
        server.await.unwrap(),
        "dropping the downstream stream should close the HTTP SSE upstream socket"
    );
}

#[tokio::test]
async fn v1_responses_should_default_to_streaming_when_stream_is_omitted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(DEFAULT_STREAM_SSE),
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
                .body(Body::from(r#"{"model":"gpt-5.5","input":[]}"#))
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
}

fn v1_response_request(api_key: &str, request_id: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .header("x-request-id", request_id)
        .body(Body::from(
            r#"{"model":"gpt-5.5","input":[],"stream":false,"use_websocket":false}"#,
        ))
        .unwrap()
}

async fn read_http_request(stream: &mut TcpStream) -> String {
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

async fn write_http_sse_response(stream: &mut TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
}

async fn write_chunked_http_sse_headers(stream: &mut TcpStream) {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
}

async fn write_http_chunk(stream: &mut TcpStream, chunk: &[u8]) {
    stream
        .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
        .await
        .unwrap();
    stream.write_all(chunk).await.unwrap();
    stream.write_all(b"\r\n").await.unwrap();
    stream.flush().await.unwrap();
}

async fn wait_for_http_sse_upstream_disconnect(stream: &mut TcpStream) {
    let mut buffer = [0u8; 1024];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}
