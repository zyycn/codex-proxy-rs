use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

use crate::support::{
    response_json, response_text,
    upstream::{build_imported_app, enable_runtime_logging, fetch_v1_event_log},
};

const ERROR_EVENT_SSE: &str = include_str!("../fixtures/responses/http_sse/error_event.sse");
const FAILED_EVENT_SSE: &str = include_str!("../fixtures/responses/http_sse/failed_event.sse");
const STREAM_ERROR_EVENT_SSE: &str =
    include_str!("../fixtures/responses/http_sse/stream_error_event.sse");
const STREAM_FAILED_EVENT_SSE: &str =
    include_str!("../fixtures/responses/http_sse/stream_failed_event.sse");
const STREAM_PREMATURE_CLOSE_SSE: &str =
    include_str!("../fixtures/responses/http_sse/stream_premature_close.sse");
const STREAM_PREMATURE_CLOSE_GOLDEN: &str =
    include_str!("../fixtures/responses/golden/stream_premature_close.sse");
const ERROR_EVENT_RESPONSE_GOLDEN: &str =
    include_str!("../fixtures/responses/golden/error_event_response.json");
const RESPONSE_FAILED_RATE_LIMIT_GOLDEN: &str =
    include_str!("../fixtures/responses/golden/response_failed_rate_limit.json");
const STREAM_ERROR_EVENT_GOLDEN: &str =
    include_str!("../fixtures/responses/golden/stream_error_event.sse");
const STREAM_RESPONSE_FAILED_GOLDEN: &str =
    include_str!("../fixtures/responses/golden/stream_response_failed.sse");

#[tokio::test]
async fn v1_responses_non_stream_should_return_codex_api_error_when_sse_error_event_arrives() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(ERROR_EVENT_SSE),
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
    let body = response_json(response).await;
    let expected: serde_json::Value = serde_json::from_str(ERROR_EVENT_RESPONSE_GOLDEN).unwrap();
    assert_eq!(body, expected);
}

#[tokio::test]
async fn v1_responses_non_stream_should_return_rate_limit_error_when_response_failed_arrives() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(FAILED_EVENT_SSE),
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

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = response_json(response).await;
    let expected: serde_json::Value =
        serde_json::from_str(RESPONSE_FAILED_RATE_LIMIT_GOLDEN).unwrap();
    assert_eq!(body, expected);
}

#[tokio::test]
async fn v1_responses_stream_should_passthrough_error_event_and_log_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(STREAM_ERROR_EVENT_SSE),
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
                .header("x-request-id", "req_stream_error_event")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert_eq!(body, with_sse_terminal_separator(STREAM_ERROR_EVENT_GOLDEN));
    let event = fetch_v1_event_log(&imported.pool, "req_stream_error_event").await;
    assert_eq!(event.3, 502);
    assert_eq!(event.4["failureEvent"], "error");
}

#[tokio::test]
async fn v1_responses_stream_should_passthrough_response_failed_and_log_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(STREAM_FAILED_EVENT_SSE),
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
                .header("x-request-id", "req_stream_response_failed")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert_eq!(
        body,
        with_sse_terminal_separator(STREAM_RESPONSE_FAILED_GOLDEN)
    );
    let event = fetch_v1_event_log(&imported.pool, "req_stream_response_failed").await;
    assert_eq!(event.3, 502);
    assert_eq!(event.4["failureEvent"], "response.failed");
}

#[tokio::test]
async fn v1_responses_stream_should_synthesize_response_failed_when_http_sse_closes_before_terminal(
) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(STREAM_PREMATURE_CLOSE_SSE),
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
                .header("x-request-id", "req_stream_premature_http_sse")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":true,"use_websocket":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert_eq!(
        redact_proxy_response_id(&body),
        with_sse_terminal_separator(STREAM_PREMATURE_CLOSE_GOLDEN)
    );
    assert!(body.contains("event: response.output_text.delta"));
    assert!(body.contains("event: response.failed"));
    assert!(body.contains("stream_disconnected"));
    let event = fetch_v1_event_log(&imported.pool, "req_stream_premature_http_sse").await;
    assert_eq!(event.3, 502);
    assert_eq!(event.4["failureEvent"], "response.failed");
    assert_eq!(event.4["upstreamCode"], "stream_disconnected");
}

fn redact_proxy_response_id(body: &str) -> String {
    const PREFIX: &str = r#""id":"resp_proxy_"#;
    let mut redacted = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(index) = rest.find(PREFIX) {
        redacted.push_str(&rest[..index]);
        redacted.push_str(r#""id":"resp_proxy_redacted"#);
        let suffix = &rest[index + PREFIX.len()..];
        let Some(end) = suffix.find('"') else {
            rest = suffix;
            break;
        };
        rest = &suffix[end..];
    }
    redacted.push_str(rest);
    redacted
}

fn with_sse_terminal_separator(body: &str) -> String {
    if body.ends_with("\n\n") {
        body.to_string()
    } else {
        format!("{body}\n")
    }
}
