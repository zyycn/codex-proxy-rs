use super::*;

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
async fn responses_review_route_should_record_review_route_in_usage_record() {
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

    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_telemetry(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/review")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-request-id", "req_review_route_log")
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
    let event = latest_response_usage_record(&pool).await;
    assert_eq!(event.request_id.as_deref(), Some("req_review_route_log"));
    assert_eq!(event.route.as_deref(), Some("/v1/responses/review"));
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
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-ratelimit-limit-requests", "33")
                .set_body_json(json!({
                    "output": [
                        {
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "compacted"}]
                        }
                    ],
                    "usage": {"input_tokens": 11, "output_tokens": 4}
                })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_telemetry(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/compact")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-request-id", "req_compact_success_log")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
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
                        "prompt_cache_key": "session-seed"
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
    // compact 原样透传 reasoning，含未知键。
    assert_eq!(
        upstream_body["reasoning"],
        json!({"effort": "high", "summary": "auto", "extra": "drop"})
    );
    assert_eq!(
        upstream_body["tools"],
        json!([{"type": "function", "name": "lookup"}])
    );
    assert_eq!(upstream_body["text"]["format"]["type"], "json_schema");
    // compact 只剥离 stream（端点不支持流式）；store/prompt_cache_key 等业务字段原样透传。
    assert!(upstream_body.get("stream").is_none());
    assert_eq!(upstream_body["store"], true);
    assert_eq!(upstream_body["prompt_cache_key"], "session-seed");
    assert_eq!(upstream_body["input"].as_array().unwrap().len(), 4);
    assert_eq!(upstream_body["input"][1]["ignored"], "drop");
    assert_eq!(
        upstream_body["input"][2]["encrypted_content"],
        "enc_compact"
    );
    assert_eq!(upstream_body["input"][3]["id"], "drop_missing_encrypted");
    let event = latest_response_usage_record(&pool).await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();
    assert_eq!(event.request_id.as_deref(), Some("req_compact_success_log"));
    assert_eq!(event.account_id.as_deref(), Some("acct_chat"));
    assert_eq!(event.route.as_deref(), Some("/v1/responses/compact"));
    assert_eq!(event.status_code, Some(200));
    assert_eq!(metadata["stream"], false);
    assert_eq!(metadata["compact"], true);
    assert_eq!(event.input_tokens, Some(11));
    assert_eq!(event.output_tokens, Some(4));
    assert_rate_limit_header(&metadata, "x-ratelimit-limit-requests", "33");
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

    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_telemetry(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/compact")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-request-id", "req_compact_rate_limited_log")
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
    assert_eq!(body["error"]["type"], "rate_limit_error");
    assert_eq!(body["error"]["code"], "rate_limit_exceeded");
    assert!(message.contains("All accounts exhausted (1 rate-limited)"));
    assert!(message.contains("compact quota reached"));
    let event = latest_response_ops_error_log(&pool).await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();
    assert_eq!(
        event.request_id.as_deref(),
        Some("req_compact_rate_limited_log")
    );
    assert_eq!(event.account_id.as_deref(), Some("acct_chat"));
    assert_eq!(event.route.as_deref(), Some("/v1/responses/compact"));
    assert_eq!(event.status_code, Some(429));
    assert_eq!(event.level, "error");
    assert_eq!(metadata["stream"], false);
    assert_eq!(metadata["compact"], true);
    assert_eq!(event.transport.as_deref(), Some("http"));
    assert_eq!(metadata["failed"], true);
    assert_eq!(event.failure_class.as_deref(), Some("rate_limited"));
    assert_eq!(metadata["exhaustedCount"], 1);
    assert!(metadata["upstreamError"]
        .as_str()
        .is_some_and(|value| value.contains("compact quota reached")));
}

#[tokio::test]
async fn responses_compact_should_preserve_upstream_client_error_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses/compact"))
        .and(header("authorization", "Bearer access-secret"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "code": "invalid_encrypted_content",
                "message": "The encrypted content could not be verified."
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_telemetry(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/compact")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-request-id", "req_compact_invalid_encrypted_content")
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

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "codex_client_error");
    let event = latest_response_ops_error_log(&pool).await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();
    assert_eq!(
        event.request_id.as_deref(),
        Some("req_compact_invalid_encrypted_content")
    );
    assert_eq!(event.route.as_deref(), Some("/v1/responses/compact"));
    assert_eq!(event.status_code, Some(400));
    assert_eq!(event.level, "error");
    assert_eq!(metadata["compact"], true);
    assert_eq!(metadata["failed"], true);
    assert_eq!(event.failure_class.as_deref(), Some("upstream"));
    assert_eq!(event.upstream_status_code, Some(400));
    assert!(metadata["error"]
        .as_str()
        .is_some_and(|value| value.contains("invalid_encrypted_content")));
}

#[tokio::test]
async fn responses_compact_should_map_upstream_internal_error_to_bad_gateway() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses/compact"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {"message": "upstream internal error"}
        })))
        .expect(3)
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

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["type"], "server_error");
    assert_eq!(body["error"]["code"], "upstream_error");
}

#[tokio::test]
async fn responses_compact_should_preserve_upstream_service_unavailable_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses/compact"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": {"message": "upstream unavailable"}
        })))
        .expect(3)
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
    assert_eq!(body["error"]["type"], "server_error");
    assert_eq!(body["error"]["code"], "upstream_error");
}

#[tokio::test]
async fn responses_compact_should_return_responses_error_when_no_accounts_are_available() {
    let server = MockServer::start().await;
    let (app, api_key, _pool, _dir) = test_app_without_accounts(server.uri()).await;

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
    assert_eq!(body["error"]["type"], "server_error");
    assert_eq!(body["error"]["code"], "no_available_accounts");
}
