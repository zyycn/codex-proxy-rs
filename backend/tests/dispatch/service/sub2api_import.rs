use super::*;

#[tokio::test]
async fn sub2api_opaque_access_token_with_future_expiry_should_handle_normal_responses_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "account_id": "sub2api-opaque-account",
            "user_id": "sub2api-opaque-user",
            "email": "sub2api-opaque@example.com",
            "plan_type": "team",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 0,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 18_000
                }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/backend-api/codex/responses"))
        .and(header("authorization", "Bearer at-sub2api-opaque-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(RESPONSES_SUCCESS_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;

    let (pool, _dir) = init_test_db("sub2api-opaque-normal-responses").await;
    let api_key = insert_client_api_key(&pool).await;
    let state = test_app_state_with_pool(
        &test_config(test_database_url(), format!("{}/backend-api", server.uri())),
        pool,
    )
    .await;
    let imported = state
        .services
        .admin_accounts
        .import(json!({
            "sourceFormat": "sub2api",
            "exported_at": "2026-07-19T18:05:29Z",
            "proxies": [],
            "accounts": [{
                "name": "sub2api-opaque@example.com",
                "platform": "openai",
                "type": "oauth",
                "credentials": {
                    "access_token": "at-sub2api-opaque-token",
                    "chatgpt_account_id": "sub2api-opaque-account",
                    "chatgpt_user_id": "sub2api-opaque-user",
                    "email": "sub2api-opaque@example.com",
                    "plan_type": "team",
                    "refresh_token": "",
                    "expires_at": "2099-12-01T00:00:00Z"
                },
                "auto_pause_on_expired": true,
                "concurrency": 30,
                "priority": 1,
                "type": "oauth"
            }]
        }))
        .await
        .unwrap();
    assert_eq!(imported.imported, 1);

    let app = router::router().with_state(state);
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

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "resp_response_1");
}
