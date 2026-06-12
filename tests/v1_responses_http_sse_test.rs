use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tower::ServiceExt;
use wiremock::{
    matchers::{body_json, header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::codex::cookies::repository::CookieRepository;

mod common;

use common::{
    response_json, response_text,
    upstream::{build_imported_app, fetch_v1_event_log},
};

const COMPLETED_USAGE_SSE: &str = include_str!("fixtures/v1_responses_completed_usage.sse");
const COMPLETED_FIELDS_SSE: &str = include_str!("fixtures/v1_responses_completed_fields.sse");
const COMPLETED_REASONING_INCLUDE_SSE: &str =
    include_str!("fixtures/v1_responses_completed_reasoning_include.sse");
const TEXT_DELTAS_COMPLETED_SSE: &str =
    include_str!("fixtures/v1_responses_text_deltas_completed.sse");
const DONE_ITEM_COMPLETED_SSE: &str = include_str!("fixtures/v1_responses_done_item_completed.sse");
const STREAM_USAGE_SSE: &str = include_str!("fixtures/v1_responses_stream_usage.sse");
const DEFAULT_STREAM_SSE: &str = include_str!("fixtures/v1_responses_default_stream.sse");

#[tokio::test]
async fn v1_responses_should_use_imported_account_and_record_usage() {
    let server = MockServer::start().await;
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
                .set_body_string(COMPLETED_USAGE_SSE),
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
async fn v1_responses_should_forward_parity_fields_and_context_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .and(header("x-codex-turn-state", "turn-body"))
        .and(header("x-codex-turn-metadata", "meta-header"))
        .and(header("x-codex-beta-features", "beta-header"))
        .and(header("x-responsesapi-include-timing-metrics", "true"))
        .and(header("version", "2026-06-12"))
        .and(header("x-codex-window-id", "window-1"))
        .and(header("x-codex-parent-thread-id", "parent-1"))
        .and(body_json(json!({
            "model": "gpt-5.5",
            "instructions": "",
            "input": [],
            "stream": true,
            "store": false,
            "reasoning": {
                "effort": "high",
                "summary": "auto"
            },
            "service_tier": "priority",
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
            }
        })))
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
                .header("x-responsesapi-include-timing-metrics", "true")
                .header("version", "2026-06-12")
                .header("x-codex-window-id", "window-1")
                .header("x-codex-parent-thread-id", "parent-1")
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
                        "turnState": "turn-body"
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
}

#[tokio::test]
async fn v1_responses_should_include_encrypted_reasoning_by_default() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(body_json(json!({
            "model": "gpt-5.5",
            "instructions": "",
            "input": [],
            "stream": true,
            "store": false,
            "reasoning": {
                "effort": "high",
                "summary": "auto"
            },
            "include": ["reasoning.encrypted_content"]
        })))
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
                .set_body_string(STREAM_USAGE_SSE),
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

#[tokio::test]
async fn v1_responses_should_default_to_streaming_when_stream_is_omitted() {
    let server = MockServer::start().await;
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
