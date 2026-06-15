use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::{timeout, Duration},
};
use tokio_tungstenite::{
    accept_async, accept_hdr_async,
    tungstenite::{
        handshake::server::{Request as WsRequest, Response as WsResponse},
        Message,
    },
};
use tower::ServiceExt;

use codex_proxy_rs::runtime::{build_router, state::AppState};

use crate::support::{
    assert_response_failed_stream, response_json, response_text,
    upstream::{
        build_imported_app, build_imported_app_with_accounts,
        build_imported_app_with_accounts_and_config, ImportAccount,
    },
};

const WEBSOCKET_COMPLETED_RESPONSE: &str =
    include_str!("../fixtures/responses/websocket/completed.json");
const WEBSOCKET_HISTORY_RATE_LIMITED: &str =
    include_str!("../fixtures/responses/websocket/history_rate_limited.json");
const WEBSOCKET_RATE_LIMITED: &str =
    include_str!("../fixtures/responses/websocket/rate_limited.json");
const WEBSOCKET_TOKEN_REVOKED: &str =
    include_str!("../fixtures/responses/websocket/token_revoked.json");
const WEBSOCKET_FIRST_ACCOUNT_LIMITED: &str =
    include_str!("../fixtures/responses/websocket/first_account_limited.json");
const WEBSOCKET_SECOND_ACCOUNT_LIMITED: &str =
    include_str!("../fixtures/responses/websocket/second_account_limited.json");
const WEBSOCKET_PREVIOUS_RESPONSE_NOT_FOUND: &str =
    include_str!("../fixtures/responses/websocket/previous_response_not_found.json");
const WEBSOCKET_UNANSWERED_FUNCTION_CALL: &str =
    include_str!("../fixtures/responses/websocket/unanswered_function_call.json");
const WEBSOCKET_PARTIAL_DELTA: &str =
    include_str!("../fixtures/responses/websocket/partial_delta.json");
const WEBSOCKET_INVALID_ENCRYPTED_CONTENT: &str =
    include_str!("../fixtures/responses/websocket/invalid_encrypted_content.json");
const WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY: &str =
    include_str!("../fixtures/responses/websocket/completed_with_reasoning_replay.json");
const REASONING_REPLAY_REQUEST_GOLDEN: &str =
    include_str!("../fixtures/responses/golden/reasoning_replay_request.json");

#[expect(
    clippy::result_large_err,
    reason = "tungstenite 的 header callback API 固定使用较大的 handshake error response"
)]
fn assert_access_secret_header(
    request: &WsRequest,
    response: WsResponse,
) -> Result<WsResponse, tokio_tungstenite::tungstenite::handshake::server::ErrorResponse> {
    assert_eq!(
        request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer access-secret")
    );
    Ok(response)
}

#[tokio::test]
async fn v1_responses_should_use_websocket_upstream_by_default_while_serving_sse() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let request = serde_json::from_str::<Value>(&message.into_text().unwrap()).unwrap();
        request_tx.send(request).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_route_ws_default", 6, 2).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

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
                .body(Body::from(r#"{"model":"gpt-5.5","input":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));
    let body = response_text(response).await;
    assert!(body.contains("event: response.completed"));
    assert!(body.contains("\"id\":\"resp_route_ws_default\""));
    let request = request_rx.await.unwrap();
    assert_eq!(request["type"], "response.create");
    assert_eq!(request["model"], "gpt-5.5");
    assert!(request.get("previous_response_id").is_none());
    assert!(request["prompt_cache_key"]
        .as_str()
        .is_some_and(|value| value.starts_with("cp_")));
    assert!(request["client_metadata"]["x-codex-installation-id"].is_string());
    assert!(request["client_metadata"]["x-codex-window-id"]
        .as_str()
        .is_some_and(|value| value.starts_with("cp_") && value.ends_with(":0")));
    server.await.unwrap();
}

#[tokio::test]
async fn v1_responses_should_ignore_camel_case_use_websocket_field() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let request = serde_json::from_str::<Value>(&message.into_text().unwrap()).unwrap();
        request_tx.send(request).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_route_ws_camel_case_transport", 6, 2).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

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
                    r#"{"model":"gpt-5.5","input":[],"useWebSocket":false}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let request = request_rx.await.unwrap();
    assert_eq!(request["type"], "response.create");
    assert!(request.get("useWebSocket").is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn v1_responses_websocket_should_stream_first_frame_before_terminal_event() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (first_frame_tx, first_frame_rx) = oneshot::channel();
    let (terminal_tx, terminal_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let _request = serde_json::from_str::<Value>(&message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "first"
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
                websocket_completed_response("resp_route_ws_streaming", 4, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let imported = build_imported_app(format!("http://{addr}")).await;
    let app = imported.app.clone();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/responses")
        .header(
            "authorization",
            format!("Bearer {}", imported.client_api_key),
        )
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model":"gpt-5.5","input":[],"stream":true,"use_websocket":true}"#,
        ))
        .unwrap();
    let mut response_task = tokio::spawn(async move { app.oneshot(request).await.unwrap() });

    first_frame_rx.await.unwrap();
    let response = match timeout(Duration::from_millis(250), &mut response_task).await {
        Ok(response) => response.unwrap(),
        Err(_) => {
            let _ = terminal_tx.send(());
            let _ = timeout(Duration::from_secs(1), response_task).await;
            panic!("websocket response should be returned after the first non-error frame");
        }
    };

    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let first_chunk = timeout(Duration::from_secs(1), body.next())
        .await
        .expect("first SSE chunk should arrive before terminal frame")
        .expect("stream should yield one chunk")
        .expect("chunk should be readable");
    let first_sse = String::from_utf8(first_chunk.to_vec()).unwrap();
    assert!(first_sse.contains("event: response.output_text.delta"));

    terminal_tx.send(()).unwrap();
    while let Some(chunk) = timeout(Duration::from_secs(1), body.next()).await.unwrap() {
        chunk.unwrap();
    }
    server.await.unwrap();
}

#[tokio::test]
async fn v1_responses_websocket_stream_should_synthesize_response_failed_when_closed_before_terminal(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let _request = serde_json::from_str::<Value>(&message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(WEBSOCKET_PARTIAL_DELTA.trim().into()))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

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
                    r#"{"model":"gpt-5.5","input":[],"stream":true,"use_websocket":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert!(body.contains("event: response.output_text.delta"));
    assert!(body.contains("event: response.failed"));
    assert!(body.contains("stream_disconnected"));
    server.await.unwrap();
}

#[tokio::test]
async fn v1_responses_websocket_should_reuse_connection_for_recorded_conversation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_request =
            serde_json::from_str::<Value>(&first_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_pool_first", 4, 1).into(),
            ))
            .await
            .unwrap();

        loop {
            tokio::select! {
                message = websocket.next() => {
                    match message {
                        Some(Ok(message)) if message.is_text() => {
                            let second_request = serde_json::from_str::<Value>(
                                &message.into_text().unwrap(),
                            )
                            .unwrap();
                            websocket
                                .send(Message::Text(
                                    websocket_completed_response("resp_pool_second", 3, 1).into(),
                                ))
                                .await
                                .unwrap();
                            websocket.close(None).await.unwrap();
                            break (true, first_request, second_request);
                        }
                        Some(_) => continue,
                        None => {
                            let second_request = accept_successful_websocket_response(
                                &listener,
                                "Bearer access-secret",
                                "resp_pool_second",
                            )
                            .await;
                            break (false, first_request, second_request);
                        }
                    }
                }
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let mut second_websocket = accept_async(stream).await.unwrap();
                    let second_message = second_websocket.next().await.unwrap().unwrap();
                    let second_request = serde_json::from_str::<Value>(
                        &second_message.into_text().unwrap(),
                    )
                    .unwrap();
                    second_websocket
                        .send(Message::Text(
                            websocket_completed_response("resp_pool_second", 3, 1).into(),
                        ))
                        .await
                        .unwrap();
                    second_websocket.close(None).await.unwrap();
                    break (false, first_request, second_request);
                }
            }
        }
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

    let first_response = imported
        .app
        .clone()
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
    let first_body = response_text(first_response).await;
    assert!(first_body.contains("\"id\":\"resp_pool_first\""));

    let second_response = imported
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
                    r#"{"model":"gpt-5.5","input":[],"previous_response_id":"resp_pool_first"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = response_text(second_response).await;
    assert!(second_body.contains("\"id\":\"resp_pool_second\""));

    let (reused_connection, first_request, second_request) = server.await.unwrap();
    assert!(reused_connection, "second request opened a new websocket");
    assert_eq!(
        second_request["prompt_cache_key"], first_request["prompt_cache_key"],
        "pooled websocket reuse should stay on the recorded conversation key"
    );
    assert_eq!(second_request["previous_response_id"], "resp_pool_first");
}

#[tokio::test]
async fn v1_responses_websocket_should_not_reuse_connection_when_pool_is_disabled() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_request =
            serde_json::from_str::<Value>(&first_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_disabled_pool_first", 4, 1).into(),
            ))
            .await
            .unwrap();

        loop {
            tokio::select! {
                message = websocket.next() => {
                    match message {
                        Some(Ok(message)) if message.is_text() => {
                            let second_request = serde_json::from_str::<Value>(
                                &message.into_text().unwrap(),
                            )
                            .unwrap();
                            websocket
                                .send(Message::Text(
                                    websocket_completed_response(
                                        "resp_disabled_pool_second",
                                        3,
                                        1,
                                    )
                                    .into(),
                                ))
                                .await
                                .unwrap();
                            websocket.close(None).await.unwrap();
                            break (true, first_request, second_request);
                        }
                        Some(_) => continue,
                        None => {
                            let second_request = accept_successful_websocket_response(
                                &listener,
                                "Bearer access-secret",
                                "resp_disabled_pool_second",
                            )
                            .await;
                            break (false, first_request, second_request);
                        }
                    }
                }
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let mut second_websocket = accept_async(stream).await.unwrap();
                    let second_message = second_websocket.next().await.unwrap().unwrap();
                    let second_request = serde_json::from_str::<Value>(
                        &second_message.into_text().unwrap(),
                    )
                    .unwrap();
                    second_websocket
                        .send(Message::Text(
                            websocket_completed_response("resp_disabled_pool_second", 3, 1).into(),
                        ))
                        .await
                        .unwrap();
                    second_websocket.close(None).await.unwrap();
                    break (false, first_request, second_request);
                }
            }
        }
    });
    let imported = build_imported_app_with_accounts_and_config(
        format!("http://{addr}"),
        &[ImportAccount {
            id: "acct_imported",
            account_id: "chatgpt-account",
            token: "access-secret",
            refresh_token: "refresh-secret",
        }],
        |config| {
            config.ws_pool.enabled = false;
        },
    )
    .await;

    let first_response = imported
        .app
        .clone()
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
    let first_body = response_text(first_response).await;
    assert!(first_body.contains("\"id\":\"resp_disabled_pool_first\""));

    let second_response = imported
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
                    r#"{"model":"gpt-5.5","input":[],"previous_response_id":"resp_disabled_pool_first"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = response_text(second_response).await;
    assert!(second_body.contains("\"id\":\"resp_disabled_pool_second\""));

    let (reused_connection, first_request, second_request) = server.await.unwrap();
    assert!(
        !reused_connection,
        "disabled pool reused the upstream websocket"
    );
    assert_eq!(
        second_request["prompt_cache_key"], first_request["prompt_cache_key"],
        "disabling the pool must not change the recorded conversation key"
    );
    assert_eq!(
        second_request["previous_response_id"],
        "resp_disabled_pool_first"
    );
}

#[tokio::test]
async fn v1_responses_websocket_should_implicitly_resume_full_history_continuation_with_reasoning_replay(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_request =
            serde_json::from_str::<Value>(&first_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().into(),
            ))
            .await
            .unwrap();

        let second_message = websocket.next().await.unwrap().unwrap();
        let second_request =
            serde_json::from_str::<Value>(&second_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_implicit_resume_second", 4, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        (first_request, second_request)
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

    let first_response = imported
        .app
        .clone()
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "answer briefly",
                        "stream": false,
                        "input": [{
                            "role": "user",
                            "content": "remember this"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = imported
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "answer briefly",
                        "stream": false,
                        "input": [
                            {
                                "role": "user",
                                "content": "remember this"
                            },
                            {
                                "role": "assistant",
                                "content": "cached answer"
                            },
                            {
                                "role": "user",
                                "content": "continue"
                            }
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_request, second_request) = server.await.unwrap();
    assert!(first_request["prompt_cache_key"].as_str().is_some());
    assert_eq!(
        second_request["previous_response_id"],
        "resp_implicit_resume_first"
    );
    assert_eq!(
        second_request["prompt_cache_key"], first_request["prompt_cache_key"],
        "implicit resume should reuse the recorded conversation identity"
    );
    assert_eq!(
        second_request["input"][0]["encrypted_content"],
        "enc_reasoning_replay"
    );
    assert_eq!(second_request["input"][1]["content"], "continue");
    assert_eq!(second_request["input"].as_array().unwrap().len(), 2);

    let mut replay_request = second_request;
    replay_request
        .as_object_mut()
        .unwrap()
        .remove("prompt_cache_key");
    replay_request
        .as_object_mut()
        .unwrap()
        .remove("client_metadata");
    let expected_replay_request: Value =
        serde_json::from_str(REASONING_REPLAY_REQUEST_GOLDEN).unwrap();
    assert_eq!(replay_request, expected_replay_request);
}

#[tokio::test]
async fn v1_responses_websocket_should_not_implicitly_resume_unmatched_function_call_output() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_request =
            serde_json::from_str::<Value>(&first_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_function_call_response("resp_call_first", "call_expected")
                    .into(),
            ))
            .await
            .unwrap();

        let second_message = websocket.next().await.unwrap().unwrap();
        let second_request =
            serde_json::from_str::<Value>(&second_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_call_mismatch_second", 4, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        (first_request, second_request)
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

    let first_response = imported
        .app
        .clone()
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "use the lookup tool",
                        "stream": false,
                        "input": [{
                            "role": "user",
                            "content": "call the lookup tool"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = imported
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "use the lookup tool",
                        "stream": false,
                        "input": [
                            {
                                "role": "user",
                                "content": "call the lookup tool"
                            },
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
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_request, second_request) = server.await.unwrap();
    assert!(first_request["prompt_cache_key"].as_str().is_some());
    assert!(second_request.get("previous_response_id").is_none());
    assert_eq!(second_request["input"].as_array().unwrap().len(), 3);
    assert_eq!(second_request["input"][2]["call_id"], "call_missing");
}

#[tokio::test]
async fn v1_responses_websocket_should_not_implicitly_resume_self_contained_function_call_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_request =
            serde_json::from_str::<Value>(&first_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_function_call_response(
                    "resp_self_contained_first",
                    "call_self",
                )
                .into(),
            ))
            .await
            .unwrap();

        let second_message = websocket.next().await.unwrap().unwrap();
        let second_request =
            serde_json::from_str::<Value>(&second_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_self_contained_second", 4, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        (first_request, second_request)
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

    let first_response = imported
        .app
        .clone()
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "use the lookup tool",
                        "stream": false,
                        "input": [{
                            "role": "user",
                            "content": "call the lookup tool"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = imported
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "use the lookup tool",
                        "stream": false,
                        "input": [
                            {
                                "role": "user",
                                "content": "call the lookup tool"
                            },
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
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_request, second_request) = server.await.unwrap();
    assert!(first_request["prompt_cache_key"].as_str().is_some());
    assert!(second_request.get("previous_response_id").is_none());
    assert_eq!(second_request["input"].as_array().unwrap().len(), 3);
    assert_eq!(second_request["input"][2]["call_id"], "call_self");
}

#[tokio::test]
async fn v1_responses_websocket_should_implicitly_resume_after_sqlite_affinity_restore() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_websocket = accept_async(first_stream).await.unwrap();
        let first_message = first_websocket.next().await.unwrap().unwrap();
        let first_request =
            serde_json::from_str::<Value>(&first_message.into_text().unwrap()).unwrap();
        first_websocket
            .send(Message::Text(
                WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().into(),
            ))
            .await
            .unwrap();
        first_websocket.close(None).await.unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        let mut second_websocket = accept_async(second_stream).await.unwrap();
        let second_message = second_websocket.next().await.unwrap().unwrap();
        let second_request =
            serde_json::from_str::<Value>(&second_message.into_text().unwrap()).unwrap();
        second_websocket
            .send(Message::Text(
                websocket_completed_response("resp_restored_implicit_resume", 4, 1).into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
        (first_request, second_request)
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

    let first_response = imported
        .app
        .clone()
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "answer briefly",
                        "stream": false,
                        "input": [{
                            "role": "user",
                            "content": "remember this"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let restored_state = AppState::with_pool_secret_and_api_key_hasher(
        imported.config.clone(),
        imported.pool.clone(),
        imported.secret_box.clone(),
        imported.api_key_hasher.clone(),
    );
    assert_eq!(
        restored_state
            .reload_account_pool_from_repository()
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        restored_state
            .reload_session_affinity_from_repository()
            .await
            .unwrap(),
        1
    );
    let restored_app = build_router(restored_state);

    let second_response = restored_app
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "answer briefly",
                        "stream": false,
                        "input": [
                            {
                                "role": "user",
                                "content": "remember this"
                            },
                            {
                                "role": "assistant",
                                "content": "cached answer"
                            },
                            {
                                "role": "user",
                                "content": "continue"
                            }
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_request, second_request) = server.await.unwrap();
    assert_eq!(
        second_request["prompt_cache_key"], first_request["prompt_cache_key"],
        "SQLite-restored affinity should keep the recorded conversation identity"
    );
    assert_eq!(
        second_request["previous_response_id"],
        "resp_implicit_resume_first"
    );
    assert_eq!(second_request["input"].as_array().unwrap().len(), 1);
    assert_eq!(second_request["input"][0]["content"], "continue");
}

#[tokio::test]
async fn v1_responses_websocket_should_not_implicitly_resume_across_codex_windows() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_request =
            serde_json::from_str::<Value>(&first_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().into(),
            ))
            .await
            .unwrap();

        let second_message = websocket.next().await.unwrap().unwrap();
        let second_request =
            serde_json::from_str::<Value>(&second_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_window_b", 8, 2).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        (first_request, second_request)
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

    let first_response = imported
        .app
        .clone()
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "answer briefly",
                        "prompt_cache_key": "shared-variant-session",
                        "codexWindowId": "window-a",
                        "stream": false,
                        "input": [{
                            "role": "user",
                            "content": "remember this"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = imported
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "answer briefly",
                        "prompt_cache_key": "shared-variant-session",
                        "codexWindowId": "window-b",
                        "stream": false,
                        "input": [
                            {
                                "role": "user",
                                "content": "remember this"
                            },
                            {
                                "role": "assistant",
                                "content": "cached answer"
                            },
                            {
                                "role": "user",
                                "content": "continue in another window"
                            }
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);

    let (first_request, second_request) = server.await.unwrap();
    assert_eq!(
        second_request["prompt_cache_key"], first_request["prompt_cache_key"],
        "both windows still share the same upstream conversation identity"
    );
    assert!(second_request.get("previous_response_id").is_none());
    assert_eq!(second_request["input"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn v1_responses_websocket_should_evict_reasoning_replay_after_invalid_encrypted_content() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_request =
            serde_json::from_str::<Value>(&first_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().into(),
            ))
            .await
            .unwrap();

        let invalid_message = websocket.next().await.unwrap().unwrap();
        let invalid_request =
            serde_json::from_str::<Value>(&invalid_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                WEBSOCKET_INVALID_ENCRYPTED_CONTENT.trim().into(),
            ))
            .await
            .unwrap();

        let retried_message = websocket.next().await.unwrap().unwrap();
        let retried_request =
            serde_json::from_str::<Value>(&retried_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_after_replay_eviction", 4, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        (first_request, invalid_request, retried_request)
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

    let first_response = imported
        .app
        .clone()
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "answer briefly",
                        "stream": false,
                        "input": [{
                            "role": "user",
                            "content": "remember this"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let continuation_body = json!({
        "model": "gpt-5.5",
        "instructions": "answer briefly",
        "stream": false,
        "input": [
            {
                "role": "user",
                "content": "remember this"
            },
            {
                "role": "assistant",
                "content": "cached answer"
            },
            {
                "role": "user",
                "content": "continue"
            }
        ]
    })
    .to_string();

    let invalid_response = imported
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header(
                    "authorization",
                    format!("Bearer {}", imported.client_api_key),
                )
                .header("content-type", "application/json")
                .body(Body::from(continuation_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_response.status(), StatusCode::BAD_GATEWAY);

    let retried_response = imported
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
                .body(Body::from(continuation_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(retried_response.status(), StatusCode::OK);

    let (first_request, invalid_request, retried_request) = server.await.unwrap();
    assert_eq!(
        invalid_request["previous_response_id"],
        "resp_implicit_resume_first"
    );
    assert_eq!(
        invalid_request["input"][0]["encrypted_content"],
        "enc_reasoning_replay"
    );
    assert_eq!(
        retried_request["prompt_cache_key"], first_request["prompt_cache_key"],
        "the replay eviction must not change the conversation identity"
    );
    assert_eq!(
        retried_request["previous_response_id"],
        "resp_implicit_resume_first"
    );
    assert_eq!(retried_request["input"].as_array().unwrap().len(), 1);
    assert_eq!(retried_request["input"][0]["content"], "continue");
}

#[tokio::test]
async fn v1_responses_websocket_should_restore_full_history_when_implicit_resume_previous_response_is_missing(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_request =
            serde_json::from_str::<Value>(&first_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                WEBSOCKET_COMPLETED_WITH_REASONING_REPLAY.trim().into(),
            ))
            .await
            .unwrap();

        let implicit_message = websocket.next().await.unwrap().unwrap();
        let implicit_request =
            serde_json::from_str::<Value>(&implicit_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(WEBSOCKET_PREVIOUS_RESPONSE_NOT_FOUND.into()))
            .await
            .unwrap();

        let restored_message = websocket.next().await.unwrap().unwrap();
        let restored_request =
            serde_json::from_str::<Value>(&restored_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_implicit_resume_restored", 10, 2).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        (first_request, implicit_request, restored_request)
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

    let first_response = imported
        .app
        .clone()
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "answer briefly",
                        "stream": false,
                        "input": [{
                            "role": "user",
                            "content": "remember this"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = imported
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "answer briefly",
                        "stream": false,
                        "input": [
                            {
                                "role": "user",
                                "content": "remember this"
                            },
                            {
                                "role": "assistant",
                                "content": "cached answer"
                            },
                            {
                                "role": "user",
                                "content": "continue"
                            }
                        ]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    let body = response_json(second_response).await;
    assert_eq!(body["id"], "resp_implicit_resume_restored");

    let (_first_request, implicit_request, restored_request) = server.await.unwrap();
    assert_eq!(
        implicit_request["previous_response_id"],
        "resp_implicit_resume_first"
    );
    assert_eq!(implicit_request["input"].as_array().unwrap().len(), 2);
    assert!(restored_request.get("previous_response_id").is_none());
    assert_eq!(restored_request["input"].as_array().unwrap().len(), 3);
    assert_eq!(restored_request["input"][0]["role"], "user");
    assert_eq!(restored_request["input"][1]["role"], "assistant");
    assert_eq!(restored_request["input"][2]["content"], "continue");
}

#[tokio::test]
async fn v1_responses_websocket_pool_should_be_evicted_after_admin_account_status_cycle() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _first_message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_pool_status_first", 4, 1).into(),
            ))
            .await
            .unwrap();

        loop {
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
                            websocket.close(None).await.unwrap();
                            break true;
                        }
                        Some(_) => continue,
                        None => {
                            accept_successful_websocket_response(
                                &listener,
                                "Bearer access-secret",
                                "resp_pool_status_second",
                            )
                            .await;
                            break false;
                        }
                    }
                }
                accepted = listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let mut second_websocket = accept_async(stream).await.unwrap();
                    let _second_message = second_websocket.next().await.unwrap().unwrap();
                    second_websocket
                        .send(Message::Text(
                            websocket_completed_response("resp_pool_status_second", 3, 1).into(),
                        ))
                        .await
                        .unwrap();
                    second_websocket.close(None).await.unwrap();
                    break false;
                }
            }
        }
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

    let first_response = imported
        .app
        .clone()
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
                    r#"{"model":"gpt-5.5","input":[],"prompt_cache_key":"status-cycle"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = response_text(first_response).await;
    assert!(first_body.contains("\"id\":\"resp_pool_status_first\""));

    update_admin_account_status(&imported.app, "acct_imported", "disabled").await;
    update_admin_account_status(&imported.app, "acct_imported", "active").await;

    let second_response = imported
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
                    r#"{"model":"gpt-5.5","input":[],"previous_response_id":"resp_pool_status_first"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = response_text(second_response).await;
    assert!(second_body.contains("\"id\":\"resp_pool_status_second\""));

    let reused_connection = server.await.unwrap();
    assert!(
        !reused_connection,
        "admin status lifecycle should evict the old pooled websocket"
    );
}

#[tokio::test]
async fn v1_responses_should_route_previous_response_id_to_recorded_account() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let first = accept_successful_websocket_response(
            &listener,
            "Bearer access-a",
            "resp_affinity_first",
        )
        .await;
        let second = accept_successful_websocket_response(
            &listener,
            "Bearer access-a",
            "resp_affinity_second",
        )
        .await;
        (first, second)
    });
    let imported = build_imported_app_with_accounts(
        format!("http://{addr}"),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;

    let first_response = imported
        .app
        .clone()
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
                    json!({
                        "model": "gpt-5.5",
                        "instructions": "answer briefly",
                        "input": [{
                            "role": "user",
                            "content": "keep this conversation on the same account"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = response_text(first_response).await;
    assert!(first_body.contains("\"id\":\"resp_affinity_first\""));
    let stored_affinity: (String, String, String, Option<i64>, String) = sqlx::query_as(
        "select account_id, conversation_id, function_call_ids_json, input_tokens, expires_at from session_affinities where response_id = ?",
    )
    .bind("resp_affinity_first")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    assert_eq!(stored_affinity.0, "acct_a");
    assert!(!stored_affinity.1.is_empty());
    assert_eq!(stored_affinity.2, "[]");
    assert_eq!(stored_affinity.3, Some(3));
    assert!(!stored_affinity.4.is_empty());

    let second_response = imported
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
                    r#"{"model":"gpt-5.5","input":[],"previous_response_id":"resp_affinity_first"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = response_text(second_response).await;
    assert!(second_body.contains("\"id\":\"resp_affinity_second\""));

    let (first_request, second_request) = server.await.unwrap();
    assert_ne!(
        first_request["prompt_cache_key"],
        Value::Null,
        "first request should establish an upstream prompt cache key"
    );
    assert_eq!(
        second_request["previous_response_id"],
        "resp_affinity_first"
    );
    assert_eq!(
        second_request["prompt_cache_key"], first_request["prompt_cache_key"],
        "previous_response_id should inherit the recorded conversation identity"
    );
}

#[tokio::test]
async fn v1_responses_non_stream_previous_response_not_found_should_strip_history_and_retry_same_account(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_hdr_async(stream, assert_access_secret_header)
            .await
            .unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_request =
            serde_json::from_str::<Value>(&first_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(WEBSOCKET_PREVIOUS_RESPONSE_NOT_FOUND.into()))
            .await
            .unwrap();
        let second_message = websocket.next().await.unwrap().unwrap();
        let second_request =
            serde_json::from_str::<Value>(&second_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_after_history_strip", 3, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        (first_request, second_request)
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

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
                    r#"{"model":"gpt-5.5","input":[],"stream":false,"previous_response_id":"resp_missing"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_after_history_strip");
    let (first_request, second_request) = server.await.unwrap();
    assert_eq!(first_request["previous_response_id"], "resp_missing");
    assert!(second_request.get("previous_response_id").is_none());
}

#[tokio::test]
async fn v1_responses_stream_previous_response_not_found_should_strip_history_and_retry_same_account(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_hdr_async(stream, assert_access_secret_header)
            .await
            .unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_request =
            serde_json::from_str::<Value>(&first_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(WEBSOCKET_PREVIOUS_RESPONSE_NOT_FOUND.into()))
            .await
            .unwrap();
        let second_message = websocket.next().await.unwrap().unwrap();
        let second_request =
            serde_json::from_str::<Value>(&second_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_stream_after_history_strip", 3, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        (first_request, second_request)
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

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
                    r#"{"model":"gpt-5.5","input":[],"stream":true,"previous_response_id":"resp_missing"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert!(body.contains("\"id\":\"resp_stream_after_history_strip\""));
    let (first_request, second_request) = server.await.unwrap();
    assert_eq!(first_request["previous_response_id"], "resp_missing");
    assert!(second_request.get("previous_response_id").is_none());
}

#[tokio::test]
async fn v1_responses_non_stream_unanswered_function_call_should_strip_history_and_retry_same_account(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_hdr_async(stream, assert_access_secret_header)
            .await
            .unwrap();
        let first_message = websocket.next().await.unwrap().unwrap();
        let first_request =
            serde_json::from_str::<Value>(&first_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(WEBSOCKET_UNANSWERED_FUNCTION_CALL.into()))
            .await
            .unwrap();
        let second_message = websocket.next().await.unwrap().unwrap();
        let second_request =
            serde_json::from_str::<Value>(&second_message.into_text().unwrap()).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_after_function_call_strip", 3, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        (first_request, second_request)
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

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
                    r#"{"model":"gpt-5.5","input":[],"stream":false,"previous_response_id":"resp_with_call"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_after_function_call_strip");
    let (first_request, second_request) = server.await.unwrap();
    assert_eq!(first_request["previous_response_id"], "resp_with_call");
    assert!(second_request.get("previous_response_id").is_none());
}

#[tokio::test]
async fn v1_responses_should_use_websocket_for_previous_response_id_streaming() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let request = serde_json::from_str::<Value>(&message.into_text().unwrap()).unwrap();
        request_tx.send(request).unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_route_ws", 8, 5).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let imported = build_imported_app(format!("http://{addr}")).await;

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
                    r#"{"model":"gpt-5.5","input":[],"previous_response_id":"resp_prev"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));
    let body = response_text(response).await;
    assert!(body.contains("event: response.completed"));
    assert!(body.contains("\"id\":\"resp_route_ws\""));
    let request = request_rx.await.unwrap();
    assert_eq!(request["type"], "response.create");
    assert_eq!(request["previous_response_id"], "resp_prev");
    server.await.unwrap();
}

#[tokio::test]
async fn v1_responses_previous_response_id_websocket_429_should_retry_fallback_account() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-a",
            429,
            "Too Many Requests",
            Some(77),
            WEBSOCKET_HISTORY_RATE_LIMITED,
        )
        .await;
        accept_successful_websocket_response(&listener, "Bearer access-b", "resp_history_fallback")
            .await
    });
    let imported = build_imported_app_with_accounts(
        format!("http://{addr}"),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;

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
                .header("x-request-id", "req_ws_history_429")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"previous_response_id":"resp_prev"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert!(body.contains("\"id\":\"resp_history_fallback\""));
    let fallback_request = server.await.unwrap();
    assert_eq!(fallback_request["previous_response_id"], "resp_prev");
    let account_b_usage =
        sqlx::query_as::<_, (i64,)>("select count(*) from account_usage where account_id = ?")
            .bind("acct_b")
            .fetch_one(&imported.pool)
            .await
            .unwrap();
    assert_eq!(account_b_usage.0, 1);
}

#[tokio::test]
async fn v1_responses_non_stream_previous_response_id_websocket_429_should_retry_fallback_account()
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-a",
            429,
            "Too Many Requests",
            Some(77),
            WEBSOCKET_HISTORY_RATE_LIMITED,
        )
        .await;
        accept_successful_websocket_response(
            &listener,
            "Bearer access-b",
            "resp_history_fallback_non_stream",
        )
        .await
    });
    let imported = build_imported_app_with_accounts(
        format!("http://{addr}"),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;

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
                .header("x-request-id", "req_ws_history_429_non_stream")
                .body(Body::from(
                    r#"{"model":"gpt-5.5","input":[],"stream":false,"previous_response_id":"resp_prev"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["id"], "resp_history_fallback_non_stream");
    let fallback_request = server.await.unwrap();
    assert_eq!(fallback_request["previous_response_id"], "resp_prev");
    let account_b_usage =
        sqlx::query_as::<_, (i64,)>("select count(*) from account_usage where account_id = ?")
            .bind("acct_b")
            .fetch_one(&imported.pool)
            .await
            .unwrap();
    assert_eq!(account_b_usage.0, 1);
}

#[tokio::test]
async fn v1_responses_websocket_without_history_should_mark_expired_after_fallback_401() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-a",
            429,
            "Too Many Requests",
            Some(30),
            WEBSOCKET_RATE_LIMITED,
        )
        .await;
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-b",
            401,
            "Unauthorized",
            None,
            WEBSOCKET_TOKEN_REVOKED,
        )
        .await
    });
    let imported = build_imported_app_with_accounts(
        format!("http://{addr}"),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;

    let response = imported
        .app
        .clone()
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
                    r#"{"model":"gpt-5.5","input":[],"stream":true,"use_websocket":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));
    let body = response_text(response).await;
    assert!(body.contains("event: response.failed"));
    assert!(body.contains("\"type\":\"invalid_request_error\""));
    assert!(body.contains("\"code\":\"authentication_error\""));
    assert!(body.contains("All accounts exhausted"));
    assert!(body.contains("token_revoked"));
    server.await.unwrap();
    let account_b_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_b")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(account_b_status.0, "expired");
    let usage_a: (i64, i64, i64) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens from account_usage where account_id = ?",
    )
    .bind("acct_a")
    .fetch_one(&imported.pool)
    .await
    .unwrap();
    assert_eq!(usage_a, (1, 0, 0));
}

#[tokio::test]
async fn v1_responses_websocket_without_history_should_return_429_when_fallback_accounts_exhausted()
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-a",
            429,
            "Too Many Requests",
            Some(11),
            WEBSOCKET_FIRST_ACCOUNT_LIMITED,
        )
        .await;
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-b",
            429,
            "Too Many Requests",
            Some(22),
            WEBSOCKET_SECOND_ACCOUNT_LIMITED,
        )
        .await;
    });
    let imported = build_imported_app_with_accounts(
        format!("http://{addr}"),
        &[
            ImportAccount {
                id: "acct_a",
                account_id: "chatgpt-a",
                token: "access-a",
                refresh_token: "refresh-a",
            },
            ImportAccount {
                id: "acct_b",
                account_id: "chatgpt-b",
                token: "access-b",
                refresh_token: "refresh-b",
            },
        ],
    )
    .await;

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
                    r#"{"model":"gpt-5.5","input":[],"stream":true,"use_websocket":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));
    let body = response_text(response).await;
    assert_response_failed_stream(
        &body,
        "rate_limit_error",
        "rate_limit_exceeded",
        &[
            "All accounts exhausted (2 rate-limited)",
            "second account limited",
        ],
    );
    server.await.unwrap();
    let usage_a: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_a")
            .fetch_one(&imported.pool)
            .await
            .unwrap();
    assert_eq!(usage_a.0, 1);
    let usage_b: (i64,) =
        sqlx::query_as("select request_count from account_usage where account_id = ?")
            .bind("acct_b")
            .fetch_one(&imported.pool)
            .await
            .unwrap();
    assert_eq!(usage_b.0, 1);
}

#[tokio::test]
async fn v1_responses_websocket_without_history_should_return_stream_error_when_402_has_no_fallback(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-a",
            402,
            "Payment Required",
            None,
            r#"{"error":{"message":"quota reached"}}"#,
        )
        .await;
    });
    let imported = build_imported_app_with_accounts(
        format!("http://{addr}"),
        &[ImportAccount {
            id: "acct_ws_402_single",
            account_id: "chatgpt-a",
            token: "access-a",
            refresh_token: "refresh-a",
        }],
    )
    .await;

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
                    r#"{"model":"gpt-5.5","input":[],"stream":true,"use_websocket":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));
    let body = response_text(response).await;
    assert_response_failed_stream(
        &body,
        "invalid_request_error",
        "codex_api_error",
        &[
            "All accounts exhausted (1 quota-exhausted)",
            "quota reached",
        ],
    );
    server.await.unwrap();
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_ws_402_single")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "quota_exhausted");
}

#[tokio::test]
async fn v1_responses_websocket_without_history_should_return_stream_error_when_model_unsupported_has_no_fallback(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        reject_next_websocket_upgrade(
            &listener,
            "Bearer access-a",
            400,
            "Bad Request",
            None,
            r#"{"error":{"code":"model_not_available","message":"Model gpt-5.5 is not available on this account plan"}}"#,
        )
        .await;
    });
    let imported = build_imported_app_with_accounts(
        format!("http://{addr}"),
        &[ImportAccount {
            id: "acct_ws_model_single",
            account_id: "chatgpt-a",
            token: "access-a",
            refresh_token: "refresh-a",
        }],
    )
    .await;

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
                    r#"{"model":"gpt-5.5","input":[],"stream":true,"use_websocket":true}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));
    let body = response_text(response).await;
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
    server.await.unwrap();
    let account_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_ws_model_single")
        .fetch_one(&imported.pool)
        .await
        .unwrap();
    assert_eq!(account_status.0, "active");
}

#[tokio::test]
async fn v1_responses_websocket_with_history_should_return_stream_error_when_path_block_has_no_fallback(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        reject_next_websocket_upgrade(&listener, "Bearer access-a", 404, "Not Found", None, "")
            .await;
    });
    let imported = build_imported_app_with_accounts(
        format!("http://{addr}"),
        &[ImportAccount {
            id: "acct_ws_path_block_single",
            account_id: "chatgpt-a",
            token: "access-a",
            refresh_token: "refresh-a",
        }],
    )
    .await;

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
                    r#"{"model":"gpt-5.5","input":[],"stream":true,"previous_response_id":"resp_prev"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/event-stream"));
    let body = response_text(response).await;
    assert_response_failed_stream(
        &body,
        "server_error",
        "codex_api_error",
        &["No accounts available", "Cloudflare path-block"],
    );
    server.await.unwrap();
}

async fn reject_next_websocket_upgrade(
    listener: &TcpListener,
    expected_authorization: &str,
    status: u16,
    reason: &str,
    retry_after_seconds: Option<u64>,
    body: &str,
) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let request = read_http_upgrade_request(&mut stream).await;
    assert!(request.starts_with("GET /codex/responses HTTP/1.1"));
    assert!(
        request.contains(&format!("Authorization: {expected_authorization}")),
        "unexpected websocket authorization header in request:\n{request}"
    );
    let retry_after = retry_after_seconds
        .map(|seconds| format!("retry-after: {seconds}\r\n"))
        .unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n{retry_after}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
}

#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn accept_successful_websocket_response(
    listener: &TcpListener,
    expected_authorization: &str,
    response_id: &str,
) -> Value {
    let (stream, _) = listener.accept().await.unwrap();
    let expected_authorization = expected_authorization.to_string();
    let mut websocket =
        accept_hdr_async(stream, move |request: &WsRequest, response: WsResponse| {
            assert_eq!(
                request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok()),
                Some(expected_authorization.as_str())
            );
            Ok(response)
        })
        .await
        .unwrap();
    let message = websocket.next().await.unwrap().unwrap();
    let request = serde_json::from_str::<Value>(&message.into_text().unwrap()).unwrap();
    websocket
        .send(Message::Text(
            websocket_completed_response(response_id, 3, 1).into(),
        ))
        .await
        .unwrap();
    websocket.close(None).await.unwrap();
    request
}

fn websocket_completed_response(
    response_id: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> String {
    let mut value: Value = serde_json::from_str(WEBSOCKET_COMPLETED_RESPONSE).unwrap();
    value["response"]["id"] = Value::String(response_id.to_string());
    value["response"]["usage"]["input_tokens"] = json!(input_tokens);
    value["response"]["usage"]["output_tokens"] = json!(output_tokens);
    value.to_string()
}

fn websocket_completed_function_call_response(response_id: &str, call_id: &str) -> String {
    let mut value: Value = serde_json::from_str(WEBSOCKET_COMPLETED_RESPONSE).unwrap();
    value["response"]["id"] = Value::String(response_id.to_string());
    value["response"]["output"] = json!([{
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": "lookup",
        "arguments": "{}"
    }]);
    value["response"]["usage"]["input_tokens"] = json!(6);
    value["response"]["usage"]["output_tokens"] = json!(1);
    value.to_string()
}

async fn read_http_upgrade_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await.unwrap();
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(request).unwrap()
}

async fn update_admin_account_status(app: &axum::Router, account_id: &str, status: &str) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/accounts/{account_id}/status"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({ "status": status }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
