use super::*;

#[tokio::test]
async fn responses_websocket_should_stream_first_frame_before_terminal_event() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (first_frame_tx, first_frame_rx) = oneshot::channel();
    let (terminal_tx, terminal_rx) = oneshot::channel();
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let payload = serde_json::from_str::<Value>(&message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.created",
                    "response": {
                        "id": "resp_ws_streaming",
                        "status": "in_progress"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        first_frame_tx.send(()).unwrap();
        terminal_rx.await.unwrap();
        websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_ws_streaming").into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        payload
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;
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
                        "input": [],
                        "stream": true,
                        "use_websocket": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
    });

    first_frame_rx.await.unwrap();
    let response = timeout(StdDuration::from_millis(250), response_task)
        .await
        .expect("websocket response should be returned after the first non-error frame")
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let first_chunk = timeout(
        StdDuration::from_secs(1),
        first_response_body_chunk(response),
    )
    .await
    .expect("downstream should receive a websocket-backed SSE chunk before completion")
    .expect("response body should produce a chunk");
    terminal_tx.send(()).unwrap();
    let payload = upstream.await.unwrap();

    assert!(first_chunk.contains("event: response.created"));
    assert_eq!(payload["type"], "response.create");
}

#[tokio::test]
async fn responses_websocket_stream_should_synthesize_response_failed_when_closed_before_terminal()
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let payload = serde_json::from_str::<Value>(&message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "partial before websocket close"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        payload
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

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
                        "stream": true,
                        "use_websocket": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_text(response).await;
    let payload = upstream.await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(payload["type"], "response.create");
    assert!(body.contains("event: response.output_text.delta"));
    assert!(body.contains("event: response.failed"));
    assert!(body.contains("stream_disconnected"));
}

#[tokio::test]
async fn responses_websocket_cyber_policy_should_change_account_on_next_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let first_payload = accept_cyber_policy_websocket_response_with_authorization(
            &listener,
            "Bearer access-primary",
            "resp_cyber_first",
        )
        .await;

        let second_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secondary",
            websocket_completed_response("resp_after_cyber_rotation", 3, 1),
        )
        .await;
        let third_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-primary",
            websocket_completed_response("resp_after_cyber_clear", 3, 1),
        )
        .await;
        (first_payload, second_payload, third_payload)
    });
    let (app, state, api_key, pool, _dir) =
        test_app_with_two_accounts_and_state_config(base_url, |config| {
            config.auth.rotation_strategy = "round_robin".to_string();
        })
        .await;
    let request_body = json!({
        "model": "gpt-5.5",
        "prompt_cache_key": "conv_cyber_next_request",
        "input": [{"role": "user", "content": "Security assessment"}],
        "stream": true,
        "use_websocket": true
    });

    let first_response = app
        .clone()
        .oneshot(responses_json_request(&api_key, &request_body))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    let mut first_body_stream = first_response.into_body().into_data_stream();
    let mut first_body = String::new();
    while let Some(chunk) = first_body_stream.next().await {
        first_body.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
        if first_body.contains("\"code\":\"cyber_policy\"") {
            break;
        }
    }
    drop(first_body_stream);
    assert!(first_body.contains("partial output before policy failure"));
    assert!(first_body.contains("\"code\":\"cyber_policy\""));
    let statuses: Vec<(String, String)> =
        sqlx::query_as("select id, status from accounts order by id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        statuses,
        vec![
            ("acct_primary".to_string(), "active".to_string()),
            ("acct_secondary".to_string(), "active".to_string()),
        ]
    );

    let second_response = app
        .clone()
        .oneshot(responses_json_request(&api_key, &request_body))
        .await
        .unwrap();
    let mut second_body_stream = second_response.into_body().into_data_stream();
    let mut second_body = String::new();
    while let Some(chunk) = second_body_stream.next().await {
        second_body.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
        if second_body.contains("resp_after_cyber_rotation") {
            break;
        }
    }
    drop(second_body_stream);
    assert!(second_body.contains("resp_after_cyber_rotation"));

    assert!(
        state
            .services
            .account_pool
            .set_status("acct_secondary", AccountStatus::Disabled)
            .await
    );
    let third_response = app
        .oneshot(responses_json_request(&api_key, &request_body))
        .await
        .unwrap();
    let third_body = response_text(third_response).await;
    let (first_payload, second_payload, third_payload) = upstream.await.unwrap();

    assert!(third_body.contains("resp_after_cyber_clear"));
    assert_eq!(first_payload["type"], "response.create");
    assert_eq!(second_payload["type"], "response.create");
    assert_eq!(third_payload["type"], "response.create");
}

#[tokio::test]
async fn responses_websocket_cyber_policy_should_stop_after_three_accounts() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (three_attempts_tx, three_attempts_rx) = oneshot::channel();
    let (verify_no_fourth_tx, verify_no_fourth_rx) = oneshot::channel();
    let upstream = tokio::spawn(async move {
        let mut payloads = Vec::new();
        for (index, authorization) in ["Bearer access-0", "Bearer access-1", "Bearer access-2"]
            .into_iter()
            .enumerate()
        {
            payloads.push(
                accept_cyber_policy_websocket_response_with_authorization(
                    &listener,
                    authorization,
                    &format!("resp_cyber_cap_{index}"),
                )
                .await,
            );
        }
        three_attempts_tx.send(()).unwrap();
        verify_no_fourth_rx.await.unwrap();
        assert!(
            timeout(StdDuration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "a fourth upstream account must not be contacted"
        );
        payloads
    });
    let (app, api_key, _dir) = test_app_with_ranked_accounts(base_url, 4).await;
    let request_body = json!({
        "model": "gpt-5.5",
        "prompt_cache_key": "conv_cyber_three_account_cap",
        "input": [{"role": "user", "content": "Security assessment"}],
        "stream": true,
        "use_websocket": true
    });

    for _ in 0..3 {
        let response = app
            .clone()
            .oneshot(responses_json_request(&api_key, &request_body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response_text(response).await.contains("cyber_policy"));
    }
    three_attempts_rx.await.unwrap();

    let fourth_response = app
        .oneshot(responses_json_request(&api_key, &request_body))
        .await
        .unwrap();
    assert_eq!(fourth_response.status(), StatusCode::BAD_REQUEST);
    let fourth_body = response_json(fourth_response).await;
    assert_eq!(fourth_body["error"]["code"], "cyber_policy");
    assert_eq!(
        fourth_body["error"]["message"],
        "This request has been flagged for possible cybersecurity risk."
    );
    verify_no_fourth_tx.send(()).unwrap();
    assert_eq!(upstream.await.unwrap().len(), 3);
}

#[tokio::test]
async fn responses_websocket_cyber_policy_state_should_isolate_session_and_api_key() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let first = accept_cyber_policy_websocket_response_with_authorization(
            &listener,
            "Bearer access-primary",
            "resp_cyber_isolation",
        )
        .await;
        let second = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-primary",
            websocket_completed_response("resp_other_session", 3, 1),
        )
        .await;
        let third = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-primary",
            websocket_completed_response("resp_other_api_key", 3, 1),
        )
        .await;
        (first, second, third)
    });
    let (app, state, first_api_key, pool, _dir) =
        test_app_with_two_accounts_and_state(base_url).await;
    assert!(
        state
            .services
            .account_pool
            .set_status("acct_secondary", AccountStatus::Disabled)
            .await
    );
    let first_session = json!({
        "model": "gpt-5.5",
        "prompt_cache_key": "conv_cyber_isolation_a",
        "input": [],
        "stream": true,
        "use_websocket": true
    });
    let other_session = json!({
        "model": "gpt-5.5",
        "prompt_cache_key": "conv_cyber_isolation_b",
        "input": [],
        "stream": true,
        "use_websocket": true
    });

    let first_response = app
        .clone()
        .oneshot(responses_json_request(&first_api_key, &first_session))
        .await
        .unwrap();
    assert!(response_text(first_response).await.contains("cyber_policy"));

    let other_session_response = app
        .clone()
        .oneshot(responses_json_request(&first_api_key, &other_session))
        .await
        .unwrap();
    assert!(
        response_text(other_session_response)
            .await
            .contains("resp_other_session")
    );

    let second_api_key = insert_client_api_key(&pool).await;
    let other_key_response = app
        .oneshot(responses_json_request(&second_api_key, &first_session))
        .await
        .unwrap();
    assert!(
        response_text(other_key_response)
            .await
            .contains("resp_other_api_key")
    );
    let _ = upstream.await.unwrap();
}

#[tokio::test]
async fn responses_websocket_cyber_policy_should_not_move_external_previous_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let first = accept_cyber_policy_websocket_response_with_authorization(
            &listener,
            "Bearer access-primary",
            "resp_cyber_external_previous",
        )
        .await;
        let second = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-primary",
            websocket_completed_response("resp_same_external_previous_account", 3, 1),
        )
        .await;
        (first, second)
    });
    let (app, state, api_key, _pool, _dir) = test_app_with_two_accounts_and_state(base_url).await;
    assert!(
        state
            .services
            .account_pool
            .set_status("acct_secondary", AccountStatus::Disabled)
            .await
    );
    let request_body = json!({
        "model": "gpt-5.5",
        "prompt_cache_key": "conv_cyber_external_previous",
        "previous_response_id": "resp_external_unknown",
        "input": [],
        "stream": true,
        "use_websocket": true
    });

    let first_response = app
        .clone()
        .oneshot(responses_json_request(&api_key, &request_body))
        .await
        .unwrap();
    assert!(response_text(first_response).await.contains("cyber_policy"));
    let second_response = app
        .oneshot(responses_json_request(&api_key, &request_body))
        .await
        .unwrap();
    assert!(
        response_text(second_response)
            .await
            .contains("resp_same_external_previous_account")
    );
    let _ = upstream.await.unwrap();
}

#[tokio::test]
async fn responses_websocket_stream_first_error_429_should_retry_fallback_account() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_websocket =
            accept_websocket_with_authorization(first_stream, "Bearer access-primary").await;
        let first_payload = send_websocket_response_and_capture_payload(
            &mut first_websocket,
            json!({
                "type": "error",
                "error": {
                    "type": "usage_limit_reached",
                    "message": "The usage limit has been reached",
                    "retry_after_seconds": 77
                },
                "status_code": 429
            })
            .to_string(),
        )
        .await;
        first_websocket.close(None).await.unwrap();

        let second_payload = accept_successful_websocket_response_with_authorization(
            &listener,
            "Bearer access-secondary",
            "resp_after_ws_stream_rate_limit",
        )
        .await;
        (first_payload, second_payload)
    });
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "use_websocket": true
            }),
        ))
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
    let (first_payload, second_payload) = upstream.await.unwrap();
    let primary_quota_state: (bool, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "select quota_limit_reached, quota_cooldown_until from accounts where id = $1",
    )
    .bind("acct_primary")
    .fetch_one(&pool)
    .await
    .unwrap();
    let primary_usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = $1")
            .bind("acct_primary")
            .fetch_one(&pool)
            .await
            .unwrap();
    let secondary_usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = $1")
            .bind("acct_secondary")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(body.contains("resp_after_ws_stream_rate_limit"));
    assert_eq!(first_payload["type"], "response.create");
    assert_eq!(second_payload["type"], "response.create");
    assert!(primary_quota_state.0);
    assert!(primary_quota_state.1.is_some());
    assert_eq!(primary_usage.0, 1);
    assert_eq!(secondary_usage.0, 1);
}

#[tokio::test]
async fn responses_websocket_should_reuse_connection_for_recorded_conversation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_payload = serde_json::from_str::<Value>(&first_message.into_text().unwrap())
            .expect("first websocket payload should be json");
        websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_pool_first").into(),
            ))
            .await
            .unwrap();

        loop {
            tokio::select! {
                message = websocket.next() => {
                    match message {
                        Some(Ok(message)) if message.is_text() => {
                            let second_payload = serde_json::from_str::<Value>(
                                &message.into_text().unwrap(),
                            )
                            .expect("second websocket payload should be json");
                            websocket
                                .send(Message::Text(
                                    response_completed_websocket_message("resp_pool_second").into(),
                                ))
                                .await
                                .unwrap();
                            websocket.close(None).await.unwrap();
                            break (true, first_payload, second_payload);
                        }
                        Some(_) => {}
                        None => {
                            let second_payload = accept_successful_websocket_response(
                                &listener,
                                "resp_pool_second",
                            )
                            .await;
                            break (false, first_payload, second_payload);
                        }
                    }
                }
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let mut second_websocket = accept_async(stream).await.unwrap();
                    let second_message = second_websocket.next().await.unwrap().unwrap();
                    let second_payload = serde_json::from_str::<Value>(
                        &second_message.into_text().unwrap(),
                    )
                    .expect("second websocket payload should be json");
                    second_websocket
                        .send(Message::Text(
                            response_completed_websocket_message("resp_pool_second").into(),
                        ))
                        .await
                        .unwrap();
                    second_websocket.close(None).await.unwrap();
                    break (false, first_payload, second_payload);
                }
            }
        }
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
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
                        "input": [{
                            "role": "user",
                            "content": "reuse this upstream websocket"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert!(
        response_text(first_response)
            .await
            .contains("\"id\":\"resp_pool_first\"")
    );

    let second_response = app
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
                        "previous_response_id": "resp_pool_first"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    assert!(
        response_text(second_response)
            .await
            .contains("\"id\":\"resp_pool_second\"")
    );
    let (reused_connection, first_payload, second_payload) = upstream.await.unwrap();

    assert!(reused_connection, "second request opened a new websocket");
    assert_eq!(
        second_payload["prompt_cache_key"], first_payload["prompt_cache_key"],
        "pooled websocket reuse should stay on the recorded conversation key"
    );
    assert_eq!(second_payload["previous_response_id"], "resp_pool_first");
}

#[tokio::test]
async fn responses_websocket_should_not_reuse_connection_when_pool_is_disabled() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_payload = serde_json::from_str::<Value>(&first_message.into_text().unwrap())
            .expect("first websocket payload should be json");
        websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_disabled_pool_first").into(),
            ))
            .await
            .unwrap();

        loop {
            tokio::select! {
                message = websocket.next() => {
                    match message {
                        Some(Ok(message)) if message.is_text() => {
                            let second_payload = serde_json::from_str::<Value>(
                                &message.into_text().unwrap(),
                            )
                            .expect("second websocket payload should be json");
                            websocket
                                .send(Message::Text(
                                    response_completed_websocket_message(
                                        "resp_disabled_pool_second",
                                    )
                                    .into(),
                                ))
                                .await
                                .unwrap();
                            websocket.close(None).await.unwrap();
                            break (true, first_payload, second_payload);
                        }
                        Some(_) => {}
                        None => {
                            let second_payload = accept_successful_websocket_response(
                                &listener,
                                "resp_disabled_pool_second",
                            )
                            .await;
                            break (false, first_payload, second_payload);
                        }
                    }
                }
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let mut second_websocket = accept_async(stream).await.unwrap();
                    let second_message = second_websocket.next().await.unwrap().unwrap();
                    let second_payload = serde_json::from_str::<Value>(
                        &second_message.into_text().unwrap(),
                    )
                    .expect("second websocket payload should be json");
                    second_websocket
                        .send(Message::Text(
                            response_completed_websocket_message("resp_disabled_pool_second")
                                .into(),
                        ))
                        .await
                        .unwrap();
                    second_websocket.close(None).await.unwrap();
                    break (false, first_payload, second_payload);
                }
            }
        }
    });
    let (app, api_key, _pool, _dir) = test_app_with_account_pool_config(base_url, |config| {
        config.ws_pool.enabled = false;
    })
    .await;

    let first_response = app
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
                        "input": [{
                            "role": "user",
                            "content": "do not reuse this upstream websocket"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert!(
        response_text(first_response)
            .await
            .contains("\"id\":\"resp_disabled_pool_first\"")
    );

    let second_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", format!("Bearer {api_key}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.5",
                        "input": [{
                            "role": "user",
                            "content": "open a second independent websocket"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    assert!(
        response_text(second_response)
            .await
            .contains("\"id\":\"resp_disabled_pool_second\"")
    );
    let (reused_connection, first_payload, second_payload) = upstream.await.unwrap();

    assert!(
        !reused_connection,
        "disabled pool reused the upstream websocket"
    );
    assert!(first_payload.get("prompt_cache_key").is_none());
    assert!(second_payload.get("prompt_cache_key").is_none());
    assert!(second_payload.get("previous_response_id").is_none());
    assert_eq!(
        second_payload["input"][0]["content"],
        "open a second independent websocket"
    );
}

#[tokio::test]
async fn responses_websocket_pool_should_be_evicted_after_admin_account_status_cycle() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            websocket_completed_response("resp_pool_status_first", 4, 1),
        )
        .await;

        tokio::select! {
            message = websocket.next() => {
                match message {
                    Some(Ok(message)) if message.is_text() => {
                        websocket
                            .send(Message::Text(
                                websocket_completed_response("resp_pool_status_second", 3, 1).into(),
                            ))
                            .await
                            .unwrap();
                        let _ = websocket.close(None).await;
                        true
                    }
                    _ => {
                        accept_websocket_response_with_message(
                            &listener,
                            websocket_completed_response("resp_pool_status_second", 3, 1),
                        )
                        .await;
                        false
                    }
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.unwrap();
                let mut second_websocket = accept_async(stream).await.unwrap();
                send_websocket_response_and_capture_payload(
                    &mut second_websocket,
                    websocket_completed_response("resp_pool_status_second", 3, 1),
                )
                .await;
                second_websocket.close(None).await.unwrap();
                let _ = websocket.close(None).await;
                false
            }
        }
    });
    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(base_url).await;
    seed_openai_admin_key(&pool, "admin-status-cycle").await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "prompt_cache_key": "status-cycle"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert!(
        response_text(first_response)
            .await
            .contains("\"id\":\"resp_pool_status_first\"")
    );

    update_admin_account_status(&app, "acct_chat", "disabled").await;
    update_admin_account_status(&app, "acct_chat", "active").await;

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [{
                    "role": "user",
                    "content": "start again after the account status cycle"
                }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    assert!(
        response_text(second_response)
            .await
            .contains("\"id\":\"resp_pool_status_second\"")
    );

    let reused_connection = upstream.await.unwrap();
    assert!(
        !reused_connection,
        "admin status lifecycle should evict the old pooled websocket"
    );
}

#[tokio::test]
async fn responses_websocket_should_route_previous_response_id_to_recorded_account() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (_reused_connection, first_payload, second_payload) =
            accept_two_successful_websocket_responses_with_authorization(
                &listener,
                "Bearer access-primary",
                "resp_affinity_first",
                "resp_affinity_second",
            )
            .await;
        (first_payload, second_payload)
    });
    let (app, state, api_key, _pool, _dir) = test_app_with_two_accounts_and_state(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "input": [{
                    "role": "user",
                    "content": "keep this conversation on the same account"
                }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert!(
        response_text(first_response)
            .await
            .contains("\"id\":\"resp_affinity_first\"")
    );
    let stored_affinity = state
        .services
        .session_affinity
        .lookup("resp_affinity_first", Utc::now())
        .await
        .unwrap();
    assert_eq!(stored_affinity.account_id, "acct_primary");
    assert!(!stored_affinity.conversation_id.is_empty());
    assert!(stored_affinity.function_call_ids.is_empty());
    assert_eq!(stored_affinity.input_tokens, Some(3));

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "previous_response_id": "resp_affinity_first"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = response_text(second_response).await;
    assert!(
        second_body.contains("\"id\":\"resp_affinity_second\""),
        "{second_body}"
    );
    let (first_payload, second_payload) = upstream.await.unwrap();

    assert!(first_payload.get("prompt_cache_key").is_none());
    assert_eq!(
        second_payload["previous_response_id"],
        "resp_affinity_first"
    );
    assert!(second_payload.get("prompt_cache_key").is_none());
}

#[tokio::test]
async fn responses_websocket_should_prefer_conversation_account_without_previous_response_id() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        accept_two_successful_websocket_responses_with_authorization(
            &listener,
            "Bearer access-secondary",
            "resp_conversation_seed",
            "resp_conversation_affinity",
        )
        .await
    });
    let (app, state, api_key, _pool, _dir) =
        test_app_with_two_accounts_and_state_config(base_url, |config| {
            config.auth.rotation_strategy = "round_robin".to_string();
        })
        .await;
    let request_body = json!({
        "model": "gpt-5.5",
        "prompt_cache_key": "conv_conversation_affinity",
        "input": [],
        "stream": false,
        "use_websocket": true
    });
    assert!(
        state
            .services
            .account_pool
            .set_status("acct_primary", AccountStatus::Disabled)
            .await
    );
    let first_response = app
        .clone()
        .oneshot(responses_json_request(&api_key, &request_body))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert!(
        response_text(first_response)
            .await
            .contains("\"id\":\"resp_conversation_seed\"")
    );
    assert!(
        state
            .services
            .account_pool
            .set_status("acct_primary", AccountStatus::Active)
            .await
    );

    let second_response = app
        .oneshot(responses_json_request(&api_key, &request_body))
        .await
        .unwrap();
    let body = response_text(second_response).await;
    let (_reused, first_payload, second_payload) = upstream.await.unwrap();

    assert!(body.contains("\"id\":\"resp_conversation_affinity\""));
    assert!(
        first_payload["prompt_cache_key"]
            .as_str()
            .is_some_and(|value| value.starts_with("wi_"))
    );
    assert_eq!(
        second_payload["prompt_cache_key"],
        first_payload["prompt_cache_key"]
    );
    assert!(second_payload.get("previous_response_id").is_none());
}

#[tokio::test]
async fn responses_websocket_non_stream_should_forward_unknown_previous_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            websocket_completed_response("resp_unknown_previous_forwarded", 3, 1),
        )
        .await
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": false,
                "previous_response_id": "resp_missing"
            }),
        ))
        .await
        .unwrap();
    let body = response_json(response).await;
    let payload = upstream.await.unwrap();

    assert_eq!(body["id"], "resp_unknown_previous_forwarded");
    assert_eq!(payload["previous_response_id"], "resp_missing");
}

#[tokio::test]
async fn responses_websocket_stream_should_forward_unknown_previous_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            websocket_completed_response("resp_stream_unknown_previous_forwarded", 3, 1),
        )
        .await
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "previous_response_id": "resp_missing"
            }),
        ))
        .await
        .unwrap();
    let body = response_text(response).await;
    let payload = upstream.await.unwrap();

    assert!(body.contains("\"id\":\"resp_stream_unknown_previous_forwarded\""));
    assert_eq!(payload["previous_response_id"], "resp_missing");
}

#[tokio::test]
async fn responses_websocket_stream_should_not_fan_out_external_previous_after_429() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-primary",
            429,
            "Too Many Requests",
            Some(77),
            WEBSOCKET_HISTORY_RATE_LIMITED,
        )
        .await;
    });
    let (app, api_key, _pool, _dir) = test_app_with_two_accounts(base_url).await;

    let response = timeout(
        StdDuration::from_secs(2),
        app.oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "previous_response_id": "resp_prev"
            }),
        )),
    )
    .await
    .expect("external previous should stop after the selected account")
    .unwrap();
    let status = response.status();
    let body = response_text(response).await;
    upstream.await.unwrap();

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(body.contains("history account rate limited"));
}

#[tokio::test]
async fn responses_websocket_non_stream_should_not_fan_out_external_previous_after_429() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-primary",
            429,
            "Too Many Requests",
            Some(77),
            WEBSOCKET_HISTORY_RATE_LIMITED,
        )
        .await;
    });
    let (app, api_key, _pool, _dir) = test_app_with_two_accounts(base_url).await;

    let response = timeout(
        StdDuration::from_secs(2),
        app.oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": false,
                "previous_response_id": "resp_prev"
            }),
        )),
    )
    .await
    .expect("external previous should stop after the selected account")
    .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    upstream.await.unwrap();

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["message"], "history account rate limited");
}

#[tokio::test]
async fn responses_websocket_without_history_should_mark_expired_after_fallback_401() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-primary",
            429,
            "Too Many Requests",
            Some(30),
            WEBSOCKET_RATE_LIMITED,
        )
        .await;
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-secondary",
            401,
            "Unauthorized",
            None,
            WEBSOCKET_TOKEN_REVOKED,
        )
        .await;
    });
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "use_websocket": true
            }),
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = response_text(response).await;
    upstream.await.unwrap();
    let secondary_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_secondary")
        .fetch_one(&pool)
        .await
        .unwrap();
    let primary_usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = $1",
    )
    .bind("acct_primary")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("\"type\":\"invalid_request_error\""));
    assert!(body.contains("\"code\":\"invalid_api_key\""));
    assert!(body.contains("All accounts exhausted"));
    assert!(body.contains("token_revoked"));
    assert_eq!(secondary_status.0, "expired");
    assert_eq!(primary_usage, (1, 0, 0));
}

#[tokio::test]
async fn responses_websocket_without_history_should_return_rate_limit_stream_error_when_fallback_accounts_exhausted()
 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-primary",
            429,
            "Too Many Requests",
            Some(11),
            WEBSOCKET_FIRST_ACCOUNT_LIMITED,
        )
        .await;
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-secondary",
            429,
            "Too Many Requests",
            Some(22),
            WEBSOCKET_SECOND_ACCOUNT_LIMITED,
        )
        .await;
    });
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "use_websocket": true
            }),
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = response_text(response).await;
    upstream.await.unwrap();
    let primary_usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = $1")
            .bind("acct_primary")
            .fetch_one(&pool)
            .await
            .unwrap();
    let secondary_usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = $1")
            .bind("acct_secondary")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_openai_error_body(
        &body,
        "rate_limit_error",
        "rate_limit_exceeded",
        &[
            "All accounts exhausted (2 rate-limited)",
            "second account limited",
        ],
    );
    assert_eq!(primary_usage.0, 1);
    assert_eq!(secondary_usage.0, 1);
}

#[tokio::test]
async fn responses_websocket_response_failed_quota_should_retry_fallback_account() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_websocket =
            accept_websocket_with_authorization(first_stream, "Bearer access-primary").await;
        let _first_payload = send_websocket_response_and_capture_payload(
            &mut first_websocket,
            json!({
                "type": "response.failed",
                "response": {
                    "id": "resp_ws_quota_failed",
                    "error": {
                        "code": "insufficient_quota",
                        "message": "quota exhausted"
                    }
                }
            })
            .to_string(),
        )
        .await;

        let (second_stream, _) = listener.accept().await.unwrap();
        let mut second_websocket =
            accept_websocket_with_authorization(second_stream, "Bearer access-secondary").await;
        send_websocket_response_and_capture_payload(
            &mut second_websocket,
            websocket_completed_response("resp_after_ws_quota", 3, 1),
        )
        .await;
        second_websocket.close(None).await.unwrap();
    });
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": false,
                "use_websocket": true
            }),
        ))
        .await
        .unwrap();
    let body = response_json(response).await;
    upstream.await.unwrap();
    let primary_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(body["id"], "resp_after_ws_quota");
    assert_eq!(primary_status.0, "quota_exhausted");
}

#[tokio::test]
async fn responses_websocket_without_history_should_return_quota_stream_error_when_402_has_no_fallback()
 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-secret",
            402,
            "Payment Required",
            None,
            r#"{"error":{"message":"quota reached"}}"#,
        )
        .await;
    });
    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "use_websocket": true
            }),
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = response_text(response).await;
    upstream.await.unwrap();
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_openai_error_body(
        &body,
        "insufficient_quota",
        "insufficient_quota",
        &[
            "All accounts exhausted (1 quota-exhausted)",
            "quota reached",
        ],
    );
    assert_eq!(account_status.0, "quota_exhausted");
}

#[tokio::test]
async fn responses_websocket_without_history_should_return_model_unsupported_stream_error_when_no_fallback()
 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-secret",
            400,
            "Bad Request",
            None,
            r#"{"error":{"code":"model_not_available","message":"Model gpt-5.5 is not available on this account plan"}}"#,
        )
        .await;
    });
    let (app, api_key, pool, _dir) = test_app_with_account_and_pool(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "use_websocket": true
            }),
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = response_text(response).await;
    upstream.await.unwrap();
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = $1")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_openai_error_body(
        &body,
        "invalid_request_error",
        "model_not_found",
        &[
            "All accounts exhausted",
            "model_not_available",
            "not available",
        ],
    );
    assert_eq!(account_status.0, "active");
}

#[tokio::test]
async fn responses_websocket_with_history_should_return_path_block_stream_error_when_no_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-secret",
            404,
            "Not Found",
            None,
            "",
        )
        .await;
    });
    let (app, api_key, _pool, _dir) = test_app_with_account_and_pool(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": true,
                "previous_response_id": "resp_prev"
            }),
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = response_text(response).await;
    upstream.await.unwrap();

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_openai_error_body(
        &body,
        "invalid_request_error",
        "codex_client_error",
        &["Upstream Codex request failed"],
    );
}

#[tokio::test]
async fn responses_with_previous_response_id_should_use_websocket_and_configured_pool() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_async(first_stream).await.unwrap();
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_runtime_pool_first").into(),
            ))
            .await
            .unwrap();

        tokio::select! {
            second_message = first_websocket.next() => {
                let _second_message = second_message.unwrap().unwrap();
                first_websocket
                    .send(Message::Text(
                        response_completed_websocket_message("resp_runtime_pool_second").into(),
                    ))
                    .await
                    .unwrap();
                first_websocket.close(None).await.unwrap();
            }
            accepted = listener.accept() => {
                let (second_stream, _) = accepted.unwrap();
                accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
                let mut second_websocket = accept_async(second_stream).await.unwrap();
                let _second_message = second_websocket.next().await.unwrap().unwrap();
                second_websocket
                    .send(Message::Text(
                        response_completed_websocket_message("resp_runtime_pool_second").into(),
                    ))
                    .await
                    .unwrap();
                second_websocket.close(None).await.unwrap();
                first_websocket.close(None).await.unwrap();
            }
        }
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first = app
        .clone()
        .oneshot(responses_previous_request(
            &api_key,
            "Continue from pooled runtime websocket",
        ))
        .await
        .unwrap();
    let second = app
        .oneshot(responses_previous_request(
            &api_key,
            "Continue again from pooled runtime websocket",
        ))
        .await
        .unwrap();
    server.await.unwrap();

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn responses_stream_with_previous_response_id_should_forward_websocket_chunks_before_completion()
 {
    let (base_url, first_chunk_sent_rx, finish_tx, upstream) =
        spawn_chunked_websocket_upstream().await;
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let response = app
        .oneshot(responses_previous_stream_request(
            &api_key,
            "Continue as a WebSocket stream",
        ))
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    first_chunk_sent_rx
        .await
        .expect("upstream should send the first websocket event");
    let first_chunk = timeout(
        StdDuration::from_secs(1),
        first_response_body_chunk(response),
    )
    .await
    .expect("downstream should receive a websocket-backed SSE chunk before upstream completes")
    .expect("response body should produce a chunk");
    finish_tx
        .send(())
        .expect("test should be able to finish upstream websocket");
    let captured = upstream.await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/event-stream"));
    assert!(first_chunk.contains("event: response.output_text.delta"));
    assert_eq!(
        captured_header(&captured.headers, "authorization"),
        Some("Bearer access-secret")
    );
    assert_eq!(
        captured.payload["previous_response_id"],
        "resp_runtime_pool_previous"
    );
}

#[tokio::test]
async fn responses_stream_with_previous_response_id_should_record_websocket_audit_metadata() {
    let (base_url, first_chunk_sent_rx, finish_tx, upstream) =
        spawn_chunked_websocket_upstream().await;
    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_telemetry(base_url).await;

    let response = app
        .oneshot(responses_previous_stream_request(
            &api_key,
            "Continue as a logged WebSocket stream",
        ))
        .await
        .unwrap();
    first_chunk_sent_rx
        .await
        .expect("upstream should send the first websocket event");
    finish_tx
        .send(())
        .expect("test should be able to finish upstream websocket");
    let body = response_text(response).await;
    let captured = upstream.await.unwrap();
    let event = latest_response_usage_record(&pool).await;
    let metadata: Value = serde_json::from_str(&event.metadata_json).unwrap();

    assert!(body.contains("resp_live_websocket_stream"));
    assert_eq!(
        captured.payload["previous_response_id"],
        "resp_runtime_pool_previous"
    );
    assert_eq!(event.level, "info");
    assert_eq!(metadata["stream"], true);
    assert_eq!(event.transport.as_deref(), Some("websocket"));
    assert_eq!(event.input_tokens, Some(3));
    assert_eq!(event.output_tokens, Some(1));
    assert!(
        event.first_token_ms.is_some_and(|value| value > 0),
        "websocket stream usage metadata should include initial event latency: {metadata:?}",
    );
    assert_rate_limit_header(&metadata, "x-codex-primary-used-percent", "44");
    assert_rate_limit_header(&metadata, "x-codex-primary-window-minutes", "5");
    let stored_quota: (Option<Value>,) =
        sqlx::query_as("select quota_json from accounts where id = $1")
            .bind("acct_chat")
            .fetch_one(&pool)
            .await
            .unwrap();
    let quota = stored_quota.0.unwrap();
    assert_eq!(
        quota["snapshots"][0]["primary"]["used_percent"].as_f64(),
        Some(44.0)
    );
}
