use super::*;
use codex_proxy_rs::codex::{
    protocol::websocket::{OpeningAuditSnapshot, WebSocketAuditArtifact},
    transport::websocket::write_websocket_audit_artifact_for_dir,
};

#[tokio::test]
async fn websocket_audit_artifact_should_require_explicit_directory() {
    let dir = tempfile::tempdir().expect("temp dir");

    let artifact = WebSocketAuditArtifact {
        transport_mode: "websocket_required".to_string(),
        fallback_allowed: false,
        opening: Some(OpeningAuditSnapshot {
            header_order: vec!["authorization".to_string()],
            ..OpeningAuditSnapshot::default()
        }),
        payload: None,
        error: None,
    };

    let disabled = write_websocket_audit_artifact_for_dir(None, &artifact)
        .await
        .expect("disabled audit should be ok");

    assert!(disabled.is_none());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

    let written = write_websocket_audit_artifact_for_dir(Some(dir.path()), &artifact)
        .await
        .expect("enabled audit should write")
        .expect("enabled audit path");
    let body = std::fs::read_to_string(&written).expect("audit file");
    let json = serde_json::from_str::<serde_json::Value>(&body).expect("audit json");

    assert!(json["opening"]["header_order"][0].as_str().is_some());
}

#[test]
fn websocket_responses_endpoint_should_convert_http_base_url_to_ws_endpoint() {
    assert_eq!(
        responses_websocket_endpoint("https://chatgpt.com/backend-api"),
        "wss://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(
        responses_websocket_endpoint("http://127.0.0.1:8080"),
        "ws://127.0.0.1:8080/codex/responses"
    );
}

#[tokio::test]
async fn codex_backend_client_should_decode_permessage_deflate_context_takeover_frames() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket_with(stream, |request, response| {
            let extensions = request
                .headers()
                .get("sec-websocket-extensions")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            assert!(extensions.contains("permessage-deflate"));
            response.headers_mut().insert(
                "sec-websocket-extensions",
                "permessage-deflate".parse().unwrap(),
            );
        })
        .await;

        let delta = json!({
            "type": "response.output_text.delta",
            "delta": "hello from websocket"
        })
        .to_string();
        let completed = json!({
            "type": "response.completed",
            "response": {
                "id": "resp_6f8d0c2b5a4e4a0d9c1b7e3f2a8d5c6b",
                "object": "response",
                "output": [],
                "usage": {
                    "input_tokens": 3,
                    "output_tokens": 1,
                    "total_tokens": 4
                }
            }
        })
        .to_string();

        let Some(Ok(Message::Text(_payload))) = websocket.next().await else {
            panic!("client should send response.create payload");
        };

        for payload in [delta, completed] {
            websocket.send(Message::Text(payload.into())).await.unwrap();
        }
    });
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_secs(60)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::fingerprint::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(pool);
    let mut request =
        codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());

    let response = backend
        .create_response(
            &request,
            request_context("req_live_deflate", Some("chatgpt-account")),
        )
        .await
        .expect("deflated websocket response should decode");
    server.await.unwrap();

    assert!(response.body.contains("hello from websocket"));
    assert!(response
        .body
        .contains("resp_6f8d0c2b5a4e4a0d9c1b7e3f2a8d5c6b"));
}

#[test]
fn websocket_connection_should_render_raw_opening_bytes_for_capture_parity() {
    let connection =
        CodexWebSocketConnection::responses(
            "https://chatgpt.com/backend-api",
            "test-websocket-key",
            vec![
                (
                    "chatgpt-account-id".to_string(),
                    "chatgpt-account".to_string(),
                ),
                (
                    "authorization".to_string(),
                    "Bearer access-token".to_string(),
                ),
                (
                    "user-agent".to_string(),
                    "Codex Desktop/26.519.81530 (darwin; arm64)".to_string(),
                ),
                ("originator".to_string(), "Codex Desktop".to_string()),
                (
                    "openai-beta".to_string(),
                    "responses_websockets=2026-02-06".to_string(),
                ),
                (
                    "x-codex-beta-features".to_string(),
                    "terminal_resize_reflow,memories,network_proxy,prevent_idle_sleep,remote_compaction_v2".to_string(),
                ),
                ("x-client-request-id".to_string(), "session-1".to_string()),
                ("session-id".to_string(), "session-1".to_string()),
                ("thread-id".to_string(), "session-1".to_string()),
                ("x-codex-window-id".to_string(), "session-1:0".to_string()),
                (
                    "x-codex-turn-metadata".to_string(),
                    r#"{"installation_id":"install-1","session_id":"session-1","thread_id":"session-1","turn_id":"","window_id":"session-1:0","request_kind":"prewarm","sandbox":"seccomp"}"#.to_string(),
                ),
            ],
        );

    let opening = connection.opening_request_text();

    assert!(opening.starts_with("GET /backend-api/codex/responses HTTP/1.1\r\n"));
    assert_substrings_appear_in_order(
        &opening,
        &[
            "Host: chatgpt.com\r\n",
            "Connection: Upgrade\r\n",
            "Upgrade: websocket\r\n",
            "Sec-WebSocket-Version: 13\r\n",
            "Sec-WebSocket-Key: test-websocket-key\r\n",
            "chatgpt-account-id: chatgpt-account\r\n",
            "authorization: Bearer access-token\r\n",
            "user-agent: Codex Desktop/26.519.81530 (darwin; arm64)\r\n",
            "originator: Codex Desktop\r\n",
            "openai-beta: responses_websockets=2026-02-06\r\n",
            "x-codex-beta-features: terminal_resize_reflow,memories,network_proxy,prevent_idle_sleep,remote_compaction_v2\r\n",
            "x-client-request-id: session-1\r\n",
            "session-id: session-1\r\n",
            "thread-id: session-1\r\n",
            "x-codex-window-id: session-1:0\r\n",
            r#"x-codex-turn-metadata: {"installation_id":"install-1","session_id":"session-1","thread_id":"session-1","turn_id":"","window_id":"session-1:0","request_kind":"prewarm","sandbox":"seccomp"}"#,
            "sec-websocket-extensions: permessage-deflate; client_max_window_bits\r\n",
        ],
    );
    assert!(opening.ends_with("\r\n\r\n"));
}

#[test]
fn websocket_connection_should_prepare_response_create_payload_text() {
    let request = codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        vec![json!({
            "role": "user",
            "content": "hello",
        })],
    );

    let prepared = CodexWebSocketConnection::responses_create_request(
        "https://chatgpt.com/backend-api",
        "test-websocket-key",
        vec![(
            "authorization".to_string(),
            "Bearer access-token".to_string(),
        )],
        &request,
    )
    .expect("payload should serialize");
    let payload: serde_json::Value =
        serde_json::from_str(prepared.payload_text()).expect("payload should be json");

    assert_eq!(
        prepared.connection().endpoint(),
        "wss://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(payload["type"], "response.create");
    assert_eq!(payload["model"], "gpt-5.5");
    assert_eq!(payload["instructions"], "be brief");
    assert_eq!(payload["input"][0]["content"], "hello");
    assert_eq!(payload["stream"], true);
}

#[test]
fn websocket_connection_should_prepare_capture_payload_with_canonical_field_order() {
    let mut request =
        codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "private capture instructions",
            vec![json!({
                "role": "user",
                "content": "private capture prompt",
            })],
        );
    request.prompt_cache_key = Some("session-1".to_string());
    request.client_metadata = Some(json!({
        "thread_id": "capture-thread-secret",
        "safe": "capture",
    }));

    let prepared = CodexWebSocketConnection::responses_create_request(
        "https://chatgpt.com/backend-api",
        "test-websocket-key",
        vec![(
            "authorization".to_string(),
            "Bearer access-token".to_string(),
        )],
        &request,
    )
    .expect("payload should serialize");

    assert_substrings_appear_in_order(
        prepared.payload_text(),
        &[
            "\"type\":\"response.create\"",
            "\"model\":\"gpt-5.5\"",
            "\"instructions\":\"private capture instructions\"",
            "\"input\":",
            "\"store\":false",
            "\"stream\":true",
            "\"tool_choice\":\"auto\"",
            "\"parallel_tool_calls\":true",
            "\"prompt_cache_key\":\"session-1\"",
            "\"client_metadata\":",
        ],
    );
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_collect_completed_sse() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let message = websocket.next().await.unwrap().unwrap();
        let payload = serde_json::from_str::<serde_json::Value>(&message.into_text().unwrap())
            .expect("client payload should be json");
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_ws_live",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 5,
                            "output_tokens": 2,
                            "total_tokens": 7
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        payload
    });
    let request = codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared = CodexWebSocketConnection::responses_create_request(
        &format!("http://{addr}"),
        "dGhlIHNhbXBsZSBub25jZQ==",
        vec![(
            "authorization".to_string(),
            "Bearer access-token".to_string(),
        )],
        &request,
    )
    .expect("payload should serialize");

    let response = execute_response_create_request(&prepared)
        .await
        .expect("websocket exchange should succeed");
    let payload = server.await.unwrap();

    assert_eq!(payload["type"], "response.create");
    assert!(response.body.contains("event: response.completed"));
    assert!(response.body.contains("\"id\":\"resp_ws_live\""));
    assert_eq!(response.usage.expect("usage").input_tokens, 5);
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_surface_response_failed_as_upstream_error(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "error": {
                            "code": "rate_limit_exceeded",
                            "message": "Rate limit reached. Please try again in 11.054s."
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let request = codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared = CodexWebSocketConnection::responses_create_request(
        &format!("http://{addr}"),
        "dGhlIHNhbXBsZSBub25jZQ==",
        vec![(
            "authorization".to_string(),
            "Bearer access-token".to_string(),
        )],
        &request,
    )
    .expect("payload should serialize");

    let error = execute_response_create_request(&prepared)
        .await
        .expect_err("response.failed should be surfaced as upstream error");
    server.await.unwrap();

    let CodexWebSocketExchangeError::Upstream {
        status_code,
        retry_after_seconds,
        body,
        ..
    } = error
    else {
        panic!("expected upstream websocket error");
    };
    assert_eq!(status_code, 429);
    assert_eq!(retry_after_seconds, Some(12));
    assert!(body.contains("rate_limit_exceeded"));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_preserve_opening_error_status_body_and_retry_after(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        assert!(request.starts_with("GET /codex/responses HTTP/1.1"));
        assert!(request.contains("authorization: Bearer access-token"));
        let body = r#"{"error":{"message":"rate limited"}}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 429 Too Many Requests\r\nretry-after: 33\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let prepared = prepared_websocket_request(&format!("http://{addr}"));

    let error = execute_response_create_request(&prepared)
        .await
        .expect_err("failed opening should surface upstream status");
    server.await.unwrap();

    let CodexWebSocketExchangeError::Upstream {
        status_code,
        retry_after_seconds,
        body,
        ..
    } = error
    else {
        panic!("expected upstream opening error");
    };
    assert_eq!(status_code, 429);
    assert_eq!(retry_after_seconds, Some(33));
    assert!(body.contains("rate limited"));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_reject_binary_event() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Binary(b"unexpected-binary".to_vec().into()))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let prepared = prepared_websocket_request(&format!("http://{addr}"));

    let error = execute_response_create_request(&prepared)
        .await
        .expect_err("binary websocket events should be rejected");
    server.await.unwrap();

    assert!(matches!(
        error,
        CodexWebSocketExchangeError::UnexpectedBinaryEvent
    ));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_surface_wrapped_error_status_and_retry_after(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "error",
                    "status": 409,
                    "headers": {
                        "retry-after": ["17"]
                    },
                    "error": {
                        "code": "conflict",
                        "message": "wrapped conflict"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let prepared = prepared_websocket_request(&format!("http://{addr}"));

    let error = execute_response_create_request(&prepared)
        .await
        .expect_err("wrapped error should surface as upstream error");
    server.await.unwrap();

    let CodexWebSocketExchangeError::Upstream {
        status_code,
        retry_after_seconds,
        body,
        ..
    } = error
    else {
        panic!("expected wrapped upstream error");
    };
    assert_eq!(status_code, 409);
    assert_eq!(retry_after_seconds, Some(17));
    assert!(body.contains("wrapped conflict"));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_surface_connection_limit_as_503() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_connection_limit",
                        "error": {
                            "code": "websocket_connection_limit_reached",
                            "message": "connection limit reached"
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let prepared = prepared_websocket_request(&format!("http://{addr}"));

    let error = execute_response_create_request(&prepared)
        .await
        .expect_err("connection limit should surface as upstream error");
    server.await.unwrap();

    let CodexWebSocketExchangeError::Upstream {
        status_code, body, ..
    } = error
    else {
        panic!("expected connection limit upstream error");
    };
    assert_eq!(status_code, 503);
    assert!(body.contains("websocket_connection_limit_reached"));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_skip_invalid_stream_events() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_after_invalid",
                        "object": "response"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let request = codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared = CodexWebSocketConnection::responses_create_request(
        &format!("http://{addr}"),
        "dGhlIHNhbXBsZSBub25jZQ==",
        vec![(
            "authorization".to_string(),
            "Bearer access-token".to_string(),
        )],
        &request,
    )
    .expect("payload should serialize");

    let response = execute_response_create_request(&prepared)
        .await
        .expect("websocket exchange should succeed");
    server.await.unwrap();

    assert!(!response.body.contains("response.output_text.delta"));
    assert!(response.body.contains("event: response.completed"));
    assert!(response.body.contains("\"id\":\"resp_after_invalid\""));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_capture_internal_metadata_and_rate_limit_events(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "codex.rate_limits",
                    "rate_limits": {
                        "primary": {
                            "used_percent": 100,
                            "window_minutes": 5,
                            "reset_at": 1893456300
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.metadata",
                    "headers": {
                        "x-codex-turn-state": ["turn-from-metadata"]
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_internal_events",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 1,
                            "output_tokens": 1,
                            "total_tokens": 2
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let prepared = prepared_websocket_request(&format!("http://{addr}"));

    let response = execute_response_create_request(&prepared)
        .await
        .expect("internal events should update metadata without forwarding");
    server.await.unwrap();

    assert_eq!(response.turn_state.as_deref(), Some("turn-from-metadata"));
    assert!(!response.body.contains("codex.rate_limits"));
    assert!(!response.body.contains("response.metadata"));
    assert!(response.body.contains("event: response.completed"));
    assert!(response
        .rate_limit_headers
        .iter()
        .any(|(name, value)| name == "x-codex-primary-used-percent" && value == "100"));
    assert!(response
        .rate_limit_headers
        .iter()
        .any(|(name, value)| name == "x-codex-primary-reset-at" && value == "1893456300"));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_surface_incomplete_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.incomplete",
                    "response": {
                        "id": "resp_incomplete",
                        "incomplete_details": {
                            "reason": "max_output_tokens"
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let request = codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared = CodexWebSocketConnection::responses_create_request(
        &format!("http://{addr}"),
        "dGhlIHNhbXBsZSBub25jZQ==",
        vec![(
            "authorization".to_string(),
            "Bearer access-token".to_string(),
        )],
        &request,
    )
    .expect("payload should serialize");

    let error = execute_response_create_request(&prepared)
        .await
        .expect_err("response.incomplete should be surfaced as websocket error");
    server.await.unwrap();

    let CodexWebSocketExchangeError::IncompleteResponse { reason } = error else {
        panic!("expected incomplete websocket response");
    };
    assert_eq!(reason, "max_output_tokens");
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_reject_invalid_completed_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_invalid_completed",
                        "object": "response",
                        "usage": {
                            "input_tokens": "bad",
                            "output_tokens": 1,
                            "total_tokens": 1
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let request = codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared = CodexWebSocketConnection::responses_create_request(
        &format!("http://{addr}"),
        "dGhlIHNhbXBsZSBub25jZQ==",
        vec![(
            "authorization".to_string(),
            "Bearer access-token".to_string(),
        )],
        &request,
    )
    .expect("payload should serialize");

    let error = execute_response_create_request(&prepared)
        .await
        .expect_err("invalid response.completed should be rejected");
    server.await.unwrap();

    let CodexWebSocketExchangeError::InvalidCompletedResponse { message } = error else {
        panic!("expected invalid completed websocket response");
    };
    assert!(message.contains("failed to parse ResponseCompleted"));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_ignore_completed_without_response_until_close(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let request = codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared = CodexWebSocketConnection::responses_create_request(
        &format!("http://{addr}"),
        "dGhlIHNhbXBsZSBub25jZQ==",
        vec![(
            "authorization".to_string(),
            "Bearer access-token".to_string(),
        )],
        &request,
    )
    .expect("payload should serialize");

    let error = execute_response_create_request(&prepared)
        .await
        .expect_err("response.completed without response should not finish the stream");
    server.await.unwrap();

    assert!(matches!(
        error,
        CodexWebSocketExchangeError::ClosedBeforeTerminal
    ));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_ignore_success_status_error_until_close()
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "error",
                    "status": 200,
                    "error": {
                        "message": "non-terminal informational frame"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let request = codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared = CodexWebSocketConnection::responses_create_request(
        &format!("http://{addr}"),
        "dGhlIHNhbXBsZSBub25jZQ==",
        vec![(
            "authorization".to_string(),
            "Bearer access-token".to_string(),
        )],
        &request,
    )
    .expect("payload should serialize");

    let error = execute_response_create_request(&prepared)
        .await
        .expect_err("unclassified success-status error frame should not finish the stream");
    server.await.unwrap();

    assert!(matches!(
        error,
        CodexWebSocketExchangeError::ClosedBeforeTerminal
    ));
}

#[tokio::test(start_paused = true)]
async fn websocket_execute_response_create_request_should_timeout_when_upstream_is_silent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_seen_tx, request_seen_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        request_seen_tx.send(()).unwrap();
        futures::future::pending::<()>().await;
    });
    let prepared = prepared_websocket_request(&format!("http://{addr}"));

    let response_task =
        tokio::spawn(async move { execute_response_create_request(&prepared).await });
    request_seen_rx.await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(20)).await;
    tokio::task::yield_now().await;

    let error = response_task
        .await
        .expect("websocket task should finish")
        .expect_err("silent upstream should time out");
    server.abort();

    let CodexWebSocketExchangeError::ReceiveIdleTimeout { timeout } = error else {
        panic!("expected receive idle timeout");
    };
    assert_eq!(timeout, Duration::from_secs(20));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_reply_to_server_ping_before_terminal() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Ping("codex-ping".as_bytes().to_vec().into()))
            .await
            .unwrap();
        let pong = timeout(Duration::from_secs(1), websocket.next())
            .await
            .expect("client should reply to server ping before terminal response")
            .expect("client should send a websocket frame")
            .expect("client frame should be valid");
        assert_eq!(pong, Message::Pong("codex-ping".as_bytes().to_vec().into()));
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_after_ping",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 2,
                            "output_tokens": 1,
                            "total_tokens": 3
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let request = codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared = CodexWebSocketConnection::responses_create_request(
        &format!("http://{addr}"),
        "dGhlIHNhbXBsZSBub25jZQ==",
        vec![(
            "authorization".to_string(),
            "Bearer access-token".to_string(),
        )],
        &request,
    )
    .expect("payload should serialize");

    let response = execute_response_create_request(&prepared)
        .await
        .expect("websocket exchange should succeed after ping/pong");
    server.await.unwrap();

    assert!(response.body.contains("event: response.completed"));
    assert!(response.body.contains("\"id\":\"resp_after_ping\""));
    assert_eq!(response.usage.expect("usage").input_tokens, 2);
}

#[tokio::test]
async fn codex_backend_client_stream_should_reject_binary_websocket_event() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Binary(b"unexpected-binary".to_vec().into()))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let mut request =
        codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.force_http_sse = false;
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::fingerprint::Fingerprint::default_for_tests(),
    );

    let mut response = backend
        .create_response_stream(
            &request,
            request_context("req_stream_binary", Some("chatgpt-account")),
        )
        .await
        .expect("websocket stream should open");
    let error = response
        .body
        .next()
        .await
        .expect("stream should yield binary error")
        .expect_err("binary event should be rejected");
    server.await.unwrap();

    assert!(matches!(
        error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::UnexpectedBinaryEvent)
    ));
}

#[tokio::test]
async fn codex_backend_client_stream_should_error_when_websocket_closes_before_terminal() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "partial"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let mut request =
        codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.force_http_sse = false;
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::fingerprint::Fingerprint::default_for_tests(),
    );

    let mut response = backend
        .create_response_stream(
            &request,
            request_context("req_stream_mid_close", Some("chatgpt-account")),
        )
        .await
        .expect("websocket stream should open");
    let first = response
        .body
        .next()
        .await
        .expect("stream should yield partial frame")
        .expect("partial frame should be valid");
    assert!(std::str::from_utf8(&first).unwrap().contains("partial"));
    let error = response
        .body
        .next()
        .await
        .expect("stream should yield close-before-terminal error")
        .expect_err("close before terminal should be an error");
    server.await.unwrap();

    assert!(matches!(
        error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::ClosedBeforeTerminal)
    ));
}

#[tokio::test]
async fn codex_backend_client_should_use_websocket_when_previous_response_id_is_present() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket_with(stream, |_request, response| {
            response.headers_mut().insert(
                "sec-websocket-extensions",
                "permessage-deflate".parse().unwrap(),
            );
            response
                .headers_mut()
                .insert("x-codex-turn-state", "turn-ws-client".parse().unwrap());
            response.headers_mut().insert(
                "set-cookie",
                "cf_clearance=client-ws; Domain=.chatgpt.com; Path=/"
                    .parse()
                    .unwrap(),
            );
            response
                .headers_mut()
                .insert("x-ratelimit-remaining-requests", "17".parse().unwrap());
        })
        .await;
        let message = websocket.next().await.unwrap().unwrap();
        let payload = serde_json::from_str::<serde_json::Value>(&message.into_text().unwrap())
            .expect("client payload should be json");
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_ws_client",
                        "object": "response",
                        "output": [],
                        "usage": {
                            "input_tokens": 3,
                            "output_tokens": 1,
                            "total_tokens": 4
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        payload
    });
    let mut request =
        codex_proxy_rs::codex::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_secs(60)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::fingerprint::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(pool);

    let response = backend
        .create_response(
            &request,
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_ws_client",
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: None,
                installation_id: None,
                session_id: None,
            },
        )
        .await
        .expect("websocket response should succeed");
    let payload = server.await.unwrap();

    assert_eq!(payload["type"], "response.create");
    assert_eq!(payload["previous_response_id"], "resp_previous");
    assert!(response.body.contains("event: response.completed"));
    assert!(response.body.contains("\"id\":\"resp_ws_client\""));
    assert_eq!(response.usage.expect("usage").input_tokens, 3);
    assert_eq!(response.turn_state.as_deref(), Some("turn-ws-client"));
    assert_eq!(
        response.set_cookie_headers,
        vec!["cf_clearance=client-ws; Domain=.chatgpt.com; Path=/".to_string()]
    );
    assert!(response
        .rate_limit_headers
        .iter()
        .any(|(name, value)| name == "x-ratelimit-remaining-requests" && value == "17"));
}
