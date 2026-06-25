use super::*;

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
    assert_eq!(event.response_id.as_deref(), Some("resp_usage"));
    assert_eq!(metadata["responseId"], "resp_usage");
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
    assert_eq!(quota["snapshots"][0]["source"], "core");
    assert_eq!(quota["snapshots"][0]["primary"]["limit_reached"], true);
    assert_eq!(quota["snapshots"][0]["primary"]["reset_at"], reset_at);
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
    assert_eq!(metadata["transport"], "http_sse");
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
