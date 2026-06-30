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
                    "type": "response.output_text.delta",
                    "delta": "first websocket frame"
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

    assert!(first_chunk.contains("event: response.output_text.delta"));
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
    assert!(response_text(first_response)
        .await
        .contains("\"id\":\"resp_pool_first\""));

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
    assert!(response_text(second_response)
        .await
        .contains("\"id\":\"resp_pool_second\""));
    let (reused_connection, first_payload, second_payload) = upstream.await.unwrap();

    assert!(reused_connection, "second request opened a new websocket");
    assert_eq!(
        second_payload["prompt_cache_key"], first_payload["prompt_cache_key"],
        "pooled websocket reuse should stay on the recorded conversation key"
    );
    assert_eq!(second_payload["previous_response_id"], "resp_pool_first");
}

#[tokio::test]
async fn responses_websocket_should_retry_fresh_connection_when_reused_connection_dies_before_first_frame(
) {
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
                response_completed_websocket_message("resp_stale_reuse_first").into(),
            ))
            .await
            .unwrap();

        let stale_message = websocket.next().await.unwrap().unwrap();
        let stale_payload = serde_json::from_str::<Value>(&stale_message.into_text().unwrap())
            .expect("stale reused websocket payload should be json");
        websocket.close(None).await.unwrap();

        let fresh_payload =
            accept_successful_websocket_response(&listener, "resp_after_stale_reuse").await;

        (first_payload, stale_payload, fresh_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [{
                    "role": "user",
                    "content": "prime stale websocket reuse"
                }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert!(response_text(first_response)
        .await
        .contains("\"id\":\"resp_stale_reuse_first\""));

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "previous_response_id": "resp_stale_reuse_first"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = response_text(second_response).await;
    assert!(
        second_body.contains("\"id\":\"resp_after_stale_reuse\""),
        "{second_body}"
    );
    let (first_payload, stale_payload, fresh_payload) = upstream.await.unwrap();

    assert_eq!(
        stale_payload["previous_response_id"],
        "resp_stale_reuse_first"
    );
    assert_eq!(
        fresh_payload["previous_response_id"],
        "resp_stale_reuse_first"
    );
    assert_eq!(
        fresh_payload["prompt_cache_key"], first_payload["prompt_cache_key"],
        "fresh retry should keep the recorded conversation identity"
    );
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
    assert!(response_text(first_response)
        .await
        .contains("\"id\":\"resp_disabled_pool_first\""));

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
                        "previous_response_id": "resp_disabled_pool_first"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    assert!(response_text(second_response)
        .await
        .contains("\"id\":\"resp_disabled_pool_second\""));
    let (reused_connection, first_payload, second_payload) = upstream.await.unwrap();

    assert!(
        !reused_connection,
        "disabled pool reused the upstream websocket"
    );
    assert_eq!(
        second_payload["prompt_cache_key"], first_payload["prompt_cache_key"],
        "disabling the pool must not change the recorded conversation key"
    );
    assert_eq!(
        second_payload["previous_response_id"],
        "resp_disabled_pool_first"
    );
}

#[tokio::test]
async fn responses_websocket_stream_should_record_metadata_turn_state_for_continuation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_websocket = accept_async(first_stream).await.unwrap();
        let first_message = first_websocket.next().await.unwrap().unwrap();
        let first_payload = serde_json::from_str::<Value>(&first_message.into_text().unwrap())
            .expect("first websocket payload should be json");
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.metadata",
                    "headers": {
                        "x-codex-turn-state": "turn-from-metadata"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        first_websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_metadata_turn_state").into(),
            ))
            .await
            .unwrap();
        first_websocket.close(None).await.unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        let second_headers = Arc::new(Mutex::new(Vec::new()));
        let second_headers_for_callback = Arc::clone(&second_headers);
        let mut second_websocket = accept_hdr_async(second_stream, move |request, _response| {
            *second_headers_for_callback.lock().unwrap() = request_headers(request);
        })
        .await
        .unwrap();
        let second_message = second_websocket.next().await.unwrap().unwrap();
        let second_payload = serde_json::from_str::<Value>(&second_message.into_text().unwrap())
            .expect("second websocket payload should be json");
        second_websocket
            .send(Message::Text(
                response_completed_websocket_message("resp_metadata_turn_state_next").into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
        let second_headers = second_headers.lock().unwrap().clone();
        (first_payload, second_payload, second_headers)
    });
    let (app, api_key, pool, _dir) = test_app_with_account_pool_config(base_url, |config| {
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
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = response_text(first_response).await;
    assert!(first_body.contains("\"id\":\"resp_metadata_turn_state\""));
    assert!(!first_body.contains("response.metadata"));
    assert_eq!(
        wait_for_session_affinity_turn_state(&pool, "resp_metadata_turn_state").await,
        Some("turn-from-metadata".to_string())
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
                        "previous_response_id": "resp_metadata_turn_state"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    assert!(response_text(second_response)
        .await
        .contains("\"id\":\"resp_metadata_turn_state_next\""));
    let (first_payload, second_payload, second_headers) = upstream.await.unwrap();

    assert!(first_payload.get("previous_response_id").is_none());
    assert_eq!(
        second_payload["previous_response_id"],
        "resp_metadata_turn_state"
    );
    assert_eq!(
        captured_header(&second_headers, "x-codex-turn-state"),
        Some("turn-from-metadata")
    );
}

#[tokio::test]
async fn responses_websocket_should_implicitly_resume_full_history_with_reasoning_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().to_string(),
        )
        .await;
        let second_payload = accept_followup_websocket_response(
            &listener,
            &mut websocket,
            websocket_completed_response("resp_implicit_resume_second", 4, 1),
        )
        .await;
        let _ = websocket.close(None).await;
        (first_payload, second_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [{"role": "user", "content": "remember this"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [
                    {"role": "user", "content": "remember this"},
                    {"role": "assistant", "content": "cached answer"},
                    {"role": "user", "content": "continue"}
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_payload, mut second_payload) = upstream.await.unwrap();
    assert!(first_payload["prompt_cache_key"].as_str().is_some());
    assert_eq!(
        second_payload["previous_response_id"],
        "resp_implicit_resume_first"
    );
    assert_eq!(
        second_payload["prompt_cache_key"],
        first_payload["prompt_cache_key"]
    );
    assert_eq!(
        second_payload["input"][0]["encrypted_content"],
        "enc_reasoning_replay"
    );
    assert_eq!(second_payload["input"][1]["content"], "continue");
    assert_eq!(second_payload["input"].as_array().unwrap().len(), 2);

    second_payload
        .as_object_mut()
        .unwrap()
        .remove("prompt_cache_key");
    second_payload
        .as_object_mut()
        .unwrap()
        .remove("client_metadata");
    let expected: Value = serde_json::from_str(REASONING_REPLAY_REQUEST_GOLDEN).unwrap();
    assert_eq!(second_payload, expected);
}

#[tokio::test]
async fn responses_websocket_should_strip_implicit_disabled_affinity_when_switching_accounts() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_websocket =
            accept_websocket_with_authorization(first_stream, "Bearer access-primary").await;
        let first_payload = send_websocket_response_and_capture_payload(
            &mut first_websocket,
            websocket_completed_response("resp_implicit_ban_first", 4, 1),
        )
        .await;
        first_websocket.close(None).await.unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        let second_headers = Arc::new(Mutex::new(Vec::new()));
        let second_headers_for_callback = Arc::clone(&second_headers);
        let mut second_websocket = accept_hdr_async(second_stream, move |request, _response| {
            *second_headers_for_callback.lock().unwrap() = request_headers(request);
        })
        .await
        .unwrap();
        let second_payload = send_websocket_response_and_capture_payload(
            &mut second_websocket,
            websocket_completed_response("resp_implicit_ban_second", 3, 1),
        )
        .await;
        second_websocket.close(None).await.unwrap();
        let captured_second_headers = second_headers.lock().unwrap().clone();

        (first_payload, second_payload, captured_second_headers)
    });
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;
    seed_openai_admin_session(&pool, "session_status_cycle").await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [{"role": "user", "content": "remember this"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert!(response_text(first_response)
        .await
        .contains("\"id\":\"resp_implicit_ban_first\""));

    update_admin_account_status(&app, "acct_primary", "disabled").await;

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [
                    {"role": "user", "content": "remember this"},
                    {"role": "assistant", "content": "cached answer"},
                    {"role": "user", "content": "continue"}
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    assert!(response_text(second_response)
        .await
        .contains("\"id\":\"resp_implicit_ban_second\""));
    let (first_payload, second_payload, second_headers) = upstream.await.unwrap();
    let affinity_count: (i64,) =
        sqlx::query_as("select count(*) from session_affinities where response_id = ?")
            .bind("resp_implicit_ban_first")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert!(first_payload["prompt_cache_key"].as_str().is_some());
    assert_eq!(
        captured_header(&second_headers, "authorization"),
        Some("Bearer access-secondary")
    );
    assert!(captured_header(&second_headers, "x-codex-turn-state").is_none());
    assert!(second_payload.get("previous_response_id").is_none());
    assert_eq!(second_payload["input"].as_array().unwrap().len(), 3);
    assert_eq!(affinity_count.0, 0);
}

#[tokio::test]
async fn responses_websocket_should_not_implicitly_resume_unmatched_function_call_output() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            websocket_completed_function_call_response("resp_call_first", "call_expected"),
        )
        .await;
        let second_payload = accept_followup_websocket_response(
            &listener,
            &mut websocket,
            websocket_completed_response("resp_call_mismatch_second", 4, 1),
        )
        .await;
        let _ = websocket.close(None).await;
        (first_payload, second_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "use the lookup tool",
                "stream": false,
                "input": [{"role": "user", "content": "call the lookup tool"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "use the lookup tool",
                "stream": false,
                "input": [
                    {"role": "user", "content": "call the lookup tool"},
                    {
                        "type": "function_call",
                        "call_id": "call_expected",
                        "name": "lookup",
                        "arguments": "{}"
                    },
                    {
                        "type": "function_call_output",
                        "call_id": "call_missing",
                        "output": "tool output"
                    }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_payload, second_payload) = upstream.await.unwrap();
    assert!(first_payload["prompt_cache_key"].as_str().is_some());
    assert!(second_payload.get("previous_response_id").is_none());
    assert_eq!(second_payload["input"].as_array().unwrap().len(), 3);
    assert_eq!(second_payload["input"][2]["call_id"], "call_missing");
}

#[tokio::test]
async fn responses_websocket_should_implicitly_resume_after_sqlite_affinity_restore() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server_base_url = base_url.clone();
    let upstream = tokio::spawn(async move {
        let first_payload = accept_websocket_response_with_message(
            &listener,
            WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().to_string(),
        )
        .await;
        let second_payload = accept_websocket_response_with_message(
            &listener,
            websocket_completed_response("resp_restored_implicit_resume", 4, 1),
        )
        .await;
        (first_payload, second_payload)
    });
    let (app, api_key, pool, dir) = test_app_with_account_pool_config(base_url, |_| {}).await;

    let first_response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [{"role": "user", "content": "remember this"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let db = dir.path().join("openai-record-affinity.sqlite");
    let restored_state = test_app_state_with_pool_and_installation_id(
        &test_config(format!("sqlite://{}", db.display()), server_base_url),
        pool.clone(),
        TEST_INSTALLATION_ID.to_string(),
    )
    .await;
    assert_eq!(
        restored_state
            .services
            .account_pool
            .restore_from_repository()
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        restored_state
            .services
            .session_affinity
            .restore_from_repository(Utc::now())
            .await
            .unwrap(),
        1
    );
    let restored_app = router::router().with_state(restored_state);

    let second_response = restored_app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [
                    {"role": "user", "content": "remember this"},
                    {"role": "assistant", "content": "cached answer"},
                    {"role": "user", "content": "continue"}
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_payload, second_payload) = upstream.await.unwrap();
    assert_eq!(
        second_payload["prompt_cache_key"],
        first_payload["prompt_cache_key"]
    );
    assert_eq!(
        second_payload["previous_response_id"],
        "resp_implicit_resume_first"
    );
    assert_eq!(second_payload["input"].as_array().unwrap().len(), 1);
    assert_eq!(second_payload["input"][0]["content"], "continue");
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
    seed_openai_admin_session(&pool, "session_status_cycle").await;

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
    assert!(response_text(first_response)
        .await
        .contains("\"id\":\"resp_pool_status_first\""));

    update_admin_account_status(&app, "acct_chat", "disabled").await;
    update_admin_account_status(&app, "acct_chat", "active").await;

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "previous_response_id": "resp_pool_status_first"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    assert!(response_text(second_response)
        .await
        .contains("\"id\":\"resp_pool_status_second\""));

    let reused_connection = upstream.await.unwrap();
    assert!(
        !reused_connection,
        "admin status lifecycle should evict the old pooled websocket"
    );
}

#[tokio::test]
async fn responses_websocket_should_not_implicitly_resume_self_contained_function_call_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            websocket_completed_function_call_response("resp_self_contained_first", "call_self"),
        )
        .await;
        let second_payload = accept_followup_websocket_response(
            &listener,
            &mut websocket,
            websocket_completed_response("resp_self_contained_second", 4, 1),
        )
        .await;
        let _ = websocket.close(None).await;
        (first_payload, second_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "use the lookup tool",
                "stream": false,
                "input": [{"role": "user", "content": "call the lookup tool"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "use the lookup tool",
                "stream": false,
                "input": [
                    {"role": "user", "content": "call the lookup tool"},
                    {
                        "type": "function_call",
                        "call_id": "call_self",
                        "name": "lookup",
                        "arguments": "{}"
                    },
                    {
                        "type": "function_call_output",
                        "call_id": "call_self",
                        "output": "tool output"
                    }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_payload, second_payload) = upstream.await.unwrap();
    assert!(first_payload["prompt_cache_key"].as_str().is_some());
    assert!(second_payload.get("previous_response_id").is_none());
    assert_eq!(second_payload["input"].as_array().unwrap().len(), 3);
    assert_eq!(second_payload["input"][2]["call_id"], "call_self");
}

#[tokio::test]
async fn responses_websocket_should_not_implicitly_resume_across_codex_windows() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().to_string(),
        )
        .await;
        let second_payload = accept_followup_websocket_response(
            &listener,
            &mut websocket,
            websocket_completed_response("resp_window_b", 8, 2),
        )
        .await;
        let _ = websocket.close(None).await;
        (first_payload, second_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "prompt_cache_key": "shared-variant-session",
                "codexWindowId": "window-a",
                "stream": false,
                "input": [{"role": "user", "content": "remember this"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "prompt_cache_key": "shared-variant-session",
                "codexWindowId": "window-b",
                "stream": false,
                "input": [
                    {"role": "user", "content": "remember this"},
                    {"role": "assistant", "content": "cached answer"},
                    {"role": "user", "content": "continue in another window"}
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_payload, second_payload) = upstream.await.unwrap();
    assert_eq!(
        second_payload["prompt_cache_key"],
        first_payload["prompt_cache_key"]
    );
    assert!(second_payload.get("previous_response_id").is_none());
    assert_eq!(second_payload["input"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn responses_websocket_should_evict_reasoning_replay_after_invalid_encrypted_content() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().to_string(),
        )
        .await;
        let invalid_payload = accept_followup_websocket_response(
            &listener,
            &mut websocket,
            WEBSOCKET_INVALID_ENCRYPTED_CONTENT.trim().to_string(),
        )
        .await;
        let _ = websocket.close(None).await;
        let retried_payload = accept_websocket_response_with_message(
            &listener,
            websocket_completed_response("resp_after_replay_eviction", 4, 1),
        )
        .await;
        (first_payload, invalid_payload, retried_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [{"role": "user", "content": "remember this"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [
                    {"role": "user", "content": "remember this"},
                    {"role": "assistant", "content": "cached answer"},
                    {"role": "user", "content": "continue"}
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_payload, invalid_payload, retried_payload) = upstream.await.unwrap();
    assert_eq!(
        invalid_payload["previous_response_id"],
        "resp_implicit_resume_first"
    );
    assert_eq!(
        invalid_payload["input"][0]["encrypted_content"],
        "enc_reasoning_replay"
    );
    assert_eq!(
        retried_payload["prompt_cache_key"],
        first_payload["prompt_cache_key"]
    );
    assert!(retried_payload.get("previous_response_id").is_none());
    let retried_input = retried_payload["input"].as_array().unwrap();
    assert!(retried_input
        .iter()
        .all(|item| item.get("encrypted_content").is_none()));
    assert_eq!(retried_input.last().unwrap()["content"], "continue");
}

#[tokio::test]
async fn responses_websocket_should_restore_full_history_when_implicit_resume_previous_response_is_missing(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_payload = send_websocket_response_and_capture_payload(
            &mut websocket,
            WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().to_string(),
        )
        .await;
        let implicit_payload = accept_followup_websocket_response(
            &listener,
            &mut websocket,
            WEBSOCKET_PREVIOUS_RESPONSE_NOT_FOUND.trim().to_string(),
        )
        .await;
        let _ = websocket.close(None).await;
        let restored_payload = accept_websocket_response_with_message(
            &listener,
            websocket_completed_response("resp_implicit_resume_restored", 10, 2),
        )
        .await;
        (first_payload, implicit_payload, restored_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let first_response = app
        .clone()
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [{"role": "user", "content": "remember this"}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "instructions": "answer briefly",
                "stream": false,
                "input": [
                    {"role": "user", "content": "remember this"},
                    {"role": "assistant", "content": "cached answer"},
                    {"role": "user", "content": "continue"}
                ]
            }),
        ))
        .await
        .unwrap();
    let body = response_json(second_response).await;
    let (_first_payload, implicit_payload, restored_payload) = upstream.await.unwrap();
    assert_eq!(body["id"], "resp_implicit_resume_restored");
    assert_eq!(
        implicit_payload["previous_response_id"],
        "resp_implicit_resume_first"
    );
    assert_eq!(implicit_payload["input"].as_array().unwrap().len(), 2);
    assert!(restored_payload.get("previous_response_id").is_none());
    assert_eq!(restored_payload["input"].as_array().unwrap().len(), 3);
    assert_eq!(restored_payload["input"][0]["role"], "user");
    assert_eq!(restored_payload["input"][1]["role"], "assistant");
    assert_eq!(restored_payload["input"][2]["content"], "continue");
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
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;

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
    assert!(response_text(first_response)
        .await
        .contains("\"id\":\"resp_affinity_first\""));
    let stored_affinity: (String, String, String, Option<i64>, String) = sqlx::query_as(
        "select account_id, conversation_id, function_call_ids_json, input_tokens, expires_at from session_affinities where response_id = ?",
    )
    .bind("resp_affinity_first")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored_affinity.0, "acct_primary");
    assert!(!stored_affinity.1.is_empty());
    assert_eq!(stored_affinity.2, "[]");
    assert_eq!(stored_affinity.3, Some(3));
    assert!(!stored_affinity.4.is_empty());

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

    assert_ne!(first_payload["prompt_cache_key"], Value::Null);
    assert_eq!(
        second_payload["previous_response_id"],
        "resp_affinity_first"
    );
    assert_eq!(
        second_payload["prompt_cache_key"], first_payload["prompt_cache_key"],
        "previous_response_id should inherit the recorded conversation identity"
    );
}

#[tokio::test]
async fn responses_websocket_non_stream_previous_response_not_found_should_strip_history_and_retry_same_account(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let first_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            WEBSOCKET_PREVIOUS_RESPONSE_NOT_FOUND.trim().to_string(),
        )
        .await;
        let second_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            websocket_completed_response("resp_after_history_strip", 3, 1),
        )
        .await;
        (first_payload, second_payload)
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
    let (first_payload, second_payload) = upstream.await.unwrap();

    assert_eq!(body["id"], "resp_after_history_strip");
    assert_eq!(first_payload["previous_response_id"], "resp_missing");
    assert!(second_payload.get("previous_response_id").is_none());
}

#[tokio::test]
async fn responses_websocket_stream_previous_response_not_found_should_strip_history_and_retry_same_account(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let first_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            WEBSOCKET_PREVIOUS_RESPONSE_NOT_FOUND.trim().to_string(),
        )
        .await;
        let second_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            websocket_completed_response("resp_stream_after_history_strip", 3, 1),
        )
        .await;
        (first_payload, second_payload)
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
    let (first_payload, second_payload) = upstream.await.unwrap();

    assert!(body.contains("\"id\":\"resp_stream_after_history_strip\""));
    assert_eq!(first_payload["previous_response_id"], "resp_missing");
    assert!(second_payload.get("previous_response_id").is_none());
}

#[tokio::test]
async fn responses_websocket_non_stream_unanswered_function_call_should_strip_history_and_retry_same_account(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let upstream = tokio::spawn(async move {
        let first_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            WEBSOCKET_UNANSWERED_FUNCTION_CALL.trim().to_string(),
        )
        .await;
        let second_payload = accept_websocket_response_with_authorization_and_message(
            &listener,
            "Bearer access-secret",
            websocket_completed_response("resp_after_function_call_strip", 3, 1),
        )
        .await;
        (first_payload, second_payload)
    });
    let (app, api_key, _dir) = test_app_with_account(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": false,
                "previous_response_id": "resp_with_call"
            }),
        ))
        .await
        .unwrap();
    let body = response_json(response).await;
    let (first_payload, second_payload) = upstream.await.unwrap();

    assert_eq!(body["id"], "resp_after_function_call_strip");
    assert_eq!(first_payload["previous_response_id"], "resp_with_call");
    assert!(second_payload.get("previous_response_id").is_none());
}

#[tokio::test]
async fn responses_websocket_previous_response_id_should_retry_fallback_account_after_429() {
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
        accept_successful_websocket_response_with_authorization(
            &listener,
            "Bearer access-secondary",
            "resp_history_fallback",
        )
        .await
    });
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "previous_response_id": "resp_prev"
            }),
        ))
        .await
        .unwrap();
    let body = response_text(response).await;
    let fallback_payload = upstream.await.unwrap();
    let secondary_usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_secondary")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert!(body.contains("\"id\":\"resp_history_fallback\""));
    assert_eq!(fallback_payload["previous_response_id"], "resp_prev");
    assert_eq!(secondary_usage.0, 1);
}

#[tokio::test]
async fn responses_websocket_non_stream_previous_response_id_should_retry_fallback_account_after_429(
) {
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
        accept_successful_websocket_response_with_authorization(
            &listener,
            "Bearer access-secondary",
            "resp_history_fallback_non_stream",
        )
        .await
    });
    let (app, api_key, pool, _dir) = test_app_with_two_accounts(base_url).await;

    let response = app
        .oneshot(responses_json_request(
            &api_key,
            &json!({
                "model": "gpt-5.5",
                "input": [],
                "stream": false,
                "previous_response_id": "resp_prev"
            }),
        ))
        .await
        .unwrap();
    let body = response_json(response).await;
    let fallback_payload = upstream.await.unwrap();
    let secondary_usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_secondary")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(body["id"], "resp_history_fallback_non_stream");
    assert_eq!(fallback_payload["previous_response_id"], "resp_prev");
    assert_eq!(secondary_usage.0, 1);
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
    let body = response_text(response).await;
    upstream.await.unwrap();
    let secondary_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_secondary")
        .fetch_one(&pool)
        .await
        .unwrap();
    let primary_usage: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_primary")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(body.contains("event: response.failed"));
    assert!(body.contains("\"type\":\"invalid_request_error\""));
    assert!(body.contains("\"code\":\"authentication_error\""));
    assert!(body.contains("All accounts exhausted"));
    assert!(body.contains("token_revoked"));
    assert_eq!(secondary_status.0, "expired");
    assert_eq!(primary_usage, (1, 0, 0));
}

#[tokio::test]
async fn responses_websocket_without_history_should_return_rate_limit_stream_error_when_fallback_accounts_exhausted(
) {
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
    let body = response_text(response).await;
    upstream.await.unwrap();
    let primary_usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_primary")
            .fetch_one(&pool)
            .await
            .unwrap();
    let secondary_usage: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_secondary")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_response_failed_stream(
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
    let primary_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_primary")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(body["id"], "resp_after_ws_quota");
    assert_eq!(primary_status.0, "quota_exhausted");
}

#[tokio::test]
async fn responses_websocket_without_history_should_return_quota_stream_error_when_402_has_no_fallback(
) {
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
    let body = response_text(response).await;
    upstream.await.unwrap();
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_response_failed_stream(
        &body,
        "invalid_request_error",
        "codex_api_error",
        &[
            "All accounts exhausted (1 quota-exhausted)",
            "quota reached",
        ],
    );
    assert_eq!(account_status.0, "quota_exhausted");
}

#[tokio::test]
async fn responses_websocket_without_history_should_return_model_unsupported_stream_error_when_no_fallback(
) {
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
    let body = response_text(response).await;
    upstream.await.unwrap();
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_chat")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_response_failed_stream(
        &body,
        "invalid_request_error",
        "codex_api_error",
        &[
            "No accounts available",
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
    let body = response_text(response).await;
    upstream.await.unwrap();

    assert_response_failed_stream(
        &body,
        "server_error",
        "codex_api_error",
        &["No accounts available", "Cloudflare path-block"],
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
async fn responses_stream_with_previous_response_id_should_forward_websocket_chunks_before_completion(
) {
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
    let (app, api_key, pool, _dir) = test_app_with_account_pool_and_logging(base_url).await;

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
    assert_eq!(metadata["transport"], "websocket");
    assert_eq!(metadata["usage"]["inputTokens"], 3);
    assert_eq!(metadata["usage"]["outputTokens"], 1);
    assert!(
        metadata["firstTokenMs"]
            .as_i64()
            .is_some_and(|value| value > 0),
        "websocket stream usage metadata should include first token latency: {metadata:?}",
    );
    assert_rate_limit_header(&metadata, "x-codex-primary-used-percent", "44");
    assert_rate_limit_header(&metadata, "x-codex-primary-window-minutes", "5");
    let stored_quota: (Option<String>,) =
        sqlx::query_as("select quota_json from accounts where id = ?")
            .bind("acct_chat")
            .fetch_one(&pool)
            .await
            .unwrap();
    let quota: Value = serde_json::from_str(stored_quota.0.as_deref().unwrap()).unwrap();
    assert_eq!(
        quota["snapshots"][0]["primary"]["used_percent"].as_f64(),
        Some(44.0)
    );
}
