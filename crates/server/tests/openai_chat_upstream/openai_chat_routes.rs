use super::*;

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
    assert_eq!(body["model"], "gpt-5.5");
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
    assert_eq!(body["usage"]["prompt_tokens"], 9);
    assert_eq!(body["usage"]["completion_tokens"], 3);
    let usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_chat")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(usage, (1, 9, 3));
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
    assert_eq!(body["error"]["code"], "upstream_error");
    assert_eq!(quota_state.0, 1);
    assert!(quota_state.1.is_some());
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
    assert_eq!(body["error"]["code"], "upstream_error");
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
    assert_eq!(body["error"]["code"], "upstream_error");
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
            "error": {"message": "account banned"}
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
    let cookie_store = SqliteCookieStore::new(pool.clone(), SecretBox::new([83u8; 32]));
    cookie_store
        .set_cookie_header("acct_primary", "cf_clearance=old")
        .await
        .unwrap();
    cookie_store
        .set_cookie_header("acct_secondary", "cf_clearance=keep")
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

    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert!(message.contains("All accounts exhausted (1 quota-exhausted)"));
    assert!(message.contains("quota reached"));
    assert_eq!(body["error"]["code"], "upstream_error");
    assert_eq!(account_status.0, "quota_exhausted");
}
