use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use flate2::{Compress, Compression, FlushCompress};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{oneshot, Mutex},
    time::sleep,
};
use tokio_tungstenite::{
    accept_async, accept_hdr_async,
    tungstenite::{
        handshake::derive_accept_key,
        handshake::server::{Request as WsRequest, Response as WsResponse},
        Message,
    },
};

use codex_proxy_rs::codex::gateway::transport::{
    http_client::{
        build_reqwest_client, CodexBackendClient, CodexClientError, CodexRequestContext,
    },
    types::CodexResponsesRequest,
    websocket::{
        http_sse_fallback_allowed, transport_for_request, websocket_opening_audit_snapshot,
        websocket_parity_diff, websocket_payload_audit_snapshot,
        write_websocket_audit_artifact_for_dir, CodexTransport, CodexWebSocketError,
        CodexWebSocketPool, CodexWebSocketPoolConfig, OpeningAuditHeader, OpeningAuditSnapshot,
        PayloadAuditSnapshot, WebSocketAuditArtifact, WebSocketAuditErrorSnapshot,
    },
};

mod pool;

const WS_COMPLETED_SSE_GOLDEN: &str =
    include_str!("../fixtures/responses/golden/websocket_completed.sse");

#[test]
fn transport_for_request_should_default_to_websocket_without_history() {
    let request = base_request();

    assert_eq!(
        transport_for_request(&request),
        CodexTransport::WebSocketPreferred
    );
    assert!(http_sse_fallback_allowed(&request));
}

#[test]
fn transport_for_request_should_require_websocket_without_fallback_for_previous_response_id() {
    let mut request = base_request();
    request.previous_response_id = Some("resp_123".to_string());

    assert_eq!(
        transport_for_request(&request),
        CodexTransport::WebSocketRequired
    );
    assert!(!http_sse_fallback_allowed(&request));
}

#[test]
fn transport_for_request_should_allow_forced_http_sse() {
    let mut request = base_request();
    request.force_http_sse = true;

    assert_eq!(transport_for_request(&request), CodexTransport::HttpSse);
    assert!(http_sse_fallback_allowed(&request));
}

#[test]
fn transport_for_request_should_prefer_websocket_with_fallback_for_explicit_websocket_without_history(
) {
    let mut request = base_request();
    request.use_websocket = true;

    assert_eq!(
        transport_for_request(&request),
        CodexTransport::WebSocketPreferred
    );
    assert!(http_sse_fallback_allowed(&request));
}

#[test]
fn use_websocket_should_not_serialize_to_upstream_json() {
    let mut request = base_request();
    request.use_websocket = true;

    let body = serde_json::to_value(&request).unwrap();

    assert!(body.get("use_websocket").is_none());
    assert!(body.get("useWebSocket").is_none());
}

#[test]
fn websocket_opening_audit_snapshot_should_redact_sensitive_headers() {
    let request = tokio_tungstenite::tungstenite::http::Request::builder()
        .uri("ws://example.test:8080/codex/responses?source=audit")
        .header("Sec-WebSocket-Key", "test-websocket-key")
        .header("Authorization", "Bearer access-token")
        .header("ChatGPT-Account-Id", "acct-secret")
        .header("User-Agent", "Codex Desktop/26.609.71450")
        .header("originator", "Codex Desktop")
        .header("OpenAI-Beta", "responses_websockets=2026-02-06")
        .header(
            "x-codex-beta-features",
            "terminal_resize_reflow,memories,network_proxy,prevent_idle_sleep,remote_compaction_v2",
        )
        .header("x-client-request-id", "req_secret")
        .header("session_id", "thread-secret")
        .header("x-codex-window-id", "window-secret")
        .header("x-codex-turn-metadata", r#"{"request_kind":"prewarm"}"#)
        .body(())
        .unwrap();

    let snapshot = websocket_opening_audit_snapshot(&request).unwrap();

    assert_eq!(
        snapshot.request_line,
        "GET /codex/responses?source=audit HTTP/1.1"
    );
    assert_eq!(
        snapshot
            .headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("Host", "example.test:8080"),
            ("Connection", "Upgrade"),
            ("Upgrade", "websocket"),
            ("Sec-WebSocket-Version", "13"),
            ("Sec-WebSocket-Key", "test-websocket-key"),
            ("chatgpt-account-id", "<redacted>"),
            ("authorization", "<redacted>"),
            ("user-agent", "Codex Desktop/26.609.71450"),
            ("originator", "Codex Desktop"),
            ("openai-beta", "responses_websockets=2026-02-06"),
            (
                "x-codex-beta-features",
                "terminal_resize_reflow,memories,network_proxy,prevent_idle_sleep,remote_compaction_v2",
            ),
            ("x-client-request-id", "<redacted>"),
            ("session-id", "<redacted>"),
            ("thread-id", "<redacted>"),
            ("x-codex-window-id", "<redacted>"),
            ("x-codex-turn-metadata", "<redacted>"),
            (
                "sec-websocket-extensions",
                "permessage-deflate; client_max_window_bits"
            ),
        ]
    );
}

#[test]
fn websocket_payload_audit_snapshot_should_redact_user_content() {
    let mut request = CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "private instructions",
        vec![json!({
            "role": "user",
            "content": "private prompt",
        })],
    );
    request.previous_response_id = Some("resp_secret".to_string());
    request.reasoning = Some(json!({"effort": "medium"}));
    request.tools = Some(vec![json!({
        "type": "function",
        "name": "private_tool",
        "description": "private schema",
    })]);
    request.text = Some(json!({"format": {"type": "text"}}));
    request.service_tier = Some("flex".to_string());
    request.prompt_cache_key = Some("cache-secret".to_string());
    request.generate = Some(false);
    request.include = Some(vec!["reasoning.encrypted_content".to_string()]);
    request.client_metadata = Some(json!({
        "thread_id": "thread-secret",
        "safe": "value",
    }));

    let snapshot = websocket_payload_audit_snapshot(&request);

    assert_eq!(
        snapshot.top_level_keys,
        vec![
            "type",
            "model",
            "instructions",
            "previous_response_id",
            "input",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "reasoning",
            "store",
            "stream",
            "include",
            "service_tier",
            "prompt_cache_key",
            "text",
            "generate",
            "client_metadata",
        ]
    );
    assert_eq!(snapshot.body["type"], "response.create");
    assert_eq!(snapshot.body["model"], "gpt-5.5");
    assert_eq!(snapshot.body["stream"], true);
    assert_eq!(snapshot.body["instructions"], "<redacted>");
    assert_eq!(snapshot.body["input"], "<redacted>");
    assert_eq!(snapshot.body["previous_response_id"], "<redacted>");
    assert_eq!(snapshot.body["prompt_cache_key"], "<redacted>");
    assert_eq!(snapshot.body["client_metadata"], "<redacted>");
    assert_eq!(snapshot.body["tools"], "<redacted>");
}

#[test]
fn websocket_parity_diff_should_report_header_order_changes() {
    let current = audit_artifact_with_header_names(&["Host", "Connection", "Authorization"]);
    let reference = audit_artifact_with_header_names(&["Host", "Authorization", "Connection"]);

    let diff = websocket_parity_diff(&current, &reference);

    assert!(diff.differences.iter().any(|difference| {
        difference.path == "opening.header_order"
            && difference.current == json!(["Host", "Connection", "Authorization"])
            && difference.reference == json!(["Host", "Authorization", "Connection"])
    }));
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn previous_response_id_should_use_websocket_transport() {
    let received_headers = Arc::new(Mutex::new(None));
    let received_request = Arc::new(Mutex::new(None));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let headers_for_task = Arc::clone(&received_headers);
    let request_for_task = Arc::clone(&received_request);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket =
            accept_hdr_async(stream, |request: &WsRequest, response: WsResponse| {
                let mut headers = Vec::new();
                for (name, value) in request.headers() {
                    let value = value.to_str().unwrap_or_default().to_string();
                    headers.push((name.as_str().to_string(), value));
                }
                let headers_for_callback = Arc::clone(&headers_for_task);
                tokio::spawn(async move {
                    *headers_for_callback.lock().await = Some(headers);
                });
                Ok(response)
            })
            .await
            .unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let text = message.into_text().unwrap();
        *request_for_task.lock().await = Some(serde_json::from_str::<Value>(&text).unwrap());
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_ws",
                        "object": "response",
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

    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "be brief", Vec::new());
    request.previous_response_id = Some("resp_prev".to_string());
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &request,
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_ws",
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
        .unwrap();

    server.await.unwrap();
    let request = received_request.lock().await.clone().unwrap();
    assert_eq!(request["type"], "response.create");
    assert_eq!(request["model"], "gpt-5.5");
    assert_eq!(request["instructions"], "be brief");
    assert_eq!(request["previous_response_id"], "resp_prev");
    assert_eq!(request["stream"], true);
    assert!(response.body.contains("event: response.completed"));
    assert!(response.body.contains("\"id\":\"resp_ws\""));
    assert_eq!(response.usage.unwrap().input_tokens, 2);
    let headers = received_headers.lock().await.clone().unwrap();
    assert!(headers
        .iter()
        .any(|(name, value)| { name == "authorization" && value == "Bearer access-token" }));
}

#[tokio::test]
async fn websocket_audit_artifact_should_require_explicit_gate() {
    let request = tokio_tungstenite::tungstenite::http::Request::builder()
        .uri("ws://example.test/codex/responses")
        .header("Sec-WebSocket-Key", "test-websocket-key")
        .header("Authorization", "Bearer access-token")
        .header("ChatGPT-Account-Id", "chatgpt-account")
        .header("Cookie", "session=secret")
        .body(())
        .unwrap();
    let mut payload_request = CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "private instructions",
        vec![json!({"role": "user", "content": "private prompt"})],
    );
    payload_request.prompt_cache_key = Some("cache-secret".to_string());
    payload_request.client_metadata = Some(json!({"thread_id": "thread-secret"}));
    let artifact = WebSocketAuditArtifact {
        transport_mode: "websocket_preferred".to_string(),
        fallback_allowed: true,
        opening: Some(websocket_opening_audit_snapshot(&request).unwrap()),
        payload: Some(websocket_payload_audit_snapshot(&payload_request)),
        error: Some(WebSocketAuditErrorSnapshot {
            classification: "opening_failed".to_string(),
            message: "connection refused".to_string(),
        }),
    };
    let dir = tempfile::tempdir().unwrap();

    let disabled = write_websocket_audit_artifact_for_dir(None, &artifact)
        .await
        .unwrap();

    assert!(disabled.is_none());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

    let written = write_websocket_audit_artifact_for_dir(Some(dir.path()), &artifact)
        .await
        .unwrap()
        .unwrap();
    let body = tokio::fs::read_to_string(written).await.unwrap();
    assert!(body.contains("websocket_preferred"));
    assert!(body.contains("response.create"));
    assert!(!body.contains("access-token"));
    assert!(!body.contains("session=secret"));
    assert!(!body.contains("acct-secret"));
    assert!(!body.contains("private prompt"));
    assert!(!body.contains("private instructions"));
    assert!(!body.contains("cache-secret"));
    assert!(!body.contains("thread-secret"));
}

#[tokio::test]
async fn websocket_capture_harness_should_record_opening_bytes() {
    let artifact_path = PathBuf::from(".codex-ws-audit/rs-current-capture.json");
    let _ = std::fs::remove_file(&artifact_path);
    let mut request = CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "private capture instructions",
        vec![json!({
            "role": "user",
            "content": "private capture prompt",
        })],
    );
    request.prompt_cache_key = Some("capture-cache-secret".to_string());
    request.client_metadata = Some(json!({
        "thread_id": "capture-thread-secret",
        "safe": "capture",
    }));
    request.generate = Some(false);

    let context = CodexRequestContext {
        access_token: "access-token",
        account_id: Some("chatgpt-account"),
        request_id: "req_ws_capture",
        turn_state: None,
        turn_metadata: Some(
            r#"{"installation_id":"install-1","session_id":"session-1","thread_id":"session-1","turn_id":"","window_id":"session-1:0","request_kind":"prewarm","sandbox":"seccomp"}"#,
        ),
        beta_features: Some(
            "terminal_resize_reflow,memories,network_proxy,prevent_idle_sleep,remote_compaction_v2",
        ),
        include_timing_metrics: None,
        version: None,
        codex_window_id: Some("session-1:0"),
        parent_thread_id: None,
        cookie_header: None,
        installation_id: Some("install-1"),
        session_id: Some("session-1"),
    };

    let capture = capture_codex_websocket_exchange(&request, context, &artifact_path).await;

    assert!(capture
        .opening_bytes
        .starts_with("GET /codex/responses HTTP/1.1\r\n"));
    assert_headers_appear_in_order(
        &capture.opening_bytes,
        &[
            "Host: ",
            "Connection: Upgrade\r\n",
            "Upgrade: websocket\r\n",
            "Sec-WebSocket-Version: 13\r\n",
            "Sec-WebSocket-Key: ",
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
            "x-codex-turn-metadata: {\"installation_id\":\"install-1\",\"session_id\":\"session-1\",\"thread_id\":\"session-1\",\"turn_id\":\"\",\"window_id\":\"session-1:0\",\"request_kind\":\"prewarm\",\"sandbox\":\"seccomp\"}\r\n",
            "sec-websocket-extensions: permessage-deflate; client_max_window_bits\r\n",
        ],
    );
    assert!(
        capture.sec_websocket_key_offset < capture.authorization_header_offset,
        "Sec-WebSocket-Key should appear before business headers in:\n{}",
        capture.opening_bytes
    );
    let payload = serde_json::from_str::<Value>(&capture.first_frame_text).unwrap();
    assert_eq!(payload["type"], "response.create");
    assert_eq!(payload["instructions"], "private capture instructions");
    assert_eq!(payload["input"][0]["content"], "private capture prompt");
    assert_eq!(payload["tools"], json!([]));
    assert_eq!(payload["reasoning"], Value::Null);
    assert_eq!(payload["include"], json!([]));
    assert_eq!(payload["generate"], false);
    assert_substrings_appear_in_order(
        &capture.first_frame_text,
        &[
            "\"type\":\"response.create\"",
            "\"model\":\"gpt-5.5\"",
            "\"instructions\":\"private capture instructions\"",
            "\"input\":",
            "\"tools\":[]",
            "\"tool_choice\":\"auto\"",
            "\"parallel_tool_calls\":true",
            "\"reasoning\":null",
            "\"store\":false",
            "\"stream\":true",
            "\"include\":[]",
            "\"prompt_cache_key\":\"session-1\"",
            "\"generate\":false",
            "\"client_metadata\":",
        ],
    );

    let artifact = tokio::fs::read_to_string(&artifact_path).await.unwrap();
    assert!(artifact.contains("websocket_preferred"));
    assert!(artifact.contains("response.create"));
    assert!(!artifact.contains("private capture instructions"));
    assert!(!artifact.contains("private capture prompt"));
    assert!(!artifact.contains("capture-cache-secret"));
    assert!(!artifact.contains("capture-thread-secret"));
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn ordinary_response_should_use_websocket_transport_by_default() {
    let received_request = Arc::new(Mutex::new(None));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let request_for_task = Arc::clone(&received_request);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket =
            accept_hdr_async(stream, |_request: &WsRequest, response: WsResponse| {
                Ok(response)
            })
            .await
            .unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let text = message.into_text().unwrap();
        *request_for_task.lock().await = Some(serde_json::from_str::<Value>(&text).unwrap());
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_default", 4, 2).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let request = base_request();
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &request,
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_ws_default",
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
        .unwrap();

    server.await.unwrap();
    let request = received_request.lock().await.clone().unwrap();
    assert_eq!(request["type"], "response.create");
    assert_eq!(request["model"], "gpt-5.5");
    assert!(request.get("previous_response_id").is_none());
    assert_eq!(
        response.body,
        with_sse_terminal_separator(WS_COMPLETED_SSE_GOLDEN)
    );
    assert!(response.body.contains("event: response.completed"));
    assert!(response.body.contains("\"id\":\"resp_ws_default\""));
    assert_eq!(response.usage.unwrap().input_tokens, 4);
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_request_should_forward_security_chain_body_fields() {
    let received_headers = Arc::new(Mutex::new(None));
    let received_request = Arc::new(Mutex::new(None));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let headers_for_task = Arc::clone(&received_headers);
    let request_for_task = Arc::clone(&received_request);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket =
            accept_hdr_async(stream, |request: &WsRequest, response: WsResponse| {
                let mut headers = Vec::new();
                for (name, value) in request.headers() {
                    let value = value.to_str().unwrap_or_default().to_string();
                    headers.push((name.as_str().to_string(), value));
                }
                let headers_for_callback = Arc::clone(&headers_for_task);
                tokio::spawn(async move {
                    *headers_for_callback.lock().await = Some(headers);
                });
                Ok(response)
            })
            .await
            .unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let text = message.into_text().unwrap();
        *request_for_task.lock().await = Some(serde_json::from_str::<Value>(&text).unwrap());
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_ws_security",
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

    let mut request = base_request();
    request.use_websocket = true;
    request.prompt_cache_key = Some("client-thread".to_string());
    request.client_metadata = Some(json!({
        "safe": "yes",
        "x-openai-subagent": "review"
    }));
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    client
        .create_response(
            &request,
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_ws_security",
                turn_state: Some("turn-state"),
                turn_metadata: Some("{\"thread_source\":\"subagent\"}"),
                beta_features: Some("feature-a"),
                include_timing_metrics: Some("true"),
                version: Some("26.318.11754"),
                codex_window_id: Some("cw_derived"),
                parent_thread_id: Some("parent-456"),
                cookie_header: None,
                installation_id: Some("install-123"),
                session_id: Some("cp_derived"),
            },
        )
        .await
        .unwrap();

    server.await.unwrap();
    let request = received_request.lock().await.clone().unwrap();
    assert_eq!(request["prompt_cache_key"], "cp_derived");
    assert_eq!(
        request["client_metadata"],
        json!({
            "safe": "yes",
            "x-openai-subagent": "review",
            "x-codex-installation-id": "install-123",
            "x-codex-window-id": "cw_derived",
            "x-codex-turn-metadata": "{\"thread_source\":\"subagent\"}",
            "x-codex-parent-thread-id": "parent-456"
        })
    );
    let headers = received_headers.lock().await.clone().unwrap();
    assert!(headers
        .iter()
        .any(|(name, value)| { name == "x-client-request-id" && value == "cp_derived" }));
    assert!(headers
        .iter()
        .any(|(name, value)| { name == "x-openai-subagent" && value == "review" }));
    assert!(headers.iter().all(|(name, _)| name != "content-type"));
    assert!(headers.iter().all(|(name, _)| name != "accept"));
}

#[tokio::test]
async fn websocket_handshake_429_should_surface_as_upstream_error_before_body_is_sent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_upgrade_request(&mut stream).await;
        assert!(request.starts_with("GET /codex/responses HTTP/1.1"));
        assert!(request.contains("authorization: Bearer access-token"));
        let body = r#"{"error":{"message":"rate limited"}}"#;
        let response = format!(
            "HTTP/1.1 429 Too Many Requests\r\nretry-after: 33\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    let mut request = base_request();
    request.use_websocket = true;
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &request,
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_ws_429",
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
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::Upstream {
        status,
        retry_after_seconds,
        body,
    } = error
    else {
        panic!("expected upstream error, found {error:?}");
    };
    assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after_seconds, Some(33));
    assert!(body.contains("rate limited"));
}

#[tokio::test]
async fn websocket_handshake_should_offer_original_permessage_deflate_extension() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_upgrade_request(&mut stream).await;
        request_tx.send(request).unwrap();
        stream
            .write_all(
                b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );
    let result = client
        .websocket_stream_response(&base_request(), request_context("req_ws_extensions", None))
        .await;
    assert!(result.is_err());

    server.await.unwrap();
    let raw_request = request_rx.await.unwrap();
    assert_headers_appear_in_order(
        &raw_request,
        &[
            "Host: ",
            "Connection: Upgrade\r\n",
            "Upgrade: websocket\r\n",
            "Sec-WebSocket-Version: 13\r\n",
            "Sec-WebSocket-Key: ",
            "chatgpt-account-id: chatgpt-account\r\n",
            "authorization: Bearer access-token\r\n",
            "user-agent: Codex Desktop/26.519.81530 (darwin; arm64)\r\n",
            "originator: Codex Desktop\r\n",
            "openai-beta: responses_websockets=2026-02-06\r\n",
            "x-client-request-id: req_ws_extensions\r\n",
            "sec-websocket-extensions: permessage-deflate; client_max_window_bits\r\n",
        ],
    );
}

#[tokio::test]
async fn websocket_should_decode_permessage_deflate_response_frame_when_server_accepts_extension() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_upgrade_request(&mut stream).await;
        let accept_key = websocket_accept_key(&request);
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Accept: {accept_key}\r\n\
             Sec-WebSocket-Extensions: permessage-deflate\r\n\
             \r\n"
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        let _client_request = read_client_websocket_frame(&mut stream).await;
        let payload = websocket_completed_response("resp_ws_deflate", 5, 2);
        let compressed_frame = compressed_server_text_frame(&payload);
        stream.write_all(&compressed_frame).await.unwrap();
        sleep(Duration::from_millis(50)).await;
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );
    let mut request = base_request();
    request.previous_response_id = Some("resp_deflate_prev".to_string());

    let response = client
        .create_response(&request, request_context("req_ws_deflate", None))
        .await
        .unwrap();

    server.await.unwrap();
    assert!(response.body.contains("event: response.completed"));
    assert!(response.body.contains("\"id\":\"resp_ws_deflate\""));
    assert_eq!(response.usage.unwrap().input_tokens, 5);
}

#[tokio::test]
async fn websocket_should_reply_to_server_ping_before_completed_event() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_upgrade_request(&mut stream).await;
        let accept_key = websocket_accept_key(&request);
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Accept: {accept_key}\r\n\
             \r\n"
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        let _client_request = read_client_websocket_frame(&mut stream).await;

        stream
            .write_all(&server_ping_frame(b"codex-ping"))
            .await
            .unwrap();
        let (opcode, payload) = tokio::time::timeout(
            Duration::from_secs(1),
            read_client_websocket_frame_with_opcode(&mut stream),
        )
        .await
        .expect("client should reply to server ping before terminal response");
        assert_eq!(opcode, 0x0a);
        assert_eq!(payload, b"codex-ping");

        let payload = websocket_completed_response("resp_ws_pong", 6, 3);
        stream
            .write_all(&server_text_frame(payload.as_bytes()))
            .await
            .unwrap();
        sleep(Duration::from_millis(50)).await;
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(&base_request(), request_context("req_ws_ping", None))
        .await
        .unwrap();

    server.await.unwrap();
    assert!(response.body.contains("event: response.completed"));
    assert!(response.body.contains("\"id\":\"resp_ws_pong\""));
    assert_eq!(response.usage.unwrap().input_tokens, 6);
}

#[tokio::test]
async fn websocket_invalid_text_frame_should_be_ignored_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_upgrade_request(&mut stream).await;
        let accept_key = websocket_accept_key(&request);
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Accept: {accept_key}\r\n\
             \r\n"
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        let _client_request = read_client_websocket_frame(&mut stream).await;
        stream
            .write_all(&server_text_frame(b"not-json-from-upstream"))
            .await
            .unwrap();
        let payload = websocket_completed_response("resp_ws_after_invalid", 7, 4);
        stream
            .write_all(&server_text_frame(payload.as_bytes()))
            .await
            .unwrap();
        sleep(Duration::from_millis(50)).await;
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_invalid_text", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("not-json-from-upstream"));
    assert!(response.body.contains("event: response.completed"));
    assert!(response.body.contains("\"id\":\"resp_ws_after_invalid\""));
    assert_eq!(response.usage.unwrap().input_tokens, 7);
}

#[tokio::test(start_paused = true)]
async fn websocket_silent_upstream_should_timeout_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_seen_tx, request_seen_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        request_seen_tx.send(()).unwrap();
        futures::future::pending::<()>().await;
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );
    let response_task = tokio::spawn(async move {
        client
            .create_response(
                &base_request(),
                request_context("req_ws_silent_timeout", None),
            )
            .await
    });

    request_seen_rx.await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(20)).await;
    tokio::task::yield_now().await;

    let error = response_task.await.unwrap().unwrap_err();
    server.abort();
    let CodexClientError::WebSocket(CodexWebSocketError::ReceiveIdleTimeout { timeout }) = error
    else {
        panic!("expected receive idle timeout, found {error:?}");
    };
    assert_eq!(timeout, Duration::from_secs(20));
}

#[tokio::test]
async fn websocket_binary_response_frame_should_error_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_upgrade_request(&mut stream).await;
        let accept_key = websocket_accept_key(&request);
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Accept: {accept_key}\r\n\
             \r\n"
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        let _client_request = read_client_websocket_frame(&mut stream).await;
        let payload = websocket_completed_response("resp_ws_binary", 6, 3);
        stream
            .write_all(&server_binary_frame(payload.as_bytes()))
            .await
            .unwrap();
        sleep(Duration::from_millis(50)).await;
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(&base_request(), request_context("req_ws_binary", None))
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::WebSocket(CodexWebSocketError::UnexpectedBinaryEvent) = error else {
        panic!("expected unexpected binary websocket event, found {error:?}");
    };
}

#[tokio::test]
async fn websocket_close_frame_before_first_event_should_error_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_upgrade_request(&mut stream).await;
        let accept_key = websocket_accept_key(&request);
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Accept: {accept_key}\r\n\
             \r\n"
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        let _client_request = read_client_websocket_frame(&mut stream).await;
        stream.write_all(&server_close_frame()).await.unwrap();
        sleep(Duration::from_millis(50)).await;
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(&base_request(), request_context("req_ws_close", None))
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::WebSocket(CodexWebSocketError::ClosedByServerBeforeCompleted) = error
    else {
        panic!("expected server close before response.completed, found {error:?}");
    };
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_first_error_frame_should_surface_as_upstream_error_without_http_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket =
            accept_hdr_async(stream, |request: &WsRequest, response: WsResponse| {
                assert_eq!(
                    request
                        .headers()
                        .get("authorization")
                        .and_then(|value| value.to_str().ok()),
                    Some("Bearer access-token")
                );
                Ok(response)
            })
            .await
            .unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_limit",
                        "error": {
                            "code": "usage_limit_reached",
                            "message": "weekly limit reached",
                            "resets_in_seconds": 45
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

    let request = base_request();
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &request,
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_ws_error_frame",
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
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::Upstream {
        status,
        retry_after_seconds,
        body,
    } = error
    else {
        panic!("expected upstream error, found {error:?}");
    };
    assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after_seconds, Some(45));
    assert!(body.contains("usage_limit_reached"));
}

#[tokio::test]
async fn websocket_malformed_completed_response_should_error_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "object": "response",
                        "usage": {
                            "input_tokens": 1,
                            "output_tokens": 1
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

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &base_request(),
            request_context("req_ws_malformed_completed", None),
        )
        .await
        .unwrap_err();

    server.await.unwrap();
    assert!(
        error
            .to_string()
            .contains("failed to parse ResponseCompleted"),
        "unexpected error: {error:?}"
    );
}

#[tokio::test]
async fn websocket_completed_response_with_incomplete_usage_should_error_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_incomplete_usage",
                        "object": "response",
                        "usage": {
                            "input_tokens": 1,
                            "output_tokens": 1
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

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &base_request(),
            request_context("req_ws_incomplete_completed_usage", None),
        )
        .await
        .unwrap_err();

    server.await.unwrap();
    assert!(
        error
            .to_string()
            .contains("failed to parse ResponseCompleted"),
        "unexpected error: {error:?}"
    );
}

#[tokio::test]
async fn websocket_completed_without_response_should_not_finish_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
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

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &base_request(),
            request_context("req_ws_completed_without_response", None),
        )
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::WebSocket(CodexWebSocketError::ClosedByServerBeforeCompleted) = error
    else {
        panic!(
            "expected response-less completed frame to be ignored before close, found {error:?}"
        );
    };
}

#[tokio::test]
async fn websocket_created_without_response_should_be_ignored_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.created"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_created_without_response", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_created_without_response", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.created"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_output_text_delta_without_delta_should_be_ignored_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
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
                websocket_completed_response("resp_ws_output_text_delta_without_delta", 2, 1)
                    .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_output_text_delta_without_delta", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_text.delta"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_delta_events_missing_required_fields_should_be_ignored_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.custom_tool_call_input.delta",
                "delta": "partial input"
            }),
            json!({
                "type": "response.reasoning_summary_text.delta",
                "delta": "summary"
            }),
            json!({
                "type": "response.reasoning_text.delta",
                "delta": "reasoning"
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_delta_missing_required_fields", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_delta_missing_required_fields", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response
        .body
        .contains("event: response.custom_tool_call_input.delta"));
    assert!(!response
        .body
        .contains("event: response.reasoning_summary_text.delta"));
    assert!(!response
        .body
        .contains("event: response.reasoning_text.delta"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_output_item_events_without_item_should_be_ignored_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done"
            }),
            json!({
                "type": "response.output_item.added"
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_output_item_without_item", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_output_item_without_item", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_output_item_events_with_non_object_item_should_be_ignored_like_official_client()
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": 123
            }),
            json!({
                "type": "response.output_item.added",
                "item": 123
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_output_item_non_object_item", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_output_item_non_object_item", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_output_item_events_with_invalid_item_type_tag_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "role": "assistant",
                    "content": []
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": 123,
                    "role": "assistant",
                    "content": []
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_output_item_invalid_type_tag", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_output_item_invalid_type_tag", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_output_item_events_with_invalid_metadata_should_be_ignored_like_official_client()
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "metadata": []
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "web_search_call",
                    "metadata": {
                        "turn_id": {}
                    }
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_output_item_invalid_metadata", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_output_item_invalid_metadata", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_message_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "content": []
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "content": {}
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_message_output_item_invalid_shape", 2, 1)
                    .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_message_output_item_invalid_shape", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_message_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "id": {},
                    "role": "assistant",
                    "content": []
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "phase": "draft"
                }
            }),
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "phase": {}
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_message_output_item_invalid_optional", 2, 1)
                    .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_message_output_item_invalid_optional", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_message_output_item_events_with_invalid_content_items_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text"}]
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "input_image",
                        "image_url": {},
                        "detail": "thumbnail"
                    }]
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_message_content_item_invalid", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_message_content_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_agent_message_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "agent_message",
                    "recipient": "user",
                    "content": []
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "agent_message",
                    "author": "assistant",
                    "recipient": {},
                    "content": {}
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_agent_message_item_invalid", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_agent_message_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_agent_message_output_item_events_with_invalid_content_items_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "agent_message",
                    "author": "assistant",
                    "recipient": "user",
                    "content": [{"type": "input_text"}]
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "agent_message",
                    "author": "assistant",
                    "recipient": "user",
                    "content": [{
                        "type": "encrypted_content",
                        "encrypted_content": {}
                    }]
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_agent_message_content_item_invalid", 2, 1)
                    .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_agent_message_content_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_reasoning_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "reasoning",
                    "encrypted_content": "enc"
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "reasoning",
                    "id": {},
                    "summary": [],
                    "content": {},
                    "encrypted_content": {}
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_reasoning_item_invalid", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_reasoning_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_reasoning_output_item_events_with_invalid_nested_items_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "reasoning",
                    "summary": [{"type": "summary_text"}]
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "reasoning",
                    "summary": [],
                    "content": [{
                        "type": "reasoning_text",
                        "text": {}
                    }]
                }
            }),
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "reasoning",
                    "summary": [],
                    "content": [{"type": "unsupported"}]
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_reasoning_nested_item_invalid", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_reasoning_nested_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_function_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "arguments": "{}",
                    "call_id": "call_ws_invalid_function"
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "name": "lookup",
                    "arguments": {}
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_function_call_output_item_invalid", 2, 1)
                    .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_function_call_output_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_function_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "id": {},
                    "name": "lookup",
                    "arguments": "{}",
                    "call_id": "call_ws_invalid_function_optional"
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "name": "lookup",
                    "namespace": {},
                    "arguments": "{}",
                    "call_id": "call_ws_invalid_function_optional"
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response(
                    "resp_ws_function_call_output_item_invalid_optional",
                    2,
                    1,
                )
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_function_call_output_item_invalid_optional", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_tool_search_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "tool_search_call",
                    "arguments": {
                        "query": "calendar create"
                    }
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "tool_search_call",
                    "execution": "client"
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_tool_search_call_output_item_invalid", 2, 1)
                    .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_tool_search_call_output_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_tool_search_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "tool_search_call",
                    "id": {},
                    "execution": "client",
                    "arguments": {
                        "query": "calendar create"
                    }
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "tool_search_call",
                    "call_id": {},
                    "status": {},
                    "execution": "client",
                    "arguments": {}
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response(
                    "resp_ws_tool_search_call_output_item_invalid_optional",
                    2,
                    1,
                )
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_tool_search_call_output_item_invalid_optional", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_function_call_output_payload_item_events_with_invalid_required_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call_output",
                    "output": "ok"
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call_output",
                    "call_id": "call_ws_invalid_function_output",
                    "output": {}
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_function_call_output_item_invalid", 2, 1)
                    .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_function_call_output_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_custom_tool_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "custom_tool_call",
                    "name": "render",
                    "input": "{}"
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "custom_tool_call",
                    "call_id": "call_ws_invalid_custom_tool",
                    "name": "render",
                    "input": {}
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_custom_tool_call_item_invalid", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_custom_tool_call_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_custom_tool_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "custom_tool_call",
                    "id": {},
                    "call_id": "call_ws_invalid_custom_tool_optional",
                    "name": "render",
                    "input": "{}"
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "custom_tool_call",
                    "status": {},
                    "call_id": "call_ws_invalid_custom_tool_optional",
                    "name": "render",
                    "input": "{}"
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response(
                    "resp_ws_custom_tool_call_item_invalid_optional",
                    2,
                    1,
                )
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_custom_tool_call_item_invalid_optional", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_custom_tool_call_output_payload_item_events_with_invalid_required_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "custom_tool_call_output",
                    "output": "ok"
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "custom_tool_call_output",
                    "call_id": "call_ws_invalid_custom_tool_output",
                    "output": {}
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_custom_tool_call_output_item_invalid", 2, 1)
                    .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_custom_tool_call_output_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_custom_tool_call_output_result_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "custom_tool_call_output",
                    "call_id": "call_ws_invalid_custom_tool_output_optional",
                    "name": {},
                    "output": "ok"
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "custom_tool_call_output",
                    "call_id": "call_ws_invalid_custom_tool_output_optional",
                    "name": [],
                    "output": "ok"
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response(
                    "resp_ws_custom_tool_call_output_item_invalid_optional",
                    2,
                    1,
                )
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_custom_tool_call_output_item_invalid_optional", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_function_output_payload_item_events_with_invalid_content_items_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call_output",
                    "call_id": "call_ws_invalid_function_output_content",
                    "output": [{"type": "input_text"}]
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "custom_tool_call_output",
                    "call_id": "call_ws_invalid_custom_tool_output_content",
                    "output": [{
                        "type": "input_image",
                        "image_url": "data:image/png;base64,aaa",
                        "detail": "full"
                    }]
                }
            }),
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "custom_tool_call_output",
                    "call_id": "call_ws_invalid_custom_tool_output_encrypted",
                    "output": [{
                        "type": "encrypted_content",
                        "encrypted_content": {}
                    }]
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_function_output_content_item_invalid", 2, 1)
                    .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_function_output_content_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_tool_search_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "tool_search_output",
                    "execution": "client",
                    "tools": []
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "tool_search_output",
                    "status": "completed",
                    "execution": "client",
                    "tools": {}
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_tool_search_output_item_invalid", 2, 1)
                    .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_tool_search_output_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_tool_search_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "tool_search_output",
                        "call_id": {},
                        "status": "completed",
                        "execution": "server",
                        "tools": []
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response(
                    "resp_ws_tool_search_output_item_optional_invalid",
                    2,
                    1,
                )
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_tool_search_output_item_optional_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_local_shell_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "local_shell_call",
                    "action": {
                        "type": "exec",
                        "command": ["pwd"]
                    }
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "local_shell_call",
                    "status": "completed",
                    "action": {
                        "type": "exec",
                        "command": "pwd"
                    }
                }
            }),
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "local_shell_call",
                    "call_id": {},
                    "status": "done",
                    "action": {
                        "type": "exec",
                        "command": ["pwd"],
                        "timeout_ms": "1000",
                        "env": {
                            "PATH": {}
                        }
                    }
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_local_shell_call_item_invalid", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_local_shell_call_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_web_search_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "web_search_call",
                    "status": {},
                    "action": {
                        "type": "search",
                        "query": "weather"
                    }
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "web_search_call",
                    "status": "completed",
                    "action": {
                        "type": "search",
                        "query": {},
                        "queries": ["weather", {}]
                    }
                }
            }),
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "web_search_call",
                    "id": {},
                    "action": {
                        "type": "find_in_page",
                        "url": "https://example.com",
                        "pattern": {}
                    }
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_web_search_call_item_invalid", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_web_search_call_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_image_generation_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "image_generation_call",
                    "status": "completed",
                    "result": "Zm9v"
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "image_generation_call",
                    "id": "ig_ws_invalid",
                    "status": "completed",
                    "result": {}
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_image_generation_call_item_invalid", 2, 1)
                    .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_image_generation_call_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_image_generation_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "image_generation_call",
                        "id": "ig_ws_optional_invalid",
                        "status": "completed",
                        "revised_prompt": {},
                        "result": "Zm9v"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response(
                    "resp_ws_image_generation_call_item_optional_invalid",
                    2,
                    1,
                )
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_image_generation_call_item_optional_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_compaction_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "compaction"
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "compaction_summary",
                    "encrypted_content": {}
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_compaction_item_invalid", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_compaction_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_context_compaction_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "context_compaction",
                    "encrypted_content": {}
                }
            }),
            json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "context_compaction",
                    "encrypted_content": []
                }
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_context_compaction_item_invalid", 2, 1)
                    .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_context_compaction_item_invalid", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_reasoning_summary_part_added_without_summary_index_should_be_ignored_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.reasoning_summary_part.added"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response(
                    "resp_ws_reasoning_summary_part_added_without_summary_index",
                    2,
                    1,
                )
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context(
                "req_ws_reasoning_summary_part_added_without_summary_index",
                None,
            ),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response
        .body
        .contains("event: response.reasoning_summary_part.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_null_option_fields_should_be_ignored_like_missing_fields_in_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        for event in [
            json!({
                "type": "response.created",
                "response": null
            }),
            json!({
                "type": "response.output_text.delta",
                "delta": null
            }),
            json!({
                "type": "response.output_item.done",
                "item": null
            }),
            json!({
                "type": "response.output_item.added",
                "item": null
            }),
            json!({
                "type": "response.completed",
                "response": null
            }),
        ] {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_null_option_fields", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_null_options", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.created"));
    assert!(!response.body.contains("event: response.output_text.delta"));
    assert!(!response.body.contains("event: response.output_item.done"));
    assert!(!response.body.contains("event: response.output_item.added"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_frames_with_invalid_official_event_shape_should_be_ignored_like_official_client()
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": 123
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_invalid_event_shape", 2, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &base_request(),
            request_context("req_ws_invalid_event_shape", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("event: response.output_text.delta"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
async fn websocket_response_failed_rate_limit_message_should_surface_retry_after_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_rate_limit_delay",
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

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &base_request(),
            request_context("req_ws_rate_limit_message_delay", None),
        )
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::Upstream {
        status,
        retry_after_seconds,
        body,
    } = error
    else {
        panic!("expected rate-limit response.failed to surface as upstream error, found {error:?}");
    };
    assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after_seconds, Some(12));
    assert!(body.contains("Please try again in 11.054s"));
}

#[tokio::test]
async fn websocket_response_failed_retry_after_message_should_require_rate_limit_code_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_non_rate_limit_delay",
                        "error": {
                            "code": "upstream_transient_error",
                            "message": "Try again in 35 seconds."
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

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &base_request(),
            request_context("req_ws_non_rate_limit_message_delay", None),
        )
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::Upstream {
        status,
        retry_after_seconds,
        body,
    } = error
    else {
        panic!(
            "expected non-rate-limit response.failed to surface as upstream error, found {error:?}"
        );
    };
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(retry_after_seconds, None);
    assert!(body.contains("Try again in 35 seconds"));
}

#[tokio::test]
async fn websocket_response_failed_server_overloaded_should_surface_as_503_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_overloaded",
                        "error": {
                            "code": "server_is_overloaded",
                            "message": "The server is overloaded"
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

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &base_request(),
            request_context("req_ws_server_overloaded", None),
        )
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::Upstream { status, body, .. } = error else {
        panic!("expected server overload response.failed to surface as upstream error, found {error:?}");
    };
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("server_is_overloaded"));
    assert!(body.contains("The server is overloaded"));
}

#[tokio::test]
async fn websocket_unknown_response_failed_should_surface_as_503_like_official_retryable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_unknown_failure",
                        "error": {
                            "code": "upstream_transient_error",
                            "message": "Try again shortly"
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

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &base_request(),
            request_context("req_ws_unknown_failed", None),
        )
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::Upstream { status, body, .. } = error else {
        panic!("expected unknown response.failed to surface as upstream error, found {error:?}");
    };
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("upstream_transient_error"));
    assert!(body.contains("Try again shortly"));
}

#[tokio::test]
async fn websocket_response_failed_special_codes_should_use_official_status_classes() {
    let cases = [
        ("insufficient_quota", reqwest::StatusCode::PAYMENT_REQUIRED),
        ("quota_exceeded", reqwest::StatusCode::PAYMENT_REQUIRED),
        ("context_length_exceeded", reqwest::StatusCode::BAD_REQUEST),
        ("invalid_prompt", reqwest::StatusCode::BAD_REQUEST),
        ("cyber_policy", reqwest::StatusCode::BAD_REQUEST),
        ("usage_not_included", reqwest::StatusCode::TOO_MANY_REQUESTS),
    ];

    for (code, expected_status) in cases {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = accept_async(stream).await.unwrap();
            let _message = websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    json!({
                        "type": "response.failed",
                        "response": {
                            "id": format!("resp_{code}"),
                            "error": {
                                "code": code,
                                "message": format!("special failure {code}")
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

        let client = CodexBackendClient::new(
            build_reqwest_client(false).unwrap(),
            format!("http://{addr}"),
            codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
        );
        let error = client
            .create_response(
                &base_request(),
                request_context("req_ws_special_failed", None),
            )
            .await
            .unwrap_err();

        server.await.unwrap();
        let CodexClientError::Upstream { status, body, .. } = error else {
            panic!("expected {code} to surface as upstream error, found {error:?}");
        };
        assert_eq!(status, expected_status, "unexpected status for {code}");
        assert!(body.contains(code));
    }
}

#[tokio::test]
async fn websocket_wrapped_error_status_should_surface_as_upstream_error_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "error",
                    "status": 400,
                    "error": {
                        "type": "invalid_request_error",
                        "message": "Model does not support this request"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &base_request(),
            request_context("req_ws_wrapped_error", None),
        )
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::Upstream {
        status,
        retry_after_seconds,
        body,
    } = error
    else {
        panic!("expected wrapped websocket error to surface as upstream error, found {error:?}");
    };
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(retry_after_seconds, None);
    assert!(body.contains("invalid_request_error"));
    assert!(body.contains("Model does not support this request"));
}

#[tokio::test]
async fn websocket_unmapped_success_status_error_should_not_return_successful_error_event_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "error",
                    "status": 200,
                    "error": {
                        "type": "non_terminal_notice",
                        "message": "This frame should not complete the response"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &base_request(),
            request_context("req_ws_unmapped_success_error", None),
        )
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::WebSocket(CodexWebSocketError::ClosedByServerBeforeCompleted) = error
    else {
        panic!("expected unmapped success-status error frame to be ignored until close, found {error:?}");
    };
}

#[tokio::test]
async fn websocket_wrapped_error_retry_after_header_should_be_preserved_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "error",
                    "status": 429,
                    "error": {
                        "type": "rate_limit_exceeded",
                        "message": "Too many requests"
                    },
                    "headers": {
                        "retry-after": "37"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &base_request(),
            request_context("req_ws_wrapped_error_retry_after", None),
        )
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::Upstream {
        status,
        retry_after_seconds,
        body,
    } = error
    else {
        panic!(
            "expected wrapped websocket error retry-after to surface as upstream error, found {error:?}"
        );
    };
    assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after_seconds, Some(37));
    assert!(body.contains("rate_limit_exceeded"));
    assert!(body.contains("retry-after"));
}

#[tokio::test]
async fn websocket_wrapped_connection_limit_should_use_retryable_503_precedence_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "error",
                    "status": 400,
                    "error": {
                        "code": "websocket_connection_limit_reached",
                        "type": "invalid_request_error",
                        "message": "Responses websocket connection limit reached (60 minutes). Create a new websocket connection to continue."
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &base_request(),
            request_context("req_ws_wrapped_connection_limit", None),
        )
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::Upstream { status, body, .. } = error else {
        panic!("expected wrapped websocket connection limit to surface as upstream error, found {error:?}");
    };
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("websocket_connection_limit_reached"));
}

#[tokio::test]
async fn websocket_midstream_wrapped_error_status_should_surface_as_upstream_error_like_official_client(
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.created",
                    "response": {
                        "id": "resp_midstream_wrapped_error"
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
                    "type": "error",
                    "status": 400,
                    "error": {
                        "type": "invalid_request_error",
                        "message": "Mid-stream wrapped websocket error"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &base_request(),
            request_context("req_ws_midstream_wrapped_error", None),
        )
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::Upstream {
        status,
        retry_after_seconds,
        body,
    } = error
    else {
        panic!("expected mid-stream wrapped websocket error to surface as upstream error, found {error:?}");
    };
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(retry_after_seconds, None);
    assert!(body.contains("invalid_request_error"));
    assert!(body.contains("Mid-stream wrapped websocket error"));
}

#[tokio::test]
async fn websocket_incomplete_event_should_error_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
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

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(&base_request(), request_context("req_ws_incomplete", None))
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::WebSocket(CodexWebSocketError::IncompleteResponse { reason }) = error
    else {
        panic!("expected response.incomplete websocket error, found {error:?}");
    };
    assert_eq!(reason, "max_output_tokens");
}

#[tokio::test]
async fn websocket_connection_limit_failed_frame_should_surface_as_503_like_official_retryable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_ws_connection_limit",
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

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(&base_request(), request_context("req_ws_limit", None))
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::Upstream { status, body, .. } = error else {
        panic!("expected upstream error, found {error:?}");
    };
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("websocket_connection_limit_reached"));
}

#[tokio::test]
async fn websocket_pooled_connection_limit_frame_should_surface_as_503() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_pooled_ws_connection_limit",
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

    let pool = Arc::new(CodexWebSocketPool::with_config(manual_pool_config(
        Duration::from_secs(60),
        8,
    )));
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(pool, "chatgpt-account");
    let mut request = base_request();
    request.prompt_cache_key = Some("chatgpt-account:conversation".to_string());

    let error = client
        .create_response(&request, request_context("req_pooled_ws_limit", None))
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::Upstream { status, body, .. } = error else {
        panic!("expected upstream error, found {error:?}");
    };
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.contains("websocket_connection_limit_reached"));
}

#[tokio::test]
async fn websocket_pooled_upstream_error_should_discard_connection_like_official_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_websocket = accept_async(first_stream).await.unwrap();
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_pooled_rate_limit",
                        "error": {
                            "code": "rate_limit_exceeded",
                            "message": "Rate limit reached. Please try again in 1s."
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        let mut second_websocket = accept_async(second_stream).await.unwrap();
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                websocket_completed_response("resp_after_pooled_error", 8, 2).into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
    });

    let pool = Arc::new(CodexWebSocketPool::with_config(manual_pool_config(
        Duration::from_secs(60),
        8,
    )));
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(pool, "chatgpt-account");
    let mut request = base_request();
    request.prompt_cache_key = Some("chatgpt-account:conversation".to_string());

    let error = client
        .create_response(&request, request_context("req_pooled_ws_error", None))
        .await
        .unwrap_err();
    let CodexClientError::Upstream { status, body, .. } = error else {
        panic!("expected first pooled request to surface upstream error, found {error:?}");
    };
    assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert!(body.contains("rate_limit_exceeded"));

    let response = client
        .create_response(&request, request_context("req_pooled_ws_after_error", None))
        .await
        .unwrap();

    server.await.unwrap();
    assert!(response.body.contains("\"id\":\"resp_after_pooled_error\""));
    assert_eq!(response.usage.unwrap().input_tokens, 8);
}

#[tokio::test]
async fn ordinary_response_should_not_fallback_to_http_sse_when_websocket_transport_fails() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut websocket_stream, _) = listener.accept().await.unwrap();
        let websocket_request = read_http_upgrade_request(&mut websocket_stream).await;
        assert!(websocket_request.starts_with("GET /codex/responses HTTP/1.1"));
        drop(websocket_stream);
    });

    let request = base_request();
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let error = client
        .create_response(
            &request,
            request_context("req_ws_transport_no_fallback", None),
        )
        .await
        .unwrap_err();

    server.await.unwrap();
    let CodexClientError::WebSocket(CodexWebSocketError::Transport(_)) = error else {
        panic!("expected websocket transport error without HTTP SSE fallback, found {error:?}");
    };
}

#[tokio::test]
async fn ordinary_response_should_fallback_to_http_sse_when_websocket_upgrade_required() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut websocket_stream, _) = listener.accept().await.unwrap();
        let websocket_request = read_http_upgrade_request(&mut websocket_stream).await;
        assert!(websocket_request.starts_with("GET /codex/responses HTTP/1.1"));
        websocket_stream
            .write_all(
                b"HTTP/1.1 426 Upgrade Required\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();

        let (mut http_stream, _) = listener.accept().await.unwrap();
        let http_request = read_http_upgrade_request(&mut http_stream).await;
        assert!(http_request.starts_with("POST /codex/responses HTTP/1.1"));
        let body = "event: response.completed\ndata: {\"response\":{\"id\":\"resp_http_fallback\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n";
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        http_stream.write_all(response.as_bytes()).await.unwrap();
    });

    let request = base_request();
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &request,
            request_context("req_ws_upgrade_required_fallback", None),
        )
        .await
        .unwrap();

    server.await.unwrap();
    assert!(response.body.contains("event: response.completed"));
    assert!(response.body.contains("\"id\":\"resp_http_fallback\""));
    assert_eq!(response.usage.unwrap().input_tokens, 3);
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_success_should_capture_handshake_headers_and_rate_limit_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket =
            accept_hdr_async(stream, |_request: &WsRequest, mut response: WsResponse| {
                response
                    .headers_mut()
                    .insert("x-codex-turn-state", "turn-ws".parse().unwrap());
                response.headers_mut().insert(
                    "set-cookie",
                    "cf_clearance=ws; Domain=.chatgpt.com; Path=/"
                        .parse()
                        .unwrap(),
                );
                response
                    .headers_mut()
                    .insert("x-ratelimit-remaining-requests", "41".parse().unwrap());
                Ok(response)
            })
            .await
            .unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_ws_headers",
                        "object": "response",
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

    let mut request = base_request();
    request.use_websocket = true;
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &request,
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_ws_headers",
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
        .unwrap();

    server.await.unwrap();
    assert_eq!(response.turn_state.as_deref(), Some("turn-ws"));
    assert_eq!(
        response.set_cookie_headers,
        vec!["cf_clearance=ws; Domain=.chatgpt.com; Path=/".to_string()]
    );
    assert!(response
        .rate_limit_headers
        .iter()
        .any(|(name, value)| { name == "x-ratelimit-remaining-requests" && value == "41" }));
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_success_should_capture_internal_rate_limit_events_without_forwarding_them() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_hdr_async(stream, |_request: &WsRequest, response| Ok(response))
            .await
            .unwrap();
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
                websocket_completed_response("resp_ws_rate_limits", 1, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let mut request = base_request();
    request.use_websocket = true;
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &request,
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_ws_rate_limits",
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
        .unwrap();

    server.await.unwrap();
    assert!(!response.body.contains("codex.rate_limits"));
    assert!(response
        .rate_limit_headers
        .iter()
        .any(|(name, value)| { name == "x-codex-primary-used-percent" && value == "100" }));
    assert!(response
        .rate_limit_headers
        .iter()
        .any(|(name, value)| { name == "x-codex-primary-reset-at" && value == "1893456300" }));
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_metadata_event_should_update_turn_state_without_forwarding() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_hdr_async(stream, |_request: &WsRequest, response| Ok(response))
            .await
            .unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
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
        websocket
            .send(Message::Text(
                websocket_completed_response("resp_ws_metadata", 1, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });

    let mut request = base_request();
    request.use_websocket = true;
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(&request, request_context("req_ws_metadata", None))
        .await
        .unwrap();

    server.await.unwrap();
    assert_eq!(response.turn_state.as_deref(), Some("turn-from-metadata"));
    assert!(!response.body.contains("response.metadata"));
    assert!(response.body.contains("event: response.completed"));
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_stream_should_error_when_connection_closes_before_terminal_frame() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_hdr_async(stream, |_request: &WsRequest, response| Ok(response))
            .await
            .unwrap();
        let _request = websocket.next().await.unwrap().unwrap();
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

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );
    let mut stream = client
        .websocket_stream_response(&base_request(), request_context("req_mid_close", None))
        .await
        .unwrap()
        .body_stream;

    let first_chunk = stream.next().await.unwrap().unwrap();
    assert!(first_chunk.contains("partial"));
    let error = stream.next().await.unwrap().unwrap_err();
    assert!(error
        .to_string()
        .contains("websocket closed by server before response.completed"));

    server.await.unwrap();
}

fn base_request() -> CodexResponsesRequest {
    CodexResponsesRequest::new_http_sse("gpt-5.5", "", Vec::new())
}

fn manual_pool_config(max_age: Duration, max_per_account: usize) -> CodexWebSocketPoolConfig {
    CodexWebSocketPoolConfig {
        enabled: true,
        max_age,
        max_per_account,
        maintenance_interval: None,
        ping_interval: None,
        ping_timeout: Duration::from_millis(50),
        liveness_timeout: None,
    }
}

fn keepalive_pool_config(ping_timeout: Duration) -> CodexWebSocketPoolConfig {
    CodexWebSocketPoolConfig {
        ping_interval: Some(Duration::from_millis(1)),
        ping_timeout,
        liveness_timeout: Some(Duration::from_secs(60)),
        ..manual_pool_config(Duration::from_secs(60), 8)
    }
}

fn request_context<'a>(
    request_id: &'a str,
    session_id: Option<&'a str>,
) -> CodexRequestContext<'a> {
    CodexRequestContext {
        access_token: "access-token",
        account_id: Some("chatgpt-account"),
        request_id,
        turn_state: None,
        turn_metadata: None,
        beta_features: None,
        include_timing_metrics: None,
        version: None,
        codex_window_id: None,
        parent_thread_id: None,
        cookie_header: None,
        installation_id: None,
        session_id,
    }
}

fn audit_artifact_with_header_names(header_names: &[&str]) -> WebSocketAuditArtifact {
    WebSocketAuditArtifact {
        transport_mode: "websocket_preferred".to_string(),
        fallback_allowed: true,
        opening: Some(OpeningAuditSnapshot {
            request_line: "GET /codex/responses HTTP/1.1".to_string(),
            headers: header_names
                .iter()
                .map(|name| OpeningAuditHeader {
                    name: (*name).to_string(),
                    value: "value".to_string(),
                })
                .collect(),
        }),
        payload: Some(PayloadAuditSnapshot {
            top_level_keys: vec!["type".to_string(), "model".to_string()],
            body: json!({
                "type": "response.create",
                "model": "gpt-5.5",
            }),
        }),
        error: None,
    }
}

#[derive(Debug)]
struct CapturedWebSocketExchange {
    opening_bytes: String,
    first_frame_text: String,
    sec_websocket_key_offset: usize,
    authorization_header_offset: usize,
}

async fn capture_codex_websocket_exchange(
    request: &CodexResponsesRequest,
    context: CodexRequestContext<'_>,
    artifact_path: &Path,
) -> CapturedWebSocketExchange {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let opening_bytes = read_http_upgrade_request(&mut stream).await;
        let accept_key = websocket_accept_key(&opening_bytes);
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Accept: {accept_key}\r\n\
             \r\n"
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        let first_frame = read_client_websocket_frame(&mut stream).await;
        let first_frame_text = String::from_utf8(first_frame).unwrap();
        let payload = websocket_completed_response("resp_ws_capture", 3, 1);
        stream
            .write_all(&encode_server_frame(0x1, false, payload.as_bytes()))
            .await
            .unwrap();
        let lower_opening_bytes = opening_bytes.to_ascii_lowercase();
        CapturedWebSocketExchange {
            sec_websocket_key_offset: lower_opening_bytes
                .find("sec-websocket-key: ")
                .expect("capture should include Sec-WebSocket-Key"),
            authorization_header_offset: lower_opening_bytes
                .find("authorization: ")
                .expect("capture should include Authorization"),
            opening_bytes,
            first_frame_text,
        }
    });

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_rs::codex::gateway::fingerprint::model::Fingerprint::default_for_tests(),
    );
    client.create_response(request, context).await.unwrap();

    let capture = server.await.unwrap();
    write_capture_artifact(artifact_path, request, &capture).await;
    capture
}

async fn write_capture_artifact(
    artifact_path: &Path,
    request: &CodexResponsesRequest,
    capture: &CapturedWebSocketExchange,
) {
    if let Some(parent) = artifact_path.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }
    let artifact = json!({
        "transport_mode": "websocket_preferred",
        "fallback_allowed": true,
        "opening": {
            "request_line": capture.opening_bytes.lines().next().unwrap_or_default(),
            "headers": redacted_headers_from_raw_opening(&capture.opening_bytes),
            "sec_websocket_key_offset": capture.sec_websocket_key_offset,
            "authorization_header_offset": capture.authorization_header_offset,
        },
        "first_frame": websocket_payload_audit_snapshot(request),
        "tls": {
            "captured": false,
            "reason": "local WSS capture is not used in this test because the production client validates against native system roots"
        }
    });
    let body = serde_json::to_vec_pretty(&artifact).unwrap();
    tokio::fs::write(artifact_path, body).await.unwrap();
}

fn redacted_headers_from_raw_opening(raw_request: &str) -> Vec<Value> {
    raw_request
        .split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| {
            let (name, value) = line.split_once(": ")?;
            Some(json!({
                "name": name,
                "value": redacted_capture_header_value(name, value),
            }))
        })
        .collect()
}

fn redacted_capture_header_value(name: &str, value: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "authorization"
        | "chatgpt-account-id"
        | "cookie"
        | "x-client-request-id"
        | "x-codex-installation-id"
        | "session_id"
        | "session-id"
        | "thread-id"
        | "x-codex-window-id"
        | "x-codex-turn-state"
        | "x-codex-turn-metadata"
        | "x-codex-parent-thread-id" => "<redacted>".to_string(),
        "sec-websocket-key" => "<dynamic>".to_string(),
        _ => value.to_string(),
    }
}

fn websocket_completed_response(
    response_id: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> String {
    json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "object": "response",
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "total_tokens": input_tokens + output_tokens
            }
        }
    })
    .to_string()
}

fn with_sse_terminal_separator(body: &str) -> String {
    if body.ends_with("\n\n") {
        body.to_string()
    } else {
        format!("{body}\n")
    }
}

async fn read_http_upgrade_request(stream: &mut tokio::net::TcpStream) -> String {
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

async fn read_client_websocket_frame(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    read_client_websocket_frame_with_opcode(stream).await.1
}

async fn read_client_websocket_frame_with_opcode(
    stream: &mut tokio::net::TcpStream,
) -> (u8, Vec<u8>) {
    let mut header = [0_u8; 2];
    stream.read_exact(&mut header).await.unwrap();
    let opcode = header[0] & 0x0f;
    let mut payload_len = u64::from(header[1] & 0x7f);
    if payload_len == 126 {
        let mut extended = [0_u8; 2];
        stream.read_exact(&mut extended).await.unwrap();
        payload_len = u64::from(u16::from_be_bytes(extended));
    } else if payload_len == 127 {
        let mut extended = [0_u8; 8];
        stream.read_exact(&mut extended).await.unwrap();
        payload_len = u64::from_be_bytes(extended);
    }
    let mut mask = [0_u8; 4];
    if header[1] & 0x80 != 0 {
        stream.read_exact(&mut mask).await.unwrap();
    }
    let mut payload = vec![0_u8; payload_len as usize];
    stream.read_exact(&mut payload).await.unwrap();
    if header[1] & 0x80 != 0 {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % mask.len()];
        }
    }
    (opcode, payload)
}

fn websocket_accept_key(raw_request: &str) -> String {
    let key = raw_request
        .lines()
        .find_map(|line| line.strip_prefix("Sec-WebSocket-Key: "))
        .expect("raw websocket request should contain Sec-WebSocket-Key");
    derive_accept_key(key.as_bytes())
}

fn compressed_server_text_frame(text: &str) -> Vec<u8> {
    let mut compressor = Compress::new(Compression::fast(), false);
    let mut payload = Vec::with_capacity(text.len() + 32);
    compressor
        .compress_vec(text.as_bytes(), &mut payload, FlushCompress::Sync)
        .unwrap();
    if payload.ends_with(&[0x00, 0x00, 0xff, 0xff]) {
        payload.truncate(payload.len() - 4);
    }
    encode_server_frame(0x1, true, &payload)
}

fn server_ping_frame(payload: &[u8]) -> Vec<u8> {
    server_frame(0x89, payload)
}

fn server_text_frame(payload: &[u8]) -> Vec<u8> {
    server_frame(0x81, payload)
}

fn server_binary_frame(payload: &[u8]) -> Vec<u8> {
    server_frame(0x82, payload)
}

fn server_close_frame() -> Vec<u8> {
    server_frame(0x88, &1000_u16.to_be_bytes())
}

fn server_frame(first_byte: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![first_byte];
    if payload.len() < 126 {
        frame.push(payload.len() as u8);
    } else if u16::try_from(payload.len()).is_ok() {
        frame.push(126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    frame
}

fn encode_server_frame(opcode: u8, rsv1: bool, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::new();
    let first = 0x80 | if rsv1 { 0x40 } else { 0 } | opcode;
    frame.push(first);
    match payload.len() {
        len @ 0..=125 => frame.push(len as u8),
        len @ 126..=65_535 => {
            frame.push(126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            frame.push(127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    frame
}

fn assert_headers_appear_in_order(raw_request: &str, expected_headers: &[&str]) {
    assert_substrings_appear_in_order(raw_request, expected_headers);
}

fn assert_substrings_appear_in_order(haystack: &str, expected_values: &[&str]) {
    let mut offset = 0;
    for expected in expected_values {
        let Some(index) = haystack[offset..].find(expected) else {
            panic!("missing expected substring `{expected}` in:\n{haystack}");
        };
        offset += index + expected.len();
    }
}
