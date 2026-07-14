use std::time::Duration as StdDuration;

use axum::{body::Body, http::Request};
use futures::StreamExt;
use serde_json::json;
use tokio::time::timeout;
use tower::util::ServiceExt;

use crate::dispatch::service::{
    collect_stream_body, spawn_chunked_sse_upstream, spawn_chunked_sse_upstream_bytes,
    spawn_chunked_sse_upstream_then_abrupt_close, spawn_chunked_sse_upstream_then_clean_close,
    test_app_with_account_and_pool,
};

const LIVE_OUTPUT: &str =
    include_str!("../../fixtures/responses/http_sse/live_stream_hello_delta.sse");

#[tokio::test]
async fn stream_terminal_should_synthesize_failure_after_upstream_read_error() {
    let (base_url, first_chunk_sent, close_upstream) =
        spawn_chunked_sse_upstream_then_abrupt_close(include_str!(
            "../../fixtures/responses/http_sse/partial_transport_failure.sse"
        ))
        .await;

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
    assert!(
        String::from_utf8(first_chunk.to_vec())
            .unwrap()
            .contains("partial before transport failure")
    );

    close_upstream.send(()).unwrap();
    let rest = collect_stream_body(body_stream).await;

    assert!(rest.contains("event: response.failed"));
    assert!(rest.contains("stream_disconnected"));
    assert!(rest.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn stream_terminal_should_synthesize_failure_after_clean_upstream_close() {
    let (base_url, first_chunk_sent, close_upstream) = spawn_chunked_sse_upstream_then_clean_close(
        include_str!("../../fixtures/responses/http_sse/partial_clean_close.sse"),
    )
    .await;

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
    let before_close = timeout(StdDuration::from_millis(300), async {
        let mut body = String::new();
        while let Some(chunk) = body_stream.next().await {
            body.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            if body.contains("partial before clean close") {
                return body;
            }
        }
        body
    })
    .await
    .expect("real output should arrive before upstream closes");
    assert!(before_close.contains("partial before clean close"));

    close_upstream.send(()).unwrap();
    let rest = collect_stream_body(body_stream).await;

    assert!(rest.contains("event: response.failed"));
    assert!(rest.contains("stream_disconnected"));
    assert!(rest.contains(r#""id":"resp_clean_close""#));
    assert!(rest.ends_with("data: [DONE]\n\n"));
}

#[tokio::test]
async fn committed_quota_failure_should_apply_effect_and_finalize_once() {
    let (base_url, first_chunk_sent, finish_upstream) = spawn_chunked_sse_upstream(
        LIVE_OUTPUT,
        include_str!("../../fixtures/responses/http_sse/live_stream_failed_quota.sse"),
    )
    .await;
    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(base_url).await;

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
                        "input": [{"role": "user", "content": "Fail after commit"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    first_chunk_sent.await.unwrap();
    let mut body = response.into_body().into_data_stream();
    let first = timeout(StdDuration::from_secs(1), body.next())
        .await
        .expect("committed output should reach the client")
        .expect("stream should yield committed output")
        .expect("committed output should be readable");
    assert!(String::from_utf8_lossy(&first).contains("live stream hello"));

    finish_upstream.send(()).unwrap();
    let rest = collect_stream_body(body).await;
    let account: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();
    let usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = $1")
            .bind("acct_chat")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert!(rest.contains("live quota exhausted"));
    assert_eq!((account.0.as_str(), usage.0), ("quota_exhausted", 1));
}

#[tokio::test]
async fn committed_auth_failure_should_expire_the_account() {
    let (base_url, first_chunk_sent, finish_upstream) = spawn_chunked_sse_upstream(
        LIVE_OUTPUT,
        include_str!("../../fixtures/responses/http_sse/live_stream_failed_auth.sse"),
    )
    .await;
    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(base_url).await;

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
                        "input": [{"role": "user", "content": "Expire after commit"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    first_chunk_sent.await.unwrap();
    let mut body = response.into_body().into_data_stream();
    let first = timeout(StdDuration::from_secs(1), body.next())
        .await
        .expect("committed output should reach the client")
        .expect("stream should yield committed output")
        .expect("committed output should be readable");
    assert!(String::from_utf8_lossy(&first).contains("live stream hello"));

    finish_upstream.send(()).unwrap();
    let rest = collect_stream_body(body).await;
    let account: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert!(rest.contains("live token expired"));
    assert_eq!(account.0, "expired");
}

#[tokio::test]
async fn capture_limit_terminal_should_finalize_once_and_fail_the_client_stream() {
    let oversized_event = format!(
        "event: response.output_text.delta\ndata: {}\n\n",
        json!({
            "type": "response.output_text.delta",
            "delta": "x".repeat(16 * 1024 * 1024),
        })
    )
    .into_bytes();
    let (base_url, first_chunk_sent, finish_upstream) =
        spawn_chunked_sse_upstream_bytes(LIVE_OUTPUT.as_bytes().to_vec(), oversized_event).await;
    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(base_url).await;

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
                        "input": [{"role": "user", "content": "Exceed capture limit"}],
                        "stream": true,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    first_chunk_sent.await.unwrap();
    let mut body = response.into_body().into_data_stream();
    let first = timeout(StdDuration::from_secs(1), body.next())
        .await
        .expect("committed output should reach the client")
        .expect("stream should yield committed output")
        .expect("committed output should be readable");
    assert!(String::from_utf8_lossy(&first).contains("live stream hello"));

    finish_upstream.send(()).unwrap();
    let rest = timeout(StdDuration::from_secs(30), collect_stream_body(body))
        .await
        .expect("capture-limit finalization should not stall");
    let usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = $1")
            .bind("acct_chat")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert!(rest.contains("upstream response exceeded the 16 MiB proxy capture limit"));
    assert!(rest.ends_with("data: [DONE]\n\n"));
    assert_eq!(usage.0, 1);
}
