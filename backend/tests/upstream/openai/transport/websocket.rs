use super::*;
use codex_proxy_rs::upstream::openai::{
    protocol::websocket::{OpeningAuditSnapshot, WebSocketAuditArtifact},
    transport::websocket::write_websocket_audit_artifact_for_dir,
};

fn rate_limit_event(used_percent: u64) -> String {
    json!({
        "type": "codex.rate_limits",
        "rate_limits": {
            "primary": {
                "used_percent": used_percent,
                "window_minutes": 43200,
                "reset_at": 1893456000 + used_percent as i64,
            }
        }
    })
    .to_string()
}

fn primary_used_percent_values(headers: &[(String, String)]) -> Vec<&str> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            (name == "x-codex-primary-used-percent").then_some(value.as_str())
        })
        .collect()
}

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
    let file_name = written
        .file_name()
        .and_then(|value| value.to_str())
        .expect("audit file name");
    assert!(
        file_name.contains("+0800"),
        "expected China-time audit file name, got {file_name}"
    );
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
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(pool);
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);

    let response = backend
        .create_response(
            &request,
            request_context("req_live_deflate", Some("chatgpt-account")),
        )
        .await
        .expect("deflated websocket response should decode");
    server.await.unwrap();

    assert!(response.body.contains("hello from websocket"));
    assert!(
        response
            .body
            .contains("resp_6f8d0c2b5a4e4a0d9c1b7e3f2a8d5c6b")
    );
}

#[test]
fn websocket_connection_should_prepare_response_create_payload_text() {
    let request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
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
    assert!(payload.get("stream").is_none());
    assert!(payload.get("store").is_none());
}

#[test]
fn websocket_connection_should_prepare_capture_payload_with_canonical_field_order() {
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "private capture instructions",
            vec![json!({
                "role": "user",
                "content": "private capture prompt",
            })],
        );
    request.set_prompt_cache_key(Some("session-1".to_string()));
    request.set_client_metadata(Some(json!({
        "thread_id": "capture-thread-secret",
        "safe": "capture",
    })));

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
    let request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
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
async fn websocket_execute_response_create_request_should_surface_response_failed_as_upstream_error()
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
    let request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
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

    let CodexWebSocketExchangeError::Upstream(error) = error else {
        panic!("expected upstream websocket error");
    };
    assert_eq!(error.status_code, 429);
    assert_eq!(error.retry_after_seconds, Some(12));
    assert!(error.body.contains("rate_limit_exceeded"));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_pass_through_unmapped_response_failed() {
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
                        "id": "resp_model_refusal",
                        "status": "failed",
                        "error": {
                            "code": "model_refusal",
                            "message": "The model refused the request"
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
        .expect("unmapped response.failed should remain a terminal SSE frame");
    server.await.unwrap();

    assert!(response.body.contains("event: response.failed"));
    assert!(response.body.contains("\"id\":\"resp_model_refusal\""));
    assert!(response.body.contains("\"code\":\"model_refusal\""));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_preserve_opening_error_status_body_and_retry_after()
 {
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

    let CodexWebSocketExchangeError::Upstream(error) = error else {
        panic!("expected upstream opening error");
    };
    assert_eq!(error.status_code, 429);
    assert_eq!(error.retry_after_seconds, Some(33));
    assert!(error.body.contains("rate limited"));
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

    std::assert_matches!(error, CodexWebSocketExchangeError::UnexpectedBinaryEvent);
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_surface_wrapped_error_status_and_retry_after()
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

    let CodexWebSocketExchangeError::Upstream(error) = error else {
        panic!("expected wrapped upstream error");
    };
    assert_eq!(error.status_code, 409);
    assert_eq!(error.retry_after_seconds, Some(17));
    assert!(error.body.contains("wrapped conflict"));
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

    let CodexWebSocketExchangeError::Upstream(error) = error else {
        panic!("expected connection limit upstream error");
    };
    assert_eq!(error.status_code, 503);
    assert!(error.body.contains("websocket_connection_limit_reached"));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_forward_typed_events_without_filtering() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        // 透明代理：缺官方必需字段的 delta 事件不再被丢弃，原样转发。
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
    let request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
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

    assert!(response.body.contains("event: response.output_text.delta"));
    assert!(response.body.contains("event: response.completed"));
    assert!(response.body.contains("\"id\":\"resp_after_invalid\""));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_capture_internal_metadata_and_rate_limit_events()
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
    assert!(
        response
            .rate_limit_headers
            .iter()
            .any(|(name, value)| name == "x-codex-primary-used-percent" && value == "100")
    );
    assert!(
        response
            .rate_limit_headers
            .iter()
            .any(|(name, value)| name == "x-codex-primary-reset-at" && value == "1893456300")
    );
}

#[tokio::test]
async fn codex_backend_client_should_not_reuse_websocket_rate_limit_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;

        let _first_message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(rate_limit_event(10).into()))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_rate_limit_first", 3, 1).into(),
            ))
            .await
            .unwrap();

        let _second_message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(rate_limit_event(20).into()))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_rate_limit_second", 5, 2).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_pool(pool);
    let request = pooled_websocket_request("conversation-rate-limit-reuse");

    let first = backend
        .create_response(
            &request,
            request_context("req_rate_limit_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    let second = backend
        .create_response(
            &request,
            request_context("req_rate_limit_second", Some("chatgpt-account")),
        )
        .await
        .expect("second pooled websocket response should reuse connection");
    server.await.unwrap();

    assert_eq!(
        primary_used_percent_values(&first.rate_limit_headers),
        vec!["10"]
    );
    assert_eq!(
        primary_used_percent_values(&second.rate_limit_headers),
        vec!["20"]
    );
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_forward_incomplete_response() {
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
    let request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
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

    let exchange = execute_response_create_request(&prepared)
        .await
        .expect("response.incomplete should remain a terminal Responses event");
    server.await.unwrap();

    assert!(exchange.body.contains("event: response.incomplete"));
    assert!(exchange.body.contains("max_output_tokens"));
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
    let request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
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
async fn websocket_execute_response_create_request_should_reject_completed_without_response() {
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
    let request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
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
        .expect_err("completed frame without response must be rejected");
    server.await.unwrap();

    std::assert_matches!(
        error,
        CodexWebSocketExchangeError::InvalidCompletedResponse { .. }
    );
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_pass_through_unclassified_error_terminal()
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
                        "code": "invalid_request",
                        "message": "No tool output found for function call call_missing"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
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
        .expect("unclassified error frame should pass through as terminal SSE");
    server.await.unwrap();

    assert!(response.body.contains("event: error"));
    assert!(response.body.contains("invalid_request"));
    assert!(response.body.contains("No tool output found"));
}

#[tokio::test]
async fn codex_backend_client_should_timeout_when_upstream_is_silent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut websockets = Vec::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = accept_codex_test_websocket(stream).await;
            let _message = websocket.next().await.unwrap().unwrap();
            websockets.push(websocket);
        }
        futures::future::pending::<()>().await;
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_initial_event_timeout(Some(Duration::from_millis(30)));
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);

    let error = backend
        .create_response(
            &request,
            request_context("req_silent_websocket", Some("chatgpt-account")),
        )
        .await
        .expect_err("silent upstream should time out");
    server.abort();

    std::assert_matches!(
        error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::InitialEventTimeout { timeout })
            if timeout == Duration::from_millis(30)
    );
}

#[tokio::test(start_paused = true)]
async fn websocket_response_created_should_switch_to_active_stream_idle_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (created_tx, created_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.created",
                    "response": {"id": "resp_structural", "status": "in_progress"}
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        created_tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_structural", 3, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let prepared = prepared_websocket_request(&format!("http://{addr}"));
    let response_task =
        tokio::spawn(async move { execute_response_create_request(&prepared).await });

    created_rx.await.unwrap();
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::task::yield_now().await;
    let response = response_task
        .await
        .expect("websocket task should finish")
        .expect("structural activity should disable the initial timeout");
    server.await.unwrap();

    assert!(response.body.contains("event: response.created"));
    assert!(response.body.contains("resp_structural"));
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
    let request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
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
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.force_http_sse = false;
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    );

    let mut response = backend
        .create_response_stream(
            &request,
            request_context("req_stream_binary", Some("chatgpt-account")),
        )
        .await
        .expect("stream should open before consuming websocket frames");
    let error = response
        .body
        .next()
        .await
        .expect("binary frame should produce a stream item")
        .expect_err("binary frame should fail the stream");
    server.await.unwrap();

    std::assert_matches!(
        error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::UnexpectedBinaryEvent)
    );
}

#[tokio::test]
async fn codex_backend_client_stream_should_keep_socket_after_structural_activity() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_codex_test_websocket(first_stream).await;
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.created",
                    "response": {
                        "id": "resp_no_pool_first_token_stalled",
                        "object": "response"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "delayed output"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        first_websocket
            .send(Message::Text(
                completed_websocket_response("resp_no_pool_delayed", 3, 1).into(),
            ))
            .await
            .unwrap();
        first_websocket.close(None).await.unwrap();
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    )
    .with_websocket_initial_event_timeout(Some(Duration::from_millis(30)));
    let request = pooled_websocket_request("conversation-structural-no-pool");

    let response = backend
        .create_response_stream(
            &request,
            request_context("req_structural_no_pool", Some("chatgpt-account")),
        )
        .await
        .expect("structural activity should keep the websocket stream open");
    let decision = response.websocket_pool_decision;
    let mut stream = response.body;
    let mut body = String::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("delayed websocket stream chunk should be valid");
        body.push_str(std::str::from_utf8(&chunk).unwrap());
    }
    server.await.unwrap();

    assert!(body.contains("resp_no_pool_first_token_stalled"));
    assert!(body.contains("delayed output"));
    assert!(body.contains("resp_no_pool_delayed"));
    assert!(decision.is_none());
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 1);
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
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.force_http_sse = false;
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
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

    std::assert_matches!(
        error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::ClosedBeforeTerminal)
    );
}

#[tokio::test]
async fn codex_backend_client_stream_should_preserve_burst_during_downstream_backpressure() {
    const BURST_FRAMES: usize = 256;

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
                    "delta": "initial;"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        for index in 0..BURST_FRAMES {
            let frame = json!({
                "type": "response.output_text.delta",
                "delta": format!("chunk-{index};")
            })
            .to_string();
            if websocket.send(Message::Text(frame.into())).await.is_err() {
                return;
            }
        }
        let _ = websocket
            .send(Message::Text(
                completed_websocket_response("resp_backpressure", 3, 257).into(),
            ))
            .await;
        let _ = websocket.close(None).await;
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    );
    let request = pooled_websocket_request("conversation-backpressure");

    let mut response = backend
        .create_response_stream(
            &request,
            request_context("req_stream_backpressure", Some("chatgpt-account")),
        )
        .await
        .expect("websocket stream should open after the first output frame");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let body = timeout(Duration::from_secs(5), async {
        let mut body = String::new();
        while let Some(chunk) = response.body.next().await {
            let chunk = chunk.expect("backpressured websocket frame should remain valid");
            body.push_str(std::str::from_utf8(&chunk).unwrap());
        }
        body
    })
    .await
    .expect("backpressured websocket stream should finish");
    server.await.unwrap();

    assert_eq!(
        body.matches("event: response.output_text.delta").count(),
        BURST_FRAMES + 1
    );
    assert!(body.contains("initial;"));
    assert!(body.contains("chunk-0;"));
    assert!(body.contains("chunk-255;"));
    assert!(body.contains("resp_backpressure"));
}

#[tokio::test]
async fn codex_backend_client_stream_should_cancel_while_inbound_is_backpressured() {
    const BURST_FRAMES: usize = 256;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (burst_sent_tx, burst_sent_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(stream).await;
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "initial;"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        for index in 0..BURST_FRAMES {
            websocket
                .send(Message::Text(
                    json!({
                        "type": "response.output_text.delta",
                        "delta": format!("chunk-{index};")
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        }
        burst_sent_tx.send(()).unwrap();

        timeout(Duration::from_secs(1), async {
            loop {
                match websocket.next().await {
                    Some(Ok(Message::Close(_))) => return,
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => panic!("websocket close should be graceful: {error}"),
                    None => panic!("websocket ended without a close frame"),
                }
            }
        })
        .await
        .expect("client cancellation should reach a backpressured pump");
    });
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    );
    let request = pooled_websocket_request("conversation-backpressure-cancel");

    let response = backend
        .create_response_stream(
            &request,
            request_context("req_stream_backpressure_cancel", Some("chatgpt-account")),
        )
        .await
        .expect("websocket stream should open after the first output frame");
    burst_sent_rx
        .await
        .expect("server should finish the inbound burst");
    tokio::time::sleep(Duration::from_millis(100)).await;

    drop(response.body);
    server.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn codex_backend_client_stream_should_wait_for_terminal_after_active_websocket_gap() {
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
        tokio::time::sleep(Duration::from_secs(30)).await;
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_after_active_gap", 3, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.force_http_sse = false;
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    );

    let mut response = backend
        .create_response_stream(
            &request,
            request_context("req_stream_active_gap", Some("chatgpt-account")),
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
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::task::yield_now().await;
    let terminal = response
        .body
        .next()
        .await
        .expect("stream should yield terminal frame after active gap")
        .expect("terminal frame should be valid");
    server.await.unwrap();

    assert!(
        std::str::from_utf8(&terminal)
            .unwrap()
            .contains("resp_after_active_gap")
    );
}

#[tokio::test(start_paused = true)]
async fn codex_backend_client_stream_should_timeout_when_active_websocket_stalls() {
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
        futures::future::pending::<()>().await;
    });
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.force_http_sse = false;
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
    );

    let mut response = backend
        .create_response_stream(
            &request,
            request_context("req_stream_active_stall", Some("chatgpt-account")),
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
    tokio::time::advance(Duration::from_secs(5 * 60)).await;
    tokio::task::yield_now().await;
    let error = response
        .body
        .next()
        .await
        .expect("stream should yield idle timeout after active stall")
        .expect_err("active stall should time out");
    server.abort();

    std::assert_matches!(
        error,
        CodexClientError::WebSocket(CodexWebSocketExchangeError::ReceiveIdleTimeout { timeout })
            if timeout == Duration::from_secs(5 * 60)
    );
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
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    let pool = Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1)));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{addr}"),
        crate::support::fingerprint::runtime_test_fingerprint(),
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
                thread_id: None,
                prompt_cache_key: None,
                client_request_id: None,
                turn_id: None,
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
    assert!(
        response
            .rate_limit_headers
            .iter()
            .any(|(name, value)| name == "x-ratelimit-remaining-requests" && value == "17")
    );
}
