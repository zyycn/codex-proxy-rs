use super::*;

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
    assert!(body.get("output_text").is_none());
    assert_eq!(body["usage"]["input_tokens"], 5);
    assert_eq!(body["usage"]["output_tokens"], 2);
}

#[tokio::test]
async fn responses_should_classify_sse_cyber_policy_failure_as_bad_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(include_str!(
                    "../../fixtures/responses/http_sse/failed_cyber_policy.sse"
                )),
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
                        "input": [{"role": "user", "content": "Security assessment"}],
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

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "cyber_policy");
    assert_eq!(
        body["error"]["message"],
        "This request has been flagged for possible cybersecurity risk."
    );
}

#[tokio::test]
async fn responses_http_cyber_policy_should_keep_account_active_and_retry_next_account() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {
                "code": "cyber_policy",
                "message": "This request has been flagged for possible cybersecurity risk."
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
                .set_body_string(RESPONSES_AFTER_402_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let request_body = json!({
        "model": "gpt-5.5",
        "prompt_cache_key": "conv_http_cyber_retry_next_account",
        "input": [{"role": "user", "content": "Security assessment"}],
        "stream": false,
        "use_websocket": false
    });

    let response = app
        .oneshot(responses_json_request(&api_key, &request_body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_json(response).await["id"], "resp_after_402");
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "active");
}

#[tokio::test]
async fn responses_http_sse_cyber_terminal_should_change_the_immediate_next_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(include_str!(
                    "../../fixtures/responses/http_sse/output_then_cyber_policy.sse"
                )),
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
                .set_body_string(RESPONSES_STREAM_AFTER_402_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let request_body = json!({
        "model": "gpt-5.5",
        "prompt_cache_key": "conv_http_sse_cyber_immediate_next",
        "input": [{"role": "user", "content": "Security assessment"}],
        "stream": true,
        "use_websocket": false
    });

    let first_response = app
        .clone()
        .oneshot(responses_json_request(&api_key, &request_body))
        .await
        .unwrap();
    let first_status = first_response.status();
    let first_body = response_text(first_response).await;

    assert_eq!(first_status, StatusCode::OK);
    assert!(first_body.contains("partial output before HTTP SSE policy failure"));
    assert!(first_body.contains("cyber_policy"));

    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "active");

    let second_response = app
        .oneshot(responses_json_request(&api_key, &request_body))
        .await
        .unwrap();
    let second_body = response_text(second_response).await;
    assert!(second_body.contains("resp_stream_after_402"));
}

#[tokio::test]
async fn responses_http_rate_limited_cyber_policy_should_retry_next_account() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "code": "cyber_policy",
                "message": "This request has been flagged for possible cybersecurity risk."
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
                .set_body_string(RESPONSES_AFTER_402_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let request_body = json!({
        "model": "gpt-5.5",
        "prompt_cache_key": "conv_http_429_cyber_retry_next_account",
        "input": [],
        "stream": false,
        "use_websocket": false
    });

    let response = app
        .oneshot(responses_json_request(&api_key, &request_body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_json(response).await["id"], "resp_after_402");
    let account_status: (String, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as("select status, quota_cooldown_until from accounts where id = $1")
            .bind("acct_primary")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(account_status, ("active".to_string(), None));
}

#[tokio::test]
async fn responses_cyber_rotation_should_return_current_quota_error_when_candidates_exhaust() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"code": "cyber_policy", "message": "cyber blocked"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-secondary"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "error": {"code": "billing_limit", "message": "quota exhausted"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let request_body = json!({
        "model": "gpt-5.5",
        "prompt_cache_key": "conv_cyber_then_quota",
        "input": [],
        "stream": false,
        "use_websocket": false
    });

    let response = app
        .oneshot(responses_json_request(&api_key, &request_body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "insufficient_quota");
    assert!(body["error"]["message"].as_str().is_some_and(|message| {
        message.contains("quota exhausted") && !message.contains("cyber blocked")
    }));
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_secondary")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "quota_exhausted");
}

#[tokio::test]
async fn responses_should_preserve_upstream_client_error_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "code": "invalid_input",
                "message": "A required parameter is invalid."
            }
        })))
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
                        "input": [{"role": "user", "content": "hello"}],
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

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_input");
    assert_eq!(body["error"]["message"], "A required parameter is invalid.");
    assert!(body["error"].get("type").is_none());
}

#[tokio::test]
async fn responses_should_return_no_available_accounts_error_when_no_accounts_are_available() {
    let server = MockServer::start().await;
    let (app, api_key, pool, _dir) = test_app_without_accounts(server.uri()).await;

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
    assert_eq!(body["error"]["type"], "server_error");
    assert_eq!(body["error"]["code"], "no_available_accounts");

    let (usage_count,): (i64,) = sqlx::query_as("select count(*) from usage_records")
        .fetch_one(&pool)
        .await
        .unwrap();
    let (ops_count, account_id, model, failure_class): (
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "select count(*), max(account_id), max(model), max(failure_class) from ops_error_logs",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let (bucket_requests, bucket_errors): (i64, i64) = sqlx::query_as(
        "select coalesce(sum(success_count + error_count), 0)::bigint, coalesce(sum(error_count), 0)::bigint from request_time_buckets",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(usage_count, 0);
    assert_eq!(ops_count, 1);
    assert_eq!(account_id, None);
    assert_eq!(model.as_deref(), Some("gpt-5.5"));
    assert_eq!(failure_class.as_deref(), Some("no_available_accounts"));
    assert_eq!((bucket_requests, bucket_errors), (1, 1));
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
        "select request_count, input_tokens, output_tokens from account_usage where account_id = $1",
    )
    .bind("acct_primary")
    .fetch_one(&pool)
    .await
    .unwrap();
    let secondary_usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = $1",
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_401");
    assert_eq!(account_status.0, "expired");
}

#[tokio::test]
async fn responses_should_disable_identity_verification_account_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "code": "identity_verification_required",
                "message": "Identity verification is required before using this account"
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
                .set_body_string(RESPONSES_AFTER_403_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [{"role": "user", "content": "Say hello"}],
                "stream": false,
                "use_websocket": false
            }),
        ))
        .await
        .unwrap();
    let body = response_json(response).await;
    let account_status: String = sqlx::query_scalar("select status from accounts where id = $1")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(body["id"], "resp_after_403");
    assert_eq!(account_status, "disabled");
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(account_status.0, "banned");
}

#[tokio::test]
async fn responses_should_mark_banned_after_402_deactivated_workspace_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "detail": {"code": "deactivated_workspace"}
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
    let body = response_json(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(body["id"], "resp_after_402");
    assert_eq!(account_status.0, "banned");
}

#[tokio::test]
async fn responses_stream_should_mark_banned_after_402_deactivated_workspace_and_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "detail": {"code": "deactivated_workspace"}
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
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [{"role": "user", "content": "Say hello"}],
                "stream": true,
                "use_websocket": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert!(body.contains("resp_stream_after_402"));
    assert_eq!(account_status.0, "banned");
}

#[tokio::test]
async fn responses_should_mark_banned_after_403_and_fallback() {
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
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
    let cookie_store = PgCookieStore::new(pool.clone());
    cookie_store
        .capture_set_cookie("acct_chat", "cf_clearance=old; Domain=.chatgpt.com; Path=/")
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
    let primary_state: (String, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as("select status, cloudflare_cooldown_until from accounts where id = $1")
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
        Some("cf_clearance=old".to_string())
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
    let cookie_store = PgCookieStore::new(pool.clone());
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
    let primary_state: (String, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as("select status, cloudflare_cooldown_until from accounts where id = $1")
            .bind("acct_primary")
            .fetch_one(&pool)
            .await
            .unwrap();
    let cooldown_until = primary_state.1.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_cloudflare");
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
    let cookie_store = PgCookieStore::new(pool.clone());
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
    let primary_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
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
    let cookie_store = PgCookieStore::new(pool.clone());
    cookie_store
        .capture_set_cookie("acct_chat", "cf_clearance=old; Domain=.chatgpt.com; Path=/")
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
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
async fn responses_should_fallback_after_http_404_model_unsupported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
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
    let primary_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(content_type.starts_with("application/json"));
    assert!(body.contains("\"code\":\"model_not_found\""));
    assert!(body.contains("All accounts exhausted (1 model-unsupported)"));
    assert!(body.contains("model_not_available"));
    assert_eq!(account_status.0, "active");
}

#[tokio::test]
async fn responses_explicit_previous_response_should_fail_without_transparent_retry() {
    let (base_url, upstream) =
        spawn_single_websocket_sequence_upstream(vec![response_failed_websocket_message(
            "resp_previous_missing_failed",
            "previous_response_not_found",
            "Previous response with id resp_stale was not found",
        )])
        .await;
    let (app, api_key, _pool, _dir) =
        test_app_with_account_pool_and_affinity(base_url, "resp_stale").await;
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

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "previous_response_unavailable");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("resend the complete input")
    );
    assert_eq!(captured.payload["previous_response_id"], "resp_stale");
    assert!(!captured.retry_attempted);
}

#[tokio::test]
async fn responses_external_previous_response_not_found_should_return_exact_upstream_400_once() {
    let server = MockServer::start().await;
    let upstream_error = json!({
        "type": "error",
        "error": {
            "type": "invalid_request_error",
            "code": "previous_response_not_found",
            "message": "Previous response with id 'resp_external' not found.",
            "param": "previous_response_id"
        },
        "status": 400
    });
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(400).set_body_json(upstream_error.clone()))
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
                .header("x-request-id", "req_external_previous_not_found")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "previous_response_id": "resp_external",
                        "input": [{"role": "user", "content": "Continue"}],
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
    let body = response_json(response).await;
    let event = latest_response_upstream_ops_error_log(&pool).await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, upstream_error);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert_eq!(
        event.request_id.as_deref(),
        Some("req_external_previous_not_found")
    );
    assert_eq!(event.status_code, Some(400));
    assert_eq!(event.client_status_code, Some(400));
    assert_eq!(event.upstream_status_code, Some(400));
    assert_eq!(event.attempt_index, Some(0));
    assert_eq!(metadata["attemptCount"], 1);
}

#[tokio::test]
async fn responses_managed_history_without_local_transcript_should_not_fan_out() {
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
    let (app, state, api_key, _pool, _dir) =
        test_app_with_two_accounts_and_state(server.uri()).await;
    state
        .services
        .session_affinity
        .record(
            "resp_managed".to_string(),
            SessionAffinityEntry {
                account_id: "acct_primary".to_string(),
                conversation_id: "conversation-managed".to_string(),
                turn_state: Some("account-bound-turn-state".to_string()),
                instructions_hash: None,
                input_tokens: Some(11),
                function_call_ids: Vec::new(),
                variant_hash: None,
                continuation_scope: codex_proxy_rs::upstream::openai::protocol::responses::PreviousResponseScope::Persisted,
                created_at: Utc::now(),
            },
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
                        "previous_response_id": "resp_managed",
                        "turnState": "client-turn-state",
                        "input": [{"role": "user", "content": "Current question"}],
                        "stream": false,
                        "use_websocket": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer access-primary")
    );
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["previous_response_id"], "resp_managed");
}

#[tokio::test]
async fn responses_explicit_unanswered_function_call_should_not_retry_without_full_context() {
    let (base_url, upstream) =
        spawn_single_websocket_sequence_upstream(vec![response_failed_websocket_message(
            "resp_unanswered_call_failed",
            "invalid_request",
            "No tool output found for function call call_1",
        )])
        .await;
    let (app, api_key, _pool, _dir) =
        test_app_with_account_pool_and_affinity(base_url, "resp_stale").await;
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

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "codex_client_error");
    assert_eq!(body["error"]["message"], "Upstream Codex response failed");
    assert_eq!(captured.payload["previous_response_id"], "resp_stale");
    assert!(!captured.retry_attempted);
}

#[tokio::test]
async fn responses_explicit_invalid_reasoning_replay_should_not_retry_without_full_context() {
    let (base_url, upstream) =
        spawn_single_websocket_sequence_upstream(vec![response_failed_websocket_message(
            "resp_invalid_reasoning_failed",
            "invalid_encrypted_content",
            "Invalid encrypted content in reasoning replay",
        )])
        .await;
    let (app, api_key, _pool, _dir) =
        test_app_with_account_pool_and_affinity(base_url, "resp_stale").await;
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

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "previous_response_unavailable");
    assert_eq!(captured.payload["previous_response_id"], "resp_stale");
    assert!(!captured.retry_attempted);
}

#[tokio::test]
async fn responses_stream_structural_events_should_not_commit_before_history_failure() {
    let (base_url, upstream) = spawn_single_websocket_sequence_upstream(vec![
        json!({
            "type": "response.created",
            "response": {
                "id": "resp_previous_missing_failed",
                "object": "response",
                "status": "in_progress"
            }
        })
        .to_string(),
        json!({
            "type": "response.content_part.added",
            "response_id": "resp_previous_missing_failed",
            "output_index": 0,
            "content_index": 0,
            "part": {"type": "output_text", "text": "", "annotations": []}
        })
        .to_string(),
        response_failed_websocket_message(
            "resp_previous_missing_failed",
            "previous_response_not_found",
            "Previous response with id resp_stale was not found",
        ),
    ])
    .await;
    let (app, api_key, _pool, _dir) =
        test_app_with_account_pool_and_affinity(base_url, "resp_stale").await;
    let response = timeout(
        StdDuration::from_secs(2),
        app.oneshot(
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
        ),
    )
    .await
    .expect("dispatch should classify the failure before starting the live stream")
    .unwrap();
    let status = response.status();
    let body = response_text(response).await;
    let captured = upstream.await.unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("previous_response_unavailable"));
    assert!(!body.contains("response.created"));
    assert_eq!(captured.payload["previous_response_id"], "resp_stale");
    assert!(!captured.retry_attempted);
}

#[tokio::test]
async fn responses_http_sse_structural_events_should_not_commit_before_history_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(include_str!(
                    "../../fixtures/responses/http_sse/structural_then_previous_response_not_found.sse"
                )),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (app, api_key, _pool, _dir) =
        test_app_with_account_pool_and_affinity(server.uri(), "resp_stale").await;
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
    let body = response_text(response).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("previous_response_unavailable"));
    assert!(!body.contains("response.created"));
}

#[tokio::test]
async fn responses_http_sse_should_commit_before_history_failure_after_real_output() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(include_str!(
                    "../../fixtures/responses/http_sse/output_then_previous_response_not_found.sse"
                )),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (app, api_key, _pool, _dir) =
        test_app_with_account_pool_and_affinity(server.uri(), "resp_stale").await;
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
    let body = response_text(response).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("visible HTTP SSE output"));
    assert!(body.contains("response.failed"));
}

#[tokio::test]
async fn responses_stream_should_not_retry_history_failure_after_real_output() {
    let (base_url, upstream) = spawn_single_websocket_sequence_upstream(vec![
        json!({
            "type": "response.created",
            "response": {
                "id": "resp_output_then_failed",
                "object": "response",
                "status": "in_progress"
            }
        })
        .to_string(),
        json!({
            "type": "response.content_part.added",
            "response_id": "resp_output_then_failed",
            "output_index": 0,
            "content_index": 0,
            "part": {"type": "output_text", "text": "", "annotations": []}
        })
        .to_string(),
        json!({
            "type": "response.output_text.delta",
            "response_id": "resp_output_then_failed",
            "output_index": 0,
            "content_index": 0,
            "delta": "visible output"
        })
        .to_string(),
        response_failed_websocket_message(
            "resp_output_then_failed",
            "previous_response_not_found",
            "Previous response with id resp_stale was not found",
        ),
    ])
    .await;
    let (app, api_key, _pool, _dir) =
        test_app_with_account_pool_and_affinity(base_url, "resp_stale").await;
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
    assert!(body.contains("visible output"));
    assert!(body.contains("response.failed"));
    assert_eq!(captured.payload["previous_response_id"], "resp_stale");
    assert!(!captured.retry_attempted);
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
    let primary_quota_state: (bool, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "select quota_limit_reached, quota_cooldown_until from accounts where id = $1",
    )
    .bind("acct_primary")
    .fetch_one(&pool)
    .await
    .unwrap();
    let primary_usage = wait_for_account_usage(&pool, "acct_primary", (1, 0, 0)).await;
    let secondary_usage = wait_for_account_usage(&pool, "acct_secondary", (1, 4, 1)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("resp_stream_after_429"));
    assert!(primary_quota_state.0);
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();
    let secondary_usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = $1",
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(content_type.starts_with("application/json"));
    assert!(body.contains("\"code\":\"invalid_api_key\""));
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
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
    let cookie_store = PgCookieStore::new(pool.clone());
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
    let primary_state: (String, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as("select status, cloudflare_cooldown_until from accounts where id = $1")
            .bind("acct_primary")
            .fetch_one(&pool)
            .await
            .unwrap();
    let cooldown_until = primary_state.1.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("resp_stream_after_cloudflare"));
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
    let cookie_store = PgCookieStore::new(pool.clone());
    cookie_store
        .capture_set_cookie("acct_chat", "cf_clearance=old; Domain=.chatgpt.com; Path=/")
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
async fn responses_should_rotate_account_after_5xx_without_same_account_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {"message": "temporary upstream failure"}
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
    assert_eq!(primary_requests, 1, "requests: {authorizations:?}");
    assert_eq!(secondary_requests, 1, "requests: {authorizations:?}");
}

#[tokio::test]
async fn responses_failover_should_replace_account_identity_and_preserve_session_semantics() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {
                "type": "authentication_error",
                "code": "token_invalidated",
                "message": "token is no longer valid"
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
                .set_body_string(RESPONSES_AFTER_5XX_RETRY_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, pool, _dir) = test_app_with_two_accounts(server.uri()).await;
    let cookie_store = PgCookieStore::new(pool);
    cookie_store
        .capture_set_cookie(
            "acct_primary",
            "cf_clearance=primary; Domain=.chatgpt.com; Path=/codex",
        )
        .await
        .unwrap();
    cookie_store
        .capture_set_cookie(
            "acct_secondary",
            "cf_clearance=secondary; Domain=.chatgpt.com; Path=/codex",
        )
        .await
        .unwrap();
    let turn_metadata = json!({
        "installation_id": "installation-client",
        "session_id": "session-client",
        "thread_id": "thread-client",
        "turn_id": "turn-client",
        "window_id": "window-client",
        "forked_from_thread_id": "forked-client",
        "parent_thread_id": "parent-client",
        "request_kind": "turn",
        "workspace_kind": "local"
    })
    .to_string();
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
                        "input": [
                            {
                                "role": "user",
                                "content": "Replay without identity pollution",
                                "id": "client_user_message_id",
                                "tool_arguments": {"id": "client_tool_argument_id"}
                            },
                            {
                                "type": "reasoning",
                                "id": "reasoning_account_primary",
                                "encrypted_content": "encrypted_reasoning_account_primary",
                                "summary": [{
                                    "type": "summary_text",
                                    "id": "nested_reasoning_summary_id",
                                    "text": "semantic summary"
                                }]
                            }
                        ],
                        "stream": false,
                        "use_websocket": false,
                        "prompt_cache_key": "cache-client",
                        "conversation_id": "conversation-client",
                        "session_id": "session-client",
                        "thread_id": "thread-client",
                        "turn_id": "turn-client",
                        "x-client-request-id": "request-client",
                        "x-codex-window-id": "window-client",
                        "x-codex-parent-thread-id": "parent-client",
                        "x-codex-installation-id": "installation-client",
                        "turnState": "turn-state-primary",
                        "turnMetadata": turn_metadata,
                        "codexWindowId": "window-client",
                        "parentThreadId": "parent-client",
                        "client_metadata": {
                            "safe": "preserved",
                            "conversation_id": "conversation-client",
                            "session_id": "session-client",
                            "thread_id": "thread-client",
                            "turn_id": "turn-client",
                            "x-client-request-id": "request-client",
                            "x-codex-window-id": "window-client",
                            "x-codex-parent-thread-id": "parent-client",
                            "x-codex-installation-id": "installation-client",
                            "x-codex-turn-state": "turn-state-primary",
                            "x-codex-turn-metadata": turn_metadata
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
    let responses_requests = requests
        .iter()
        .filter(|request| request.url.path() == "/codex/responses")
        .collect::<Vec<_>>();
    let primary = responses_requests
        .iter()
        .copied()
        .find(|request| {
            request
                .headers
                .get("authorization")
                .is_some_and(|value| value == "Bearer access-primary")
        })
        .unwrap();
    let secondary = responses_requests
        .iter()
        .copied()
        .find(|request| {
            request
                .headers
                .get("authorization")
                .is_some_and(|value| value == "Bearer access-secondary")
        })
        .unwrap();
    let primary_body: Value = serde_json::from_slice(&primary.body).unwrap();
    let secondary_body: Value = serde_json::from_slice(&secondary.body).unwrap();
    let primary_metadata = &primary_body["client_metadata"];
    let secondary_metadata = &secondary_body["client_metadata"];

    assert_eq!(responses_requests.len(), 2);
    assert_eq!(primary.headers["chatgpt-account-id"], "chatgpt-primary");
    assert_eq!(secondary.headers["chatgpt-account-id"], "chatgpt-secondary");
    assert_eq!(primary.headers["cookie"], "cf_clearance=primary");
    assert_eq!(secondary.headers["cookie"], "cf_clearance=secondary");
    assert_ne!(
        primary_metadata["x-codex-installation-id"],
        secondary_metadata["x-codex-installation-id"]
    );
    for key in [
        "session_id",
        "thread_id",
        "turn_id",
        "x-client-request-id",
        "x-codex-window-id",
        "x-codex-parent-thread-id",
    ] {
        assert_eq!(primary_metadata[key], secondary_metadata[key], "key: {key}");
    }
    assert_ne!(
        primary_body["x-codex-installation-id"],
        secondary_body["x-codex-installation-id"]
    );
    for key in [
        "prompt_cache_key",
        "conversation_id",
        "session_id",
        "thread_id",
        "turn_id",
        "x-client-request-id",
        "x-codex-window-id",
        "x-codex-parent-thread-id",
        "codexWindowId",
        "parentThreadId",
    ] {
        assert_eq!(primary_body[key], secondary_body[key], "key: {key}");
    }
    let primary_turn_metadata: Value =
        serde_json::from_str(primary_metadata["x-codex-turn-metadata"].as_str().unwrap()).unwrap();
    let secondary_turn_metadata: Value = serde_json::from_str(
        secondary_metadata["x-codex-turn-metadata"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_ne!(
        primary_turn_metadata["installation_id"],
        secondary_turn_metadata["installation_id"]
    );
    for key in [
        "session_id",
        "thread_id",
        "turn_id",
        "window_id",
        "forked_from_thread_id",
        "parent_thread_id",
    ] {
        assert_eq!(
            primary_turn_metadata[key], secondary_turn_metadata[key],
            "turn metadata key: {key}"
        );
    }
    for (request, body, metadata, turn_metadata) in [
        (
            primary,
            &primary_body,
            primary_metadata,
            &primary_turn_metadata,
        ),
        (
            secondary,
            &secondary_body,
            secondary_metadata,
            &secondary_turn_metadata,
        ),
    ] {
        for (header_key, body_key, metadata_key, turn_metadata_key) in [
            (
                "x-codex-installation-id",
                "x-codex-installation-id",
                "x-codex-installation-id",
                "installation_id",
            ),
            ("session-id", "session_id", "session_id", "session_id"),
            ("thread-id", "thread_id", "thread_id", "thread_id"),
            ("x-codex-turn-id", "turn_id", "turn_id", "turn_id"),
            (
                "x-codex-window-id",
                "x-codex-window-id",
                "x-codex-window-id",
                "window_id",
            ),
            (
                "x-codex-parent-thread-id",
                "x-codex-parent-thread-id",
                "x-codex-parent-thread-id",
                "parent_thread_id",
            ),
        ] {
            let header_value = request.headers[header_key].to_str().unwrap();
            assert_eq!(body[body_key].as_str(), Some(header_value));
            assert_eq!(metadata[metadata_key].as_str(), Some(header_value));
            assert_eq!(
                turn_metadata[turn_metadata_key].as_str(),
                Some(header_value)
            );
        }
        assert_eq!(
            body["x-client-request-id"].as_str(),
            request.headers["x-client-request-id"].to_str().ok()
        );
        assert_eq!(
            metadata["x-client-request-id"].as_str(),
            request.headers["x-client-request-id"].to_str().ok()
        );
        assert_eq!(
            body["turnMetadata"].as_str(),
            metadata["x-codex-turn-metadata"].as_str()
        );
        assert_eq!(
            request.headers["x-codex-turn-metadata"].to_str().ok(),
            metadata["x-codex-turn-metadata"].as_str()
        );
    }
    assert_eq!(primary_body["turnState"], "turn-state-primary");
    assert_eq!(primary_metadata["x-codex-turn-state"], "turn-state-primary");
    assert!(secondary_body.get("turnState").is_none());
    assert!(secondary_metadata.get("x-codex-turn-state").is_none());
    assert!(secondary.headers.get("x-codex-turn-state").is_none());
    assert_eq!(secondary_metadata["safe"], "preserved");
    assert_eq!(
        secondary_body["input"][0]["role"],
        primary_body["input"][0]["role"]
    );
    assert_eq!(
        secondary_body["input"][0]["content"],
        primary_body["input"][0]["content"]
    );
    assert_eq!(primary_body["input"][0]["id"], "client_user_message_id");
    assert!(secondary_body["input"][0].get("id").is_none());
    assert_eq!(
        secondary_body["input"][0]["tool_arguments"]["id"],
        "client_tool_argument_id"
    );
    assert_eq!(primary_body["input"][1]["id"], "reasoning_account_primary");
    assert_eq!(
        primary_body["input"][1]["encrypted_content"],
        "encrypted_reasoning_account_primary"
    );
    assert!(secondary_body["input"][1].get("id").is_none());
    assert!(
        secondary_body["input"][1]
            .get("encrypted_content")
            .is_none()
    );
    assert_eq!(
        secondary_body["input"][1]["summary"][0]["id"],
        "nested_reasoning_summary_id"
    );
    assert!(secondary_body.get("previous_response_id").is_none());
}

#[tokio::test]
async fn responses_should_traverse_all_configured_strategy_candidates_without_fixed_cap() {
    let server = MockServer::start().await;
    for index in 0..6 {
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .and(header("authorization", format!("Bearer access-{index}")))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {
                    "type": "invalid_request_error",
                    "code": "token_invalidated",
                    "message": "token is no longer valid"
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-6"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (app, api_key, _dir) = test_app_with_ranked_accounts(server.uri(), 7).await;
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

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(server.received_requests().await.unwrap().len(), 7);
}

#[tokio::test]
async fn responses_stream_should_rotate_account_after_5xx_without_same_account_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-primary"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {"message": "temporary upstream failure"}
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
    assert_eq!(primary_requests, 1, "requests: {authorizations:?}");
    assert_eq!(secondary_requests, 1, "requests: {authorizations:?}");
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();
    let failed_usage: Option<(i64,)> =
        sqlx::query_as("select request_count from account_usage where account_id = $1")
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();
    let secondary_usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = $1",
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
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

    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_telemetry(server.uri()).await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .header("x-request-id", "req_responses_429_exhausted")
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
    let quota_state: (bool, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "select quota_limit_reached, quota_cooldown_until from accounts where id = $1",
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
    assert!(quota_state.0);
    assert!(quota_state.1.is_some());
    let event = latest_response_ops_error_log(&pool).await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();
    assert_eq!(event.level, "error");
    assert_eq!(
        event.request_id.as_deref(),
        Some("req_responses_429_exhausted")
    );
    assert_eq!(event.account_id.as_deref(), Some("acct_chat"));
    assert_eq!(event.route.as_deref(), Some("/v1/responses"));
    assert_eq!(event.status_code, Some(429));
    assert_eq!(metadata["stream"], false);
    assert_eq!(event.transport.as_deref(), Some("http_sse"));
    assert_eq!(event.failure_class.as_deref(), Some("rate_limited"));
    assert_eq!(metadata["exhaustedCount"], 1);
    assert!(
        metadata["upstreamError"]
            .as_str()
            .is_some_and(|error| error.contains("rate limited"))
    );
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

    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_telemetry(server.uri()).await;
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
    let response_request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response_text(response).await;
    let quota_state: (bool, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "select quota_limit_reached, quota_cooldown_until from accounts where id = $1",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(content_type.starts_with("application/json"));
    assert!(body.contains("rate_limit_exceeded"));
    assert!(body.contains("All accounts exhausted (1 rate-limited)"));
    assert!(body.contains("rate limited"));
    assert!(quota_state.0);
    assert!(quota_state.1.is_some());
    assert!(!response_request_id.is_empty());
    let event = latest_response_ops_error_log(&pool).await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();
    assert_eq!(event.level, "error");
    assert_eq!(
        event.request_id.as_deref(),
        Some(response_request_id.as_str())
    );
    assert_eq!(event.account_id.as_deref(), Some("acct_chat"));
    assert_eq!(event.route.as_deref(), Some("/v1/responses"));
    assert_eq!(event.status_code, Some(429));
    assert_eq!(metadata["stream"], true);
    assert_eq!(event.transport.as_deref(), Some("http_sse"));
    assert_eq!(event.failure_class.as_deref(), Some("rate_limited"));
    assert_eq!(metadata["exhaustedCount"], 1);
    assert!(
        metadata["upstreamError"]
            .as_str()
            .is_some_and(|error| error.contains("rate limited"))
    );
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(content_type.starts_with("application/json"));
    assert!(body.contains("\"code\":\"insufficient_quota\""));
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
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_after_402");
    assert_eq!(account_status.0, "quota_exhausted");
}
