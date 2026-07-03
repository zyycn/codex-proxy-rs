use super::*;
use crate::support::assertions::assert_substrings_appear_in_order;

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

    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_logging(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-request-id", "req_chat_nonstream_log")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5-high-fast",
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
    assert_eq!(body["model"], "gpt-5.5-high-fast");
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
    assert_eq!(body["usage"]["prompt_tokens"], 9);
    assert_eq!(body["usage"]["completion_tokens"], 3);
    let requests = server.received_requests().await.unwrap();
    let upstream_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
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
    let usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(usage, (1, 9, 3));
    let model_usage: (String, i64, i64, i64) = sqlx::query_as(
        "select model, request_count, input_tokens, output_tokens from account_model_usage where account_id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(model_usage, ("gpt-5.5".to_string(), 1, 9, 3));
    let event = latest_usage_record(&pool, "v1.chat").await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();
    assert_eq!(event.request_id.as_deref(), Some("req_chat_nonstream_log"));
    assert_eq!(event.account_id.as_deref(), Some("acct_chat"));
    assert_eq!(event.route.as_deref(), Some("/v1/chat/completions"));
    assert_eq!(event.status_code, Some(200));
    let response_id = event.response_id.as_deref().unwrap_or_default();
    assert!(!response_id.is_empty());
    assert!(response_id.starts_with("chatcmpl-"));
    assert_eq!(metadata["route"], "/v1/chat/completions");
    assert_eq!(metadata["apiKind"], "chat");
    assert_eq!(metadata["responseId"], response_id);
    assert_eq!(metadata["stream"], false);
    assert_eq!(metadata["transport"], "http_sse");
    assert_eq!(metadata["serviceTier"], "priority");
    assert_eq!(metadata["usage"]["inputTokens"], 9);
    assert_eq!(metadata["usage"]["outputTokens"], 3);
}

#[tokio::test]
async fn chat_completions_with_user_should_use_and_reuse_websocket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_payload = send_chat_websocket_response(
            &mut websocket,
            "websocket chat first",
            "resp_chat_ws_first",
        )
        .await;

        loop {
            tokio::select! {
                message = websocket.next() => {
                    match message {
                        Some(Ok(message)) if message.is_text() => {
                            let second_payload = send_chat_websocket_response_after_message(
                                &mut websocket,
                                message,
                                "websocket chat second",
                                "resp_chat_ws_second",
                            )
                            .await;
                            websocket.close(None).await.unwrap();
                            break (true, first_payload, second_payload);
                        }
                        Some(_) => {}
                        None => {
                            let second_payload = accept_chat_websocket_response(
                                &listener,
                                "websocket chat second",
                                "resp_chat_ws_second",
                            )
                            .await;
                            break (false, first_payload, second_payload);
                        }
                    }
                }
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let mut second_websocket = accept_async(stream).await.unwrap();
                    let second_payload = send_chat_websocket_response(
                        &mut second_websocket,
                        "websocket chat second",
                        "resp_chat_ws_second",
                    )
                    .await;
                    second_websocket.close(None).await.unwrap();
                    break (false, first_payload, second_payload);
                }
            }
        }
    });
    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_logging(base_url).await;

    let first_response = app
        .clone()
        .oneshot(chat_json_request(
            &api_key,
            "req_chat_ws_first",
            "chat-ws-user",
            "Say hello over websocket",
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = response_json(first_response).await;
    assert_eq!(
        first_body["choices"][0]["message"]["content"],
        "websocket chat first"
    );

    let second_response = app
        .oneshot(chat_json_request(
            &api_key,
            "req_chat_ws_second",
            "chat-ws-user",
            "Continue over the same websocket",
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = response_json(second_response).await;
    assert_eq!(
        second_body["choices"][0]["message"]["content"],
        "websocket chat second"
    );
    let (reused_connection, first_payload, second_payload) = upstream.await.unwrap();
    let event = latest_usage_record(&pool, "v1.chat").await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();

    assert!(
        reused_connection,
        "second chat request opened a new websocket"
    );
    assert_eq!(first_payload["type"], "response.create");
    assert_eq!(second_payload["type"], "response.create");
    assert_eq!(
        first_payload["prompt_cache_key"], second_payload["prompt_cache_key"],
        "same chat user should keep the same account-scoped conversation key"
    );
    assert!(first_payload["prompt_cache_key"]
        .as_str()
        .is_some_and(|key| key.starts_with("cp_")));
    assert_eq!(metadata["transport"], "websocket");
    assert_eq!(metadata["websocketPool"]["kind"], "reuse");
    assert!(
        metadata["firstTokenMs"]
            .as_i64()
            .is_some_and(|value| value > 0),
        "chat websocket usage metadata should include first token latency: {metadata:?}",
    );
}

#[tokio::test]
async fn chat_completions_websocket_should_report_unmapped_response_failed_without_rotation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let payload = serde_json::from_str::<Value>(&message.into_text().unwrap())
            .expect("websocket payload should be json");
        websocket
            .send(Message::Text(
                response_failed_websocket_message(
                    "resp_chat_failed_terminal",
                    "policy_violation",
                    "Terminal policy failure",
                )
                .into(),
            ))
            .await
            .unwrap();
        payload
    });
    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_logging(base_url).await;

    let response = app
        .oneshot(chat_json_request(
            &api_key,
            "req_chat_ws_unmapped_failed",
            "chat-ws-failed-user",
            "Trigger a terminal response.failed",
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let payload = upstream.await.unwrap();
    let event = latest_usage_record(&pool, "v1.chat").await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "invalid_upstream_response");
    assert_eq!(payload["type"], "response.create");
    assert_eq!(
        event.request_id.as_deref(),
        Some("req_chat_ws_unmapped_failed")
    );
    assert_eq!(event.account_id.as_deref(), Some("acct_chat"));
    assert_eq!(event.route.as_deref(), Some("/v1/chat/completions"));
    assert_eq!(event.status_code, Some(502));
    assert_eq!(metadata["route"], "/v1/chat/completions");
    assert_eq!(metadata["apiKind"], "chat");
    assert_eq!(metadata["stream"], false);
    assert_eq!(metadata["transport"], "websocket");
    assert_eq!(metadata["failureClass"], "invalid_sse");
    assert!(metadata["error"]
        .as_str()
        .is_some_and(|error| error.contains("Terminal policy failure")));
}

#[tokio::test]
async fn chat_completions_stream_should_translate_codex_sse_to_openai_chunks() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secret"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(include_str!(
                    "../../../fixtures/responses/http_sse/chat_reasoning_text_completed.sse"
                )),
        )
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_logging(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-request-id", "req_chat_stream_log")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5-high-fast",
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
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("\"object\":\"chat.completion.chunk\""));
    assert!(body.contains("\"model\":\"gpt-5.5-high-fast\""));
    assert_substrings_appear_in_order(
        &body,
        &[
            "\"delta\":{\"role\":\"assistant\"}",
            "\"delta\":{\"reasoning_content\":\"I considered the context.\"}",
            "\"delta\":{\"content\":\"hello\"}",
            "\"finish_reason\":\"stop\"",
            "\"usage\":{\"completion_tokens\":3,\"prompt_tokens\":9,\"total_tokens\":12}",
            "data: [DONE]",
        ],
    );
    let event = latest_usage_record(&pool, "v1.chat").await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();
    assert_eq!(event.request_id.as_deref(), Some("req_chat_stream_log"));
    assert_eq!(event.route.as_deref(), Some("/v1/chat/completions"));
    assert_eq!(metadata["route"], "/v1/chat/completions");
    assert_eq!(metadata["apiKind"], "chat");
    assert_eq!(metadata["stream"], true);
    assert_eq!(metadata["transport"], "http_sse");
}

#[tokio::test]
async fn chat_completions_stream_should_emit_openai_error_when_upstream_fails_after_chunks() {
    let (base_url, first_chunk_sent, finish_upstream) = spawn_chunked_sse_upstream(
        "event: response.output_text.delta\ndata: {\"delta\":\"partial hello\"}\n\n",
        "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_failed\",\"status\":\"failed\",\"error\":{\"code\":\"quota_exceeded\",\"message\":\"quota exhausted\"}}}\n\n",
    )
    .await;

    let (app, api_key, _pool, _dir) = test_app_with_account_pool_and_logging(base_url).await;
    let response_task = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "stream": true,
                        "messages": [
                            {"role": "user", "content": "Start then fail"}
                        ]
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
        .expect("stream response should be returned before terminal upstream failure")
        .unwrap();
    finish_upstream.send(()).unwrap();

    let body = response_text(response).await;
    assert!(body.contains("\"delta\":{\"content\":\"partial hello\"}"));
    assert!(body.contains("\"error\""));
    assert!(body.contains("\"type\":\"insufficient_quota\""));
    assert!(body.contains("\"code\":\"insufficient_quota\""));
    assert!(body.contains("\"message\":\"quota exhausted\""));
    assert!(!body.contains("stream_error"));
    assert!(body.ends_with("data: [DONE]\n\n"));
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
                .header("x-request-id", "req_chat_429_exhausted")
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

    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_logging(server.uri()).await;
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
    assert_eq!(body["error"]["type"], "rate_limit_error");
    assert_eq!(body["error"]["code"], "rate_limit_exceeded");
    assert_eq!(quota_state.0, 1);
    assert!(quota_state.1.is_some());
    let event = latest_usage_record(&pool, "v1.chat").await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();
    assert_eq!(event.level, "error");
    assert!(event
        .request_id
        .as_deref()
        .is_some_and(|request_id| request_id.starts_with("req_")));
    assert_eq!(event.account_id.as_deref(), Some("acct_chat"));
    assert_eq!(event.route.as_deref(), Some("/v1/chat/completions"));
    assert_eq!(event.status_code, Some(429));
    assert_eq!(metadata["route"], "/v1/chat/completions");
    assert_eq!(metadata["apiKind"], "chat");
    assert_eq!(metadata["stream"], false);
    assert_eq!(metadata["transport"], "http_sse");
    assert_eq!(metadata["failed"], true);
    assert_eq!(metadata["failureClass"], "rate_limited");
    assert_eq!(metadata["exhaustedCount"], 1);
    assert!(metadata["upstreamError"]
        .as_str()
        .is_some_and(|error| error.contains("rate limited")));
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
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "model_not_found");
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
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "invalid_api_key");
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
            "error": {"message": "request forbidden"}
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
    let cookie_store = SqliteCookieStore::new(pool.clone());
    cookie_store
        .capture_set_cookie(
            "acct_primary",
            "cf_clearance=old; Domain=.chatgpt.com; Path=/",
        )
        .await
        .unwrap();
    cookie_store
        .capture_set_cookie(
            "acct_secondary",
            "cf_clearance=keep; Domain=.chatgpt.com; Path=/",
        )
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
            .unwrap()
            .as_deref(),
        Some("cf_clearance=old")
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

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(message.contains("All accounts exhausted (1 quota-exhausted)"));
    assert!(message.contains("quota reached"));
    assert_eq!(body["error"]["type"], "insufficient_quota");
    assert_eq!(body["error"]["code"], "insufficient_quota");
    assert_eq!(account_status.0, "quota_exhausted");
}

fn chat_json_request(api_key: &str, request_id: &str, user: &str, content: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("authorization", format!("Bearer {api_key}"))
        .header("content-type", "application/json")
        .header("x-request-id", request_id)
        .body(Body::from(
            json!({
                "model": "gpt-5.5",
                "user": user,
                "messages": [{"role": "user", "content": content}]
            })
            .to_string(),
        ))
        .unwrap()
}

async fn accept_chat_websocket_response(
    listener: &TcpListener,
    content: &str,
    response_id: &str,
) -> Value {
    let (stream, _) = listener.accept().await.unwrap();
    let mut websocket = accept_async(stream).await.unwrap();
    let payload = send_chat_websocket_response(&mut websocket, content, response_id).await;
    websocket.close(None).await.unwrap();
    payload
}

async fn send_chat_websocket_response(
    websocket: &mut WebSocketStream<TcpStream>,
    content: &str,
    response_id: &str,
) -> Value {
    let message = websocket.next().await.unwrap().unwrap();
    send_chat_websocket_response_after_message(websocket, message, content, response_id).await
}

async fn send_chat_websocket_response_after_message(
    websocket: &mut WebSocketStream<TcpStream>,
    message: Message,
    content: &str,
    response_id: &str,
) -> Value {
    let payload = serde_json::from_str::<Value>(&message.into_text().unwrap())
        .expect("websocket payload should be json");
    websocket
        .send(Message::Text(
            json!({
                "type": "response.output_text.delta",
                "delta": content
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    websocket
        .send(Message::Text(
            response_completed_websocket_message(response_id).into(),
        ))
        .await
        .unwrap();
    payload
}
