use std::{
    io::Write,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use codex_proxy_adapters::codex::{
    client,
    websocket::pool::{CodexWebSocketPool, CodexWebSocketPoolConfig},
};
use flate2::{write::DeflateEncoder, Compression};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{
    accept_async, accept_hdr_async,
    tungstenite::{
        handshake::{
            derive_accept_key,
            server::{Request as WsRequest, Response as WsResponse},
        },
        Message,
    },
};

#[test]
fn endpoints_should_join_backend_paths() {
    assert_eq!(
        client::endpoint_url("https://api.example.com/", "/codex/responses"),
        "https://api.example.com/codex/responses"
    );
    assert_eq!(
        client::endpoint_request_path("https://api.example.com/backend-api", "/codex/usage"),
        "/backend-api/codex/usage"
    );
}

#[test]
fn ordered_codex_headers_should_preserve_fingerprint_priority_and_request_fields() {
    let fingerprint = codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests();

    let headers = client::build_ordered_codex_headers(
        &fingerprint,
        "access-token",
        Some("acct-1"),
        Some("turn-1"),
        "req-1",
    );
    let keys = headers.keys().cloned().collect::<Vec<_>>();

    assert_eq!(headers["authorization"], "Bearer access-token");
    assert_eq!(headers["chatgpt-account-id"], "acct-1");
    assert_eq!(headers["x-client-request-id"], "req-1");
    assert_eq!(headers["x-codex-turn-state"], "turn-1");
    assert_eq!(headers["accept"], "text/event-stream");
    assert_eq!(keys.first().map(String::as_str), Some("authorization"));
}

#[test]
fn custom_ca_should_report_environment_cache_key_consistently() {
    let cache_key = client::custom_ca_env_cache_key();

    assert_eq!(
        cache_key.is_some(),
        std::env::var("CODEX_CA_CERTIFICATE").is_ok() || std::env::var("SSL_CERT_FILE").is_ok()
    );
}

#[test]
fn reqwest_http2_feature_should_be_enabled_for_fallback_parity() {
    let builder = reqwest::Client::builder().http2_prior_knowledge();

    drop(builder);
}

#[test]
fn websocket_connection_should_preserve_endpoint_and_header_order() {
    let connection = codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::new(
        "wss://chatgpt.com/backend-api/codex",
        vec![
            ("authorization".to_string(), "Bearer token".to_string()),
            ("user-agent".to_string(), "Codex Desktop/test".to_string()),
        ],
    );

    assert_eq!(
        (
            connection.endpoint(),
            connection.opening_audit_snapshot().header_order,
        ),
        (
            "wss://chatgpt.com/backend-api/codex",
            vec!["authorization".to_string(), "user-agent".to_string()],
        )
    );
}

#[test]
fn websocket_connection_opening_audit_should_redact_sensitive_headers() {
    let connection = codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::new(
        "wss://chatgpt.com/backend-api/codex/responses?source=audit",
        vec![
            (
                "authorization".to_string(),
                "Bearer access-secret".to_string(),
            ),
            ("chatgpt-account-id".to_string(), "acct-secret".to_string()),
            ("user-agent".to_string(), "Codex Desktop/test".to_string()),
            ("x-client-request-id".to_string(), "req-secret".to_string()),
            (
                "x-codex-turn-metadata".to_string(),
                "{\"secret\":true}".to_string(),
            ),
        ],
    );

    let snapshot = connection.opening_audit_snapshot();

    assert_eq!(
        snapshot.request_line,
        "GET /backend-api/codex/responses?source=audit HTTP/1.1"
    );
    assert_eq!(
        snapshot
            .headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("authorization", "<redacted>"),
            ("chatgpt-account-id", "<redacted>"),
            ("user-agent", "Codex Desktop/test"),
            ("x-client-request-id", "<redacted>"),
            ("x-codex-turn-metadata", "<redacted>"),
        ]
    );
}

#[tokio::test]
async fn websocket_audit_artifact_should_require_explicit_directory() {
    let connection =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses(
            "https://chatgpt.com",
            "test-websocket-key",
            vec![
                (
                    "authorization".to_string(),
                    "Bearer access-token".to_string(),
                ),
                ("chatgpt-account-id".to_string(), "acct-secret".to_string()),
                ("cookie".to_string(), "session=secret".to_string()),
            ],
        );
    let mut payload_request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "private instructions",
            vec![json!({"role": "user", "content": "private prompt"})],
        );
    payload_request.prompt_cache_key = Some("cache-secret".to_string());
    payload_request.client_metadata = Some(json!({"thread_id": "thread-secret"}));
    let mut artifact =
        codex_proxy_core::protocol::codex::websocket::websocket_audit_artifact_from_attempt(
            &payload_request,
            connection.opening_audit_snapshot(),
            codex_proxy_core::protocol::codex::websocket::websocket_payload_audit_snapshot(
                &payload_request,
            ),
        );
    artifact.error = Some(
        codex_proxy_core::protocol::codex::websocket::WebSocketAuditErrorSnapshot {
            classification: "opening_failed".to_string(),
            message: "connection refused".to_string(),
        },
    );
    let dir = tempfile::tempdir().expect("temp dir");

    let disabled =
        codex_proxy_adapters::codex::websocket::opening::write_websocket_audit_artifact_for_dir(
            None, &artifact,
        )
        .await
        .expect("disabled audit should be ok");

    assert!(disabled.is_none());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

    let written =
        codex_proxy_adapters::codex::websocket::opening::write_websocket_audit_artifact_for_dir(
            Some(dir.path()),
            &artifact,
        )
        .await
        .expect("enabled audit should write")
        .expect("enabled audit path");
    let body = std::fs::read_to_string(&written).expect("audit file");
    let json = serde_json::from_str::<serde_json::Value>(&body).expect("audit json");

    assert_eq!(json["transport_mode"], "websocket_preferred");
    assert_eq!(
        json["opening"]["request_line"],
        "GET /codex/responses HTTP/1.1"
    );
    assert_eq!(json["payload"]["top_level_keys"][1], "model");
    assert_eq!(json["error"]["classification"], "opening_failed");
    assert!(!body.contains("access-token"));
    assert!(!body.contains("acct-secret"));
    assert!(!body.contains("session=secret"));
    assert!(!body.contains("private prompt"));
    assert!(!body.contains("private instructions"));
    assert!(!body.contains("cache-secret"));
    assert!(!body.contains("thread-secret"));
}

#[test]
fn websocket_responses_endpoint_should_convert_http_base_url_to_ws_endpoint() {
    assert_eq!(
        codex_proxy_adapters::codex::websocket::connect::responses_websocket_endpoint(
            "https://chatgpt.com/backend-api"
        ),
        "wss://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(
        codex_proxy_adapters::codex::websocket::connect::responses_websocket_endpoint(
            "http://127.0.0.1:8080"
        ),
        "ws://127.0.0.1:8080/codex/responses"
    );
}

#[test]
fn websocket_deflate_should_rewrite_compressed_server_text_frame() {
    let payload = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_deflate",
            "object": "response"
        }
    })
    .to_string();
    let compressed = compressed_permessage_deflate_payload(payload.as_bytes());
    let frame = server_websocket_frame(0x1, true, &compressed);

    let rewritten =
        codex_proxy_adapters::codex::websocket::deflate::rewrite_permessage_deflate_server_frame(
            &frame,
        )
        .expect("compressed frame should rewrite")
        .expect("compressed frame should produce rewritten frame");

    assert_eq!(rewritten[0] & 0x40, 0);
    assert_eq!(rewritten[0] & 0x0f, 0x1);
    assert_eq!(server_websocket_payload(&rewritten), payload.as_bytes());
}

#[test]
fn websocket_deflate_should_leave_uncompressed_server_frame_unchanged() {
    let frame = server_websocket_frame(0x1, false, b"plain");

    let rewritten =
        codex_proxy_adapters::codex::websocket::deflate::rewrite_permessage_deflate_server_frame(
            &frame,
        )
        .expect("plain frame should parse");

    assert_eq!(rewritten, None);
}

#[tokio::test]
async fn codex_backend_client_should_decode_live_permessage_deflate_websocket_frame() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        let websocket_key = websocket_opening_header(&request, "sec-websocket-key")
            .expect("opening request should include sec-websocket-key");
        let accept_key = derive_accept_key(websocket_key.as_bytes());
        stream
            .write_all(
                format!(
                    "HTTP/1.1 101 Switching Protocols\r\n\
                     Upgrade: websocket\r\n\
                     Connection: Upgrade\r\n\
                     Sec-WebSocket-Accept: {accept_key}\r\n\
                     Sec-WebSocket-Extensions: permessage-deflate\r\n\
                     \r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        let completed = json!({
            "type": "response.completed",
            "response": {
                "id": "resp_live_deflate",
                "object": "response",
                "usage": {
                    "input_tokens": 3,
                    "output_tokens": 1,
                    "total_tokens": 4
                }
            }
        })
        .to_string();
        let compressed = compressed_permessage_deflate_payload(completed.as_bytes());
        stream
            .write_all(&server_websocket_frame(0x1, true, &compressed))
            .await
            .unwrap();
        stream.flush().await.unwrap();
    });
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
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

    assert!(response.body.contains("resp_live_deflate"));
}

fn compressed_permessage_deflate_payload(payload: &[u8]) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(payload).expect("payload should compress");
    encoder.finish().expect("compressed payload")
}

fn server_websocket_frame(opcode: u8, rsv1: bool, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.push(0x80 | if rsv1 { 0x40 } else { 0 } | opcode);
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

fn server_websocket_payload(frame: &[u8]) -> &[u8] {
    let len_marker = frame[1] & 0x7f;
    let (offset, len) = match len_marker {
        len @ 0..=125 => (2, usize::from(len)),
        126 => (4, usize::from(u16::from_be_bytes([frame[2], frame[3]]))),
        127 => {
            let len = u64::from_be_bytes([
                frame[2], frame[3], frame[4], frame[5], frame[6], frame[7], frame[8], frame[9],
            ]) as usize;
            (10, len)
        }
        _ => unreachable!("websocket length marker is masked to 7 bits"),
    };
    &frame[offset..offset + len]
}

#[test]
fn websocket_connection_should_build_standard_opening_headers_around_business_headers() {
    let connection =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses(
            "https://chatgpt.com/backend-api",
            "test-websocket-key",
            vec![
                (
                    "authorization".to_string(),
                    "Bearer access-token".to_string(),
                ),
                ("user-agent".to_string(), "Codex Desktop/test".to_string()),
                (
                    "openai-beta".to_string(),
                    "responses_websockets=2026-02-06".to_string(),
                ),
            ],
        );

    let snapshot = connection.opening_audit_snapshot();

    assert_eq!(
        connection.endpoint(),
        "wss://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(
        snapshot.header_order,
        vec![
            "Host",
            "Connection",
            "Upgrade",
            "Sec-WebSocket-Version",
            "Sec-WebSocket-Key",
            "authorization",
            "user-agent",
            "openai-beta",
            "sec-websocket-extensions",
        ]
    );
    assert_eq!(
        snapshot
            .headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("Host", "chatgpt.com"),
            ("Connection", "Upgrade"),
            ("Upgrade", "websocket"),
            ("Sec-WebSocket-Version", "13"),
            ("Sec-WebSocket-Key", "test-websocket-key"),
            ("authorization", "<redacted>"),
            ("user-agent", "Codex Desktop/test"),
            ("openai-beta", "responses_websockets=2026-02-06"),
            (
                "sec-websocket-extensions",
                "permessage-deflate; client_max_window_bits"
            ),
        ]
    );
}

#[test]
fn websocket_connection_should_render_raw_opening_bytes_for_capture_parity() {
    let connection =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses(
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
    let request = codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        vec![json!({
            "role": "user",
            "content": "hello",
        })],
    );

    let prepared =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses_create_request(
            "https://chatgpt.com/backend-api",
            "test-websocket-key",
            vec![("authorization".to_string(), "Bearer access-token".to_string())],
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
fn websocket_connection_should_prepare_capture_payload_with_old_field_order() {
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
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
    request.generate = Some(false);

    let prepared =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses_create_request(
            "https://chatgpt.com/backend-api",
            "test-websocket-key",
            vec![("authorization".to_string(), "Bearer access-token".to_string())],
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
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_collect_completed_sse() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
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
    let request = codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses_create_request(
            &format!("http://{addr}"),
            "dGhlIHNhbXBsZSBub25jZQ==",
            vec![("authorization".to_string(), "Bearer access-token".to_string())],
            &request,
        )
        .expect("payload should serialize");

    let response =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
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
        let mut websocket = accept_async(stream).await.unwrap();
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
    let request = codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses_create_request(
            &format!("http://{addr}"),
            "dGhlIHNhbXBsZSBub25jZQ==",
            vec![("authorization".to_string(), "Bearer access-token".to_string())],
            &request,
        )
        .expect("payload should serialize");

    let error =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
            .await
            .expect_err("response.failed should be surfaced as upstream error");
    server.await.unwrap();

    let codex_proxy_adapters::codex::websocket::connect::CodexWebSocketExchangeError::Upstream {
        status_code,
        retry_after_seconds,
        body,
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

    let error =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
            .await
            .expect_err("failed opening should surface upstream status");
    server.await.unwrap();

    let codex_proxy_adapters::codex::websocket::connect::CodexWebSocketExchangeError::Upstream {
        status_code,
        retry_after_seconds,
        body,
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
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Binary(b"unexpected-binary".to_vec().into()))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let prepared = prepared_websocket_request(&format!("http://{addr}"));

    let error =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
            .await
            .expect_err("binary websocket events should be rejected");
    server.await.unwrap();

    assert!(matches!(
        error,
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketExchangeError::UnexpectedBinaryEvent
    ));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_surface_wrapped_error_status_and_retry_after(
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

    let error =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
            .await
            .expect_err("wrapped error should surface as upstream error");
    server.await.unwrap();

    let codex_proxy_adapters::codex::websocket::connect::CodexWebSocketExchangeError::Upstream {
        status_code,
        retry_after_seconds,
        body,
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
        let mut websocket = accept_async(stream).await.unwrap();
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

    let error =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
            .await
            .expect_err("connection limit should surface as upstream error");
    server.await.unwrap();

    let codex_proxy_adapters::codex::websocket::connect::CodexWebSocketExchangeError::Upstream {
        status_code,
        body,
        ..
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
    let request = codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses_create_request(
            &format!("http://{addr}"),
            "dGhlIHNhbXBsZSBub25jZQ==",
            vec![("authorization".to_string(), "Bearer access-token".to_string())],
            &request,
        )
        .expect("payload should serialize");

    let response =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
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
        let mut websocket = accept_async(stream).await.unwrap();
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

    let response =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
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
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn websocket_execute_response_create_request_should_capture_handshake_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket =
            accept_hdr_async(stream, |_request: &WsRequest, mut response: WsResponse| {
                response
                    .headers_mut()
                    .insert("x-codex-turn-state", "turn-from-handshake".parse().unwrap());
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
    let request = codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses_create_request(
            &format!("http://{addr}"),
            "dGhlIHNhbXBsZSBub25jZQ==",
            vec![("authorization".to_string(), "Bearer access-token".to_string())],
            &request,
        )
        .expect("payload should serialize");

    let response =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
            .await
            .expect("websocket exchange should succeed");
    server.await.unwrap();

    assert_eq!(response.turn_state.as_deref(), Some("turn-from-handshake"));
    assert_eq!(
        response.set_cookie_headers,
        vec!["cf_clearance=ws; Domain=.chatgpt.com; Path=/".to_string()]
    );
    assert!(response
        .rate_limit_headers
        .iter()
        .any(|(name, value)| name == "x-ratelimit-remaining-requests" && value == "41"));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_surface_incomplete_response() {
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
    let request = codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses_create_request(
            &format!("http://{addr}"),
            "dGhlIHNhbXBsZSBub25jZQ==",
            vec![("authorization".to_string(), "Bearer access-token".to_string())],
            &request,
        )
        .expect("payload should serialize");

    let error =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
            .await
            .expect_err("response.incomplete should be surfaced as websocket error");
    server.await.unwrap();

    let codex_proxy_adapters::codex::websocket::connect::CodexWebSocketExchangeError::IncompleteResponse {
        reason,
    } = error
    else {
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
        let mut websocket = accept_async(stream).await.unwrap();
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
    let request = codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses_create_request(
            &format!("http://{addr}"),
            "dGhlIHNhbXBsZSBub25jZQ==",
            vec![("authorization".to_string(), "Bearer access-token".to_string())],
            &request,
        )
        .expect("payload should serialize");

    let error =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
            .await
            .expect_err("invalid response.completed should be rejected");
    server.await.unwrap();

    let codex_proxy_adapters::codex::websocket::connect::CodexWebSocketExchangeError::InvalidCompletedResponse {
        message,
    } = error
    else {
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
    let request = codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses_create_request(
            &format!("http://{addr}"),
            "dGhlIHNhbXBsZSBub25jZQ==",
            vec![("authorization".to_string(), "Bearer access-token".to_string())],
            &request,
        )
        .expect("payload should serialize");

    let error =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
            .await
            .expect_err("response.completed without response should not finish the stream");
    server.await.unwrap();

    assert!(matches!(
        error,
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketExchangeError::ClosedBeforeTerminal
    ));
}

#[tokio::test]
async fn websocket_execute_response_create_request_should_ignore_success_status_error_until_close()
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
    let request = codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses_create_request(
            &format!("http://{addr}"),
            "dGhlIHNhbXBsZSBub25jZQ==",
            vec![("authorization".to_string(), "Bearer access-token".to_string())],
            &request,
        )
        .expect("payload should serialize");

    let error =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
            .await
            .expect_err("unclassified success-status error frame should not finish the stream");
    server.await.unwrap();

    assert!(matches!(
        error,
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketExchangeError::ClosedBeforeTerminal
    ));
}

#[tokio::test(start_paused = true)]
async fn websocket_execute_response_create_request_should_timeout_when_upstream_is_silent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_seen_tx, request_seen_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        request_seen_tx.send(()).unwrap();
        futures::future::pending::<()>().await;
    });
    let prepared = prepared_websocket_request(&format!("http://{addr}"));

    let response_task = tokio::spawn(async move {
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
            .await
    });
    request_seen_rx.await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(20)).await;
    tokio::task::yield_now().await;

    let error = response_task
        .await
        .expect("websocket task should finish")
        .expect_err("silent upstream should time out");
    server.abort();

    let codex_proxy_adapters::codex::websocket::connect::CodexWebSocketExchangeError::ReceiveIdleTimeout {
        timeout,
    } = error
    else {
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
        let mut websocket = accept_async(stream).await.unwrap();
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
    let request = codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    let prepared =
        codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses_create_request(
            &format!("http://{addr}"),
            "dGhlIHNhbXBsZSBub25jZQ==",
            vec![("authorization".to_string(), "Bearer access-token".to_string())],
            &request,
        )
        .expect("payload should serialize");

    let response =
        codex_proxy_adapters::codex::websocket::connect::execute_response_create_request(&prepared)
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
        let mut websocket = accept_async(stream).await.unwrap();
        let _message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Binary(b"unexpected-binary".to_vec().into()))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
    });
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
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
        client::CodexClientError::WebSocket(
            codex_proxy_adapters::codex::websocket::connect::CodexWebSocketExchangeError::UnexpectedBinaryEvent
        )
    ));
}

#[tokio::test]
async fn codex_backend_client_stream_should_error_when_websocket_closes_before_terminal() {
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
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
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
        client::CodexClientError::WebSocket(
            codex_proxy_adapters::codex::websocket::connect::CodexWebSocketExchangeError::ClosedBeforeTerminal
        )
    ));
}

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn codex_backend_client_should_use_websocket_when_previous_response_id_is_present() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket =
            accept_hdr_async(stream, |_request: &WsRequest, mut response: WsResponse| {
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
                Ok(response)
            })
            .await
            .unwrap();
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
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );

    let response = backend
        .create_response(
            &request,
            client::CodexRequestContext {
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

#[tokio::test]
#[expect(
    clippy::result_large_err,
    reason = "tokio-tungstenite handshake callbacks use a large error response type"
)]
async fn codex_backend_client_websocket_should_forward_security_chain_headers_and_payload_fields() {
    let received_headers = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let headers_for_server = Arc::clone(&received_headers);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket =
            accept_hdr_async(stream, move |request: &WsRequest, response: WsResponse| {
                let headers = request
                    .headers()
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.as_str().to_string(),
                            value.to_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect::<Vec<_>>();
                *headers_for_server.lock().unwrap() = headers;
                Ok(response)
            })
            .await
            .unwrap();
        let message = websocket.next().await.unwrap().unwrap();
        let payload = serde_json::from_str::<serde_json::Value>(&message.into_text().unwrap())
            .expect("client payload should be json");
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_ws_security", 1, 1).into(),
            ))
            .await
            .unwrap();
        websocket.close(None).await.unwrap();
        payload
    });
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.use_websocket = true;
    request.prompt_cache_key = Some("client-thread".to_string());
    request.client_metadata = Some(json!({
        "safe": "yes",
        "x-openai-subagent": "review",
        "ignored_non_string": 42
    }));
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );

    let response = backend
        .create_response(
            &request,
            client::CodexRequestContext {
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
        .expect("websocket response should succeed");
    let payload = server.await.unwrap();

    assert!(response.body.contains("resp_ws_security"));
    assert_eq!(payload["prompt_cache_key"], "cp_derived");
    assert_eq!(
        payload["client_metadata"],
        json!({
            "safe": "yes",
            "x-openai-subagent": "review",
            "x-codex-installation-id": "install-123",
            "x-codex-window-id": "cw_derived",
            "x-codex-turn-metadata": "{\"thread_source\":\"subagent\"}",
            "x-codex-parent-thread-id": "parent-456"
        })
    );

    let headers = received_headers.lock().unwrap().clone();
    assert!(headers
        .iter()
        .any(|(name, value)| name == "x-client-request-id" && value == "cp_derived"));
    assert!(headers
        .iter()
        .any(|(name, value)| name == "x-openai-subagent" && value == "review"));
    assert!(headers
        .iter()
        .any(|(name, value)| name == "session-id" && value == "cp_derived"));
    assert!(headers
        .iter()
        .any(|(name, value)| name == "thread-id" && value == "cp_derived"));
    assert!(headers.iter().all(|(name, _)| name != "content-type"));
    assert!(headers.iter().all(|(name, _)| name != "accept"));
    assert!(headers.iter().all(|(name, _)| name != "session_id"));
}

#[test]
fn websocket_pool_should_apply_capacity_and_age_policy() {
    let pool = codex_proxy_adapters::codex::websocket::pool::CodexWebSocketPool::new(
        2,
        Duration::from_secs(60),
    );

    assert_eq!(
        (
            pool.permits_new_connection(1),
            pool.permits_new_connection(2),
            pool.should_recycle(Duration::from_secs(59)),
            pool.should_recycle(Duration::from_secs(60)),
        ),
        (true, false, false, true)
    );
}

#[tokio::test]
async fn codex_backend_client_should_reuse_pooled_websocket_for_same_account_and_conversation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut websocket = accept_async(stream).await.unwrap();
        for response_id in ["resp_pool_first", "resp_pool_second"] {
            let _message = websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    json!({
                        "type": "response.completed",
                        "response": {
                            "id": response_id,
                            "object": "response",
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
        }
        websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(
        codex_proxy_adapters::codex::websocket::pool::CodexWebSocketPool::new(
            8,
            Duration::from_secs(60),
        ),
    );
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool".to_string());

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_second", Some("chatgpt-account")),
        )
        .await
        .expect("second pooled websocket response should succeed");
    server.await.unwrap();

    assert!(first.body.contains("resp_pool_first"));
    assert!(second.body.contains("resp_pool_second"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn websocket_pool_should_bypass_busy_key_with_one_shot_connections() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (release_first_tx, release_first_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        let mut first_websocket = accept_async(first_stream).await.unwrap();
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.output_text.delta",
                    "delta": "first connection is still busy"
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
                completed_websocket_response("resp_busy_second", 2, 1).into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();

        let (third_stream, _) = listener.accept().await.unwrap();
        let mut third_websocket = accept_async(third_stream).await.unwrap();
        let _third_message = third_websocket.next().await.unwrap().unwrap();
        third_websocket
            .send(Message::Text(
                completed_websocket_response("resp_busy_third", 2, 1).into(),
            ))
            .await
            .unwrap();
        third_websocket.close(None).await.unwrap();

        release_first_rx.await.unwrap();
        first_websocket
            .send(Message::Text(
                completed_websocket_response("resp_busy_first", 2, 1).into(),
            ))
            .await
            .unwrap();
        first_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_default_max_age());
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(pool);
    let request = pooled_websocket_request("conversation-busy");

    let mut first = backend
        .create_response_stream(
            &request,
            request_context("req_busy_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket stream should start")
        .body;
    let first_chunk = first
        .next()
        .await
        .expect("first stream should yield an initial chunk")
        .expect("first stream chunk should be valid");
    let first_chunk = std::str::from_utf8(&first_chunk).unwrap();
    assert!(first_chunk.contains("first connection is still busy"));

    let second = backend
        .create_response(
            &request,
            request_context("req_busy_second", Some("chatgpt-account")),
        )
        .await
        .expect("busy key should bypass with a one-shot second connection");
    let third = backend
        .create_response(
            &request,
            request_context("req_busy_third", Some("chatgpt-account")),
        )
        .await
        .expect("busy key should bypass with a one-shot third connection");

    release_first_tx.send(()).unwrap();
    while first.next().await.transpose().unwrap().is_some() {}
    server.await.unwrap();

    assert!(second.body.contains("resp_busy_second"));
    assert!(third.body.contains("resp_busy_third"));
}

#[tokio::test]
async fn websocket_pool_should_bypass_new_keys_after_account_cap() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        for response_id in ["resp_cap_first", "resp_cap_second", "resp_cap_third"] {
            let (stream, _) = listener.accept().await.unwrap();
            accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
            let mut websocket = accept_async(stream).await.unwrap();
            let _message = websocket.next().await.unwrap().unwrap();
            websocket
                .send(Message::Text(
                    completed_websocket_response(response_id, 2, 1).into(),
                ))
                .await
                .unwrap();
            if response_id == "resp_cap_third" {
                websocket.close(None).await.unwrap();
            }
        }
    });
    let pool = Arc::new(CodexWebSocketPool::with_limits(Duration::from_secs(60), 1));
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(pool);
    let first_request = pooled_websocket_request("conversation-cap-one");
    let second_request = pooled_websocket_request("conversation-cap-two");

    let first = backend
        .create_response(
            &first_request,
            request_context("req_cap_first", Some("chatgpt-account")),
        )
        .await
        .expect("first capped websocket response should succeed");
    let second = backend
        .create_response(
            &second_request,
            request_context("req_cap_second", Some("chatgpt-account")),
        )
        .await
        .expect("new key over account cap should use one-shot connection");
    let third = backend
        .create_response(
            &second_request,
            request_context("req_cap_third", Some("chatgpt-account")),
        )
        .await
        .expect("capped key should keep bypassing instead of entering the pool");
    server.await.unwrap();

    assert!(first.body.contains("resp_cap_first"));
    assert!(second.body.contains("resp_cap_second"));
    assert!(third.body.contains("resp_cap_third"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn codex_backend_client_should_ping_idle_pooled_websocket_during_maintenance() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ping_count = Arc::new(AtomicUsize::new(0));
    let ping_count_for_server = Arc::clone(&ping_count);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _first_message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_keepalive_first",
                        "object": "response",
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

        let ping = timeout(Duration::from_secs(1), websocket.next())
            .await
            .expect("pool maintenance should probe the idle websocket")
            .expect("pool maintenance should send a websocket frame")
            .expect("pool maintenance frame should be valid");
        let Message::Ping(payload) = ping else {
            panic!("expected pool maintenance ping frame, got {ping:?}");
        };
        ping_count_for_server.fetch_add(1, Ordering::SeqCst);
        websocket.send(Message::Pong(payload)).await.unwrap();

        let _second_message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_keepalive_second",
                        "object": "response",
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
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(
        websocket_pool_config_for_tests(None, Some(Duration::from_millis(1)), None),
    ));
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool-keepalive".to_string());

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_keepalive_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    pool.maintain_idle_connections().await;
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_keepalive_second", Some("chatgpt-account")),
        )
        .await
        .expect("second pooled websocket response should reuse the probed socket");
    server.await.unwrap();

    assert!(first.body.contains("resp_pool_keepalive_first"));
    assert!(second.body.contains("resp_pool_keepalive_second"));
    assert_eq!(ping_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn websocket_pool_should_evict_idle_connection_when_ping_times_out() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_async(first_stream).await.unwrap();
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                completed_websocket_response("resp_no_pong_first", 2, 1).into(),
            ))
            .await
            .unwrap();
        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_async(second_stream).await.unwrap();
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                completed_websocket_response("resp_no_pong_second", 2, 1).into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
        drop(first_websocket);
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
        ping_interval: Some(Duration::from_millis(1)),
        ping_timeout: Duration::from_millis(20),
        maintenance_interval: None,
        ..websocket_pool_config_for_tests(None, None, None)
    }));
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let request = pooled_websocket_request("conversation-no-pong");

    let first = backend
        .create_response(
            &request,
            request_context("req_no_pong_first", Some("chatgpt-account")),
        )
        .await
        .expect("first websocket response should succeed");
    pool.maintain_idle_connections().await;
    let second = backend
        .create_response(
            &request,
            request_context("req_no_pong_second", Some("chatgpt-account")),
        )
        .await
        .expect("second websocket response should use a fresh connection");
    server.await.unwrap();

    assert!(first.body.contains("resp_no_pong_first"));
    assert!(second.body.contains("resp_no_pong_second"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn websocket_pool_should_gc_expired_idle_connections() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_async(first_stream).await.unwrap();
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                completed_websocket_response("resp_gc_first", 2, 1).into(),
            ))
            .await
            .unwrap();
        let close = timeout(Duration::from_secs(1), first_websocket.next())
            .await
            .expect("gc sweep should close the expired idle websocket")
            .expect("gc sweep should send a close frame")
            .expect("close frame should be valid");
        assert!(matches!(close, Message::Close(_)));

        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_async(second_stream).await.unwrap();
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                completed_websocket_response("resp_gc_second", 2, 1).into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(CodexWebSocketPoolConfig {
        max_age: Duration::from_millis(5),
        maintenance_interval: None,
        ping_interval: None,
        liveness_timeout: None,
        ..CodexWebSocketPoolConfig::default()
    }));
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let request = pooled_websocket_request("conversation-gc");

    let first = backend
        .create_response(
            &request,
            request_context("req_gc_first", Some("chatgpt-account")),
        )
        .await
        .expect("first websocket response should succeed");
    tokio::time::sleep(Duration::from_millis(15)).await;
    pool.gc_sweep().await;
    let second = backend
        .create_response(
            &request,
            request_context("req_gc_second", Some("chatgpt-account")),
        )
        .await
        .expect("second websocket response should use a fresh connection after gc");
    server.await.unwrap();

    assert!(first.body.contains("resp_gc_first"));
    assert!(second.body.contains("resp_gc_second"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn codex_backend_client_should_ping_idle_pooled_websocket_from_background_maintenance() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ping_count = Arc::new(AtomicUsize::new(0));
    let ping_count_for_server = Arc::clone(&ping_count);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut websocket = accept_async(stream).await.unwrap();
        let _first_message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_background_first",
                        "object": "response",
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

        let ping = timeout(Duration::from_secs(1), websocket.next())
            .await
            .expect("background maintenance should probe the idle websocket")
            .expect("background maintenance should send a websocket frame")
            .expect("background maintenance frame should be valid");
        let Message::Ping(payload) = ping else {
            panic!("expected background maintenance ping frame, got {ping:?}");
        };
        ping_count_for_server.fetch_add(1, Ordering::SeqCst);
        websocket.send(Message::Pong(payload)).await.unwrap();

        let _second_message = websocket.next().await.unwrap().unwrap();
        websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_background_second",
                        "object": "response",
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
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(
        websocket_pool_config_for_tests(
            Some(Duration::from_millis(20)),
            Some(Duration::from_secs(60)),
            None,
        ),
    ));
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool-background".to_string());

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_background_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    tokio::time::sleep(Duration::from_millis(80)).await;
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_background_second", Some("chatgpt-account")),
        )
        .await
        .expect("second pooled websocket response should reuse the background-probed socket");
    server.await.unwrap();
    pool.shutdown().await;

    assert!(first.body.contains("resp_pool_background_first"));
    assert!(second.body.contains("resp_pool_background_second"));
    assert_eq!(ping_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn codex_backend_client_should_close_idle_pooled_websocket_when_account_is_evicted() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_async(first_stream).await.unwrap();
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_evict_first",
                        "object": "response",
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
        let close = timeout(Duration::from_secs(1), first_websocket.next())
            .await
            .expect("evict_account should close the idle websocket")
            .expect("evict_account should send a close frame")
            .expect("close frame should be valid");
        assert!(matches!(close, Message::Close(_)));

        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_async(second_stream).await.unwrap();
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_evict_second",
                        "object": "response",
                        "usage": {
                            "input_tokens": 4,
                            "output_tokens": 1,
                            "total_tokens": 5
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(
        websocket_pool_config_for_tests(None, None, None),
    ));
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool-evict".to_string());

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_evict_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    pool.evict_account("chatgpt-account").await;
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_evict_second", Some("chatgpt-account")),
        )
        .await
        .expect("second websocket response should open a fresh socket after eviction");
    server.await.unwrap();

    assert!(first.body.contains("resp_pool_evict_first"));
    assert!(second.body.contains("resp_pool_evict_second"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn codex_backend_client_should_stop_reusing_pooled_websockets_after_shutdown() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_async(first_stream).await.unwrap();
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_shutdown_first",
                        "object": "response",
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
        let close = timeout(Duration::from_secs(1), first_websocket.next())
            .await
            .expect("shutdown should close the idle websocket")
            .expect("shutdown should send a close frame")
            .expect("close frame should be valid");
        assert!(matches!(close, Message::Close(_)));

        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_async(second_stream).await.unwrap();
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_shutdown_second",
                        "object": "response",
                        "usage": {
                            "input_tokens": 4,
                            "output_tokens": 1,
                            "total_tokens": 5
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(
        websocket_pool_config_for_tests(None, None, None),
    ));
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool-shutdown".to_string());

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_shutdown_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    pool.shutdown().await;
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_shutdown_second", Some("chatgpt-account")),
        )
        .await
        .expect("second websocket response should bypass the shut down pool");
    server.await.unwrap();

    assert!(first.body.contains("resp_pool_shutdown_first"));
    assert!(second.body.contains("resp_pool_shutdown_second"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn codex_backend_client_should_close_idle_pooled_websocket_after_liveness_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_async(first_stream).await.unwrap();
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_liveness_first",
                        "object": "response",
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
        let close = timeout(Duration::from_secs(1), first_websocket.next())
            .await
            .expect("liveness timeout should close the idle websocket")
            .expect("liveness timeout should send a close frame")
            .expect("close frame should be valid");
        assert!(matches!(close, Message::Close(_)));

        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_async(second_stream).await.unwrap();
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_liveness_second",
                        "object": "response",
                        "usage": {
                            "input_tokens": 4,
                            "output_tokens": 1,
                            "total_tokens": 5
                        }
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        second_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(CodexWebSocketPool::with_config(
        websocket_pool_config_for_tests(None, None, Some(Duration::from_millis(1))),
    ));
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(Arc::clone(&pool));
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool-liveness".to_string());

    let first = backend
        .create_response(
            &request,
            request_context("req_pool_liveness_first", Some("chatgpt-account")),
        )
        .await
        .expect("first pooled websocket response should succeed");
    tokio::time::sleep(Duration::from_millis(10)).await;
    pool.maintain_idle_connections().await;
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_liveness_second", Some("chatgpt-account")),
        )
        .await
        .expect("second websocket response should open a fresh socket after liveness close");
    server.await.unwrap();

    assert!(first.body.contains("resp_pool_liveness_first"));
    assert!(second.body.contains("resp_pool_liveness_second"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn codex_backend_client_should_discard_pooled_websocket_after_upstream_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted_connections = Arc::new(AtomicUsize::new(0));
    let accepted_connections_for_server = Arc::clone(&accepted_connections);
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut first_websocket = accept_async(first_stream).await.unwrap();
        let _first_message = first_websocket.next().await.unwrap().unwrap();
        first_websocket
            .send(Message::Text(
                json!({
                    "type": "response.failed",
                    "response": {
                        "id": "resp_pool_rate_limit",
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
        first_websocket.close(None).await.unwrap();

        let (second_stream, _) = listener.accept().await.unwrap();
        accepted_connections_for_server.fetch_add(1, Ordering::SeqCst);
        let mut second_websocket = accept_async(second_stream).await.unwrap();
        let _second_message = second_websocket.next().await.unwrap().unwrap();
        second_websocket
            .send(Message::Text(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_pool_after_error",
                        "object": "response",
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
        second_websocket.close(None).await.unwrap();
    });
    let pool = Arc::new(
        codex_proxy_adapters::codex::websocket::pool::CodexWebSocketPool::new(
            8,
            Duration::from_secs(60),
        ),
    );
    let backend = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    )
    .with_websocket_pool(pool);
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some("conversation-pool".to_string());

    let first_error = backend
        .create_response(
            &request,
            request_context("req_pool_error", Some("chatgpt-account")),
        )
        .await
        .expect_err("first pooled websocket response should surface upstream error");
    let second = backend
        .create_response(
            &request,
            request_context("req_pool_after_error", Some("chatgpt-account")),
        )
        .await
        .expect("second pooled websocket response should use a fresh connection");
    server.await.unwrap();

    let client::CodexClientError::Upstream { status, body, .. } = first_error else {
        panic!("expected upstream error from first pooled websocket response");
    };
    assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert!(body.contains("rate_limit_exceeded"));
    assert!(second.body.contains("resp_pool_after_error"));
    assert_eq!(accepted_connections.load(Ordering::SeqCst), 2);
}

#[test]
fn update_manifest_should_extract_version_and_build_number() {
    let update = codex_proxy_adapters::codex::fingerprint::parse_update_manifest(
        r#"{"version":"26.700.111","build_number":"5002"}"#,
    )
    .expect("manifest should parse");

    assert_eq!(update.app_version, "26.700.111");
    assert_eq!(update.build_number, "5002");
}

#[tokio::test]
async fn fingerprint_repository_should_upsert_auto_update_record() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("fingerprints.sqlite");
    let pool = codex_proxy_platform::storage::connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let repo = codex_proxy_adapters::codex::fingerprint::FingerprintRepository::new(pool.clone());

    repo.upsert_auto_update("26.800.1", "6001", Some("147"))
        .await
        .expect("first upsert");
    repo.upsert_auto_update("26.800.2", "6002", None)
        .await
        .expect("second upsert");

    let stored = repo
        .load_latest_auto_updated()
        .await
        .expect("load latest")
        .expect("stored fingerprint");
    let count: (i64,) =
        sqlx::query_as("select count(*) from fingerprints where id = 'auto_updated'")
            .fetch_one(&pool)
            .await
            .expect("count row");

    assert_eq!(count.0, 1);
    assert_eq!(stored.app_version, "26.800.2");
    assert_eq!(stored.build_number, "6002");
    assert_eq!(stored.chromium_version, "146");
}

#[tokio::test]
async fn fingerprint_updater_should_fetch_manifest_and_persist_history() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/desktop/update.json"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_raw(
            r#"{"version":"26.700.111","build_number":"5002"}"#,
            "application/json",
        ))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("fingerprints.sqlite");
    let pool = codex_proxy_platform::storage::connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let repo = codex_proxy_adapters::codex::fingerprint::FingerprintRepository::new(pool);
    let updater = codex_proxy_adapters::codex::fingerprint::FingerprintUpdater::new(
        reqwest::Client::new(),
        repo.clone(),
        format!("{}/desktop/update.json", server.uri()),
    );

    updater.poll_once().await.expect("poll once");

    let latest = repo
        .latest()
        .await
        .expect("latest row")
        .expect("stored history");
    assert_eq!(latest.app_version, "26.700.111");
    assert_eq!(latest.build_number, "5002");
    assert_eq!(
        latest.source,
        codex_proxy_adapters::codex::fingerprint::CODEX_DESKTOP_UPDATE_SOURCE
    );
}

#[tokio::test]
async fn update_checker_should_report_available_update_from_appcast() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/appcast.xml"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_raw(
            r#"
            <rss>
              <channel>
                <item>
                  <enclosure url="https://example.invalid/download" sparkle:shortVersionString="26.900.1" sparkle:version="7001" />
                </item>
              </channel>
            </rss>
            "#,
            "application/xml",
        ))
        .mount(&server)
        .await;

    let checker = codex_proxy_adapters::codex::fingerprint::UpdateChecker::with_client(
        None,
        reqwest::Client::new(),
        format!("{}/appcast.xml", server.uri()),
        tempfile::tempdir()
            .expect("temp dir")
            .path()
            .join("extracted-fingerprint.json"),
        "26.800.1",
        "6001",
    );

    let state = checker.check_for_update().await.expect("update state");

    assert!(state.update_available);
    assert_eq!(state.latest_version.as_deref(), Some("26.900.1"));
    assert_eq!(state.latest_build.as_deref(), Some("7001"));
}

#[tokio::test]
async fn update_checker_should_apply_available_update_to_repository() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/appcast.xml"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_raw(
            r#"
            <rss>
              <channel>
                <item>
                  <enclosure url="https://example.invalid/download" sparkle:shortVersionString="26.900.1" sparkle:version="7001" />
                </item>
              </channel>
            </rss>
            "#,
            "application/xml",
        ))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("fingerprints.sqlite");
    let pool = codex_proxy_platform::storage::connect_sqlite(&format!("sqlite://{}", db.display()))
        .await
        .expect("sqlite pool");
    let repo = codex_proxy_adapters::codex::fingerprint::FingerprintRepository::new(pool);
    let extracted_path = dir.path().join("extracted-fingerprint.json");
    std::fs::write(
        &extracted_path,
        r#"{"app_version":"26.900.1","build_number":"7001","chromium_version":"147"}"#,
    )
    .expect("write extracted fingerprint");

    let checker = codex_proxy_adapters::codex::fingerprint::UpdateChecker::with_client(
        Some(repo.clone()),
        reqwest::Client::new(),
        format!("{}/appcast.xml", server.uri()),
        extracted_path,
        "26.800.1",
        "6001",
    );

    let applied = checker
        .check_and_apply_update()
        .await
        .expect("apply update");
    let stored = repo
        .load_latest_auto_updated()
        .await
        .expect("load latest")
        .expect("stored fingerprint");

    assert!(applied);
    assert_eq!(stored.app_version, "26.900.1");
    assert_eq!(stored.build_number, "7001");
    assert_eq!(stored.chromium_version, "147");
}

#[tokio::test]
async fn codex_backend_client_should_send_desktop_headers_and_capture_response_metadata() {
    let server = wiremock::MockServer::start().await;
    let sse_body = concat!(
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"input_tokens_details\":{\"cached_tokens\":1}}}}\n",
        "\n",
    );
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/codex/responses"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer access-token",
        ))
        .and(wiremock::matchers::header(
            "chatgpt-account-id",
            "chatgpt-account",
        ))
        .and(wiremock::matchers::header("originator", "Codex Desktop"))
        .and(wiremock::matchers::header("x-client-request-id", "req_1"))
        .and(wiremock::matchers::header("x-codex-turn-state", "turn_1"))
        .and(wiremock::matchers::header("cookie", "cf_clearance=old"))
        .and(wiremock::matchers::body_json(json!({
            "model": "gpt-5.5",
            "instructions": "",
            "input": [],
            "stream": true,
            "store": false
        })))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header(
                    "set-cookie",
                    "cf_clearance=new; Domain=.chatgpt.com; Path=/",
                )
                .insert_header("x-codex-turn-state", "turn_2")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;
    let client = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        server.uri(),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "",
            Vec::new(),
        );
    request.force_http_sse = true;

    let response = client
        .create_response(
            &request,
            client::CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_1",
                turn_state: Some("turn_1"),
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: Some("cf_clearance=old"),
                installation_id: None,
                session_id: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        response.usage,
        Some(codex_proxy_core::protocol::codex::events::TokenUsage {
            input_tokens: 2,
            output_tokens: 3,
            cached_tokens: 1,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 5,
        })
    );
    assert_eq!(response.turn_state.as_deref(), Some("turn_2"));
    assert_eq!(
        response.set_cookie_headers,
        vec!["cf_clearance=new; Domain=.chatgpt.com; Path=/".to_string()]
    );
}

#[tokio::test]
async fn codex_backend_client_usage_should_use_original_auxiliary_headers() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/codex/usage"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "limit_reached": false
            }
        })))
        .mount(&server)
        .await;
    let client = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        server.uri(),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );

    let usage = client
        .fetch_usage(client::CodexRequestContext {
            access_token: "access-token",
            account_id: Some("chatgpt-account"),
            request_id: "req_aux",
            turn_state: Some("turn-state"),
            turn_metadata: Some("turn-meta"),
            beta_features: Some("feature-a"),
            include_timing_metrics: Some("true"),
            version: Some("26.318.11754"),
            codex_window_id: Some("cw_1"),
            parent_thread_id: Some("parent-1"),
            cookie_header: Some("cf_clearance=old"),
            installation_id: Some("install-1"),
            session_id: Some("session-1"),
        })
        .await
        .unwrap();

    assert_eq!(usage["rate_limit"]["limit_reached"], false);
    let requests = server.received_requests().await.unwrap();
    let headers = &requests[0].headers;
    assert_eq!(
        headers.get("accept").and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        headers
            .get("accept-encoding")
            .and_then(|value| value.to_str().ok()),
        Some("gzip, deflate")
    );
    assert!(headers.get("content-type").is_none());
    assert!(headers.get("openai-beta").is_none());
    assert!(headers.get("x-openai-internal-codex-residency").is_none());
    assert!(headers.get("x-client-request-id").is_none());
    assert_eq!(
        headers
            .get("x-codex-installation-id")
            .and_then(|value| value.to_str().ok()),
        Some("install-1")
    );
    assert!(headers.get("session_id").is_none());
}

#[tokio::test]
async fn codex_backend_client_models_should_use_original_auxiliary_headers() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/codex/models"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "models": [
                {"slug": "gpt-5.5", "title": "GPT 5.5"}
            ]
        })))
        .mount(&server)
        .await;
    let client = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        server.uri(),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );

    let models = client
        .fetch_models(client::CodexRequestContext {
            access_token: "access-token",
            account_id: Some("chatgpt-account"),
            request_id: "req_models",
            turn_state: Some("turn-state"),
            turn_metadata: None,
            beta_features: None,
            include_timing_metrics: None,
            version: None,
            codex_window_id: None,
            parent_thread_id: None,
            cookie_header: None,
            installation_id: Some("install-1"),
            session_id: Some("session-1"),
        })
        .await
        .unwrap();

    assert_eq!(models.len(), 1);
    let requests = server.received_requests().await.unwrap();
    let models_request = requests
        .iter()
        .find(|request| request.url.path() == "/codex/models")
        .unwrap();
    let headers = &models_request.headers;
    assert_eq!(
        headers.get("accept").and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert!(headers.get("content-type").is_none());
    assert!(headers.get("openai-beta").is_none());
    assert!(headers.get("x-openai-internal-codex-residency").is_none());
    assert!(headers.get("x-client-request-id").is_none());
    assert_eq!(
        headers
            .get("x-codex-installation-id")
            .and_then(|value| value.to_str().ok()),
        Some("install-1")
    );
    assert!(headers.get("session_id").is_none());
}

#[tokio::test]
async fn codex_backend_client_probe_should_return_models_endpoint_status() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/codex/models"))
        .respond_with(wiremock::ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let client = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        server.uri(),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );

    let probe = client
        .probe_models_endpoint(client::CodexRequestContext {
            access_token: "access-token",
            account_id: Some("chatgpt-account"),
            request_id: "req_probe",
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
        })
        .await
        .unwrap();

    assert_eq!(probe.status, reqwest::StatusCode::NO_CONTENT);
    assert!(probe
        .endpoint
        .ends_with("/codex/models?client_version=26.519.81530"));
}

#[tokio::test]
async fn codex_backend_client_should_cap_non_success_error_body_at_one_mib() {
    let server = wiremock::MockServer::start().await;
    let large_error_body = "x".repeat(1024 * 1024 + 17);
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/codex/responses"))
        .respond_with(wiremock::ResponseTemplate::new(500).set_body_string(large_error_body))
        .mount(&server)
        .await;
    let client = client::CodexBackendClient::new(
        client::build_reqwest_client(false).unwrap(),
        server.uri(),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "",
            Vec::new(),
        );
    request.force_http_sse = true;

    let result = client
        .create_response(
            &request,
            client::CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_large_error",
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
        .await;

    let Err(client::CodexClientError::Upstream { status, body, .. }) = result else {
        panic!("expected upstream error");
    };
    assert_eq!(status, reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body.len(), 1024 * 1024);
}

#[tokio::test]
async fn codex_backend_client_should_send_http_sse_headers_in_fingerprint_order() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        write_completed_sse_response(&mut stream).await;
        request
    });

    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "",
            Vec::new(),
        );
    request.force_http_sse = true;
    request.turn_metadata = Some("turn-meta".to_string());
    request.beta_features = Some("beta-a".to_string());
    request.include_timing_metrics = Some("true".to_string());
    request.version = Some("26.519.81530".to_string());
    request.codex_window_id = Some("cw_1".to_string());
    request.parent_thread_id = Some("parent-1".to_string());
    let client = client::CodexBackendClient::new(
        client::build_reqwest_client(true).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );

    client
        .create_response(
            &request,
            client::CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_order",
                turn_state: Some("turn-state"),
                turn_metadata: request.turn_metadata.as_deref(),
                beta_features: request.beta_features.as_deref(),
                include_timing_metrics: request.include_timing_metrics.as_deref(),
                version: request.version.as_deref(),
                codex_window_id: request.codex_window_id.as_deref(),
                parent_thread_id: request.parent_thread_id.as_deref(),
                cookie_header: Some("cf_clearance=old"),
                installation_id: Some("install-1"),
                session_id: Some("session-1"),
            },
        )
        .await
        .unwrap();

    let raw_request = server.await.unwrap();
    let header_names = read_header_names(&raw_request);
    assert_header_subsequence(
        &header_names,
        &[
            "authorization",
            "chatgpt-account-id",
            "originator",
            "user-agent",
            "sec-ch-ua",
            "sec-ch-ua-mobile",
            "sec-ch-ua-platform",
            "accept-encoding",
            "accept-language",
            "sec-fetch-site",
            "sec-fetch-mode",
            "sec-fetch-dest",
            "content-type",
            "cookie",
            "accept",
            "openai-beta",
            "x-openai-internal-codex-residency",
            "x-client-request-id",
            "x-codex-installation-id",
            "session_id",
            "x-codex-window-id",
            "x-codex-turn-state",
            "x-codex-turn-metadata",
            "x-codex-beta-features",
            "x-responsesapi-include-timing-metrics",
            "version",
            "x-codex-parent-thread-id",
        ],
    );
}

#[tokio::test]
async fn codex_backend_client_should_send_compact_headers_in_fingerprint_order() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        write_compact_json_response(&mut stream).await;
        request
    });
    let client = client::CodexBackendClient::new(
        client::build_reqwest_client(true).unwrap(),
        format!("http://{addr}"),
        codex_proxy_core::gateway::fingerprint::Fingerprint::default_for_tests(),
    );

    client
        .create_compact_response(
            &codex_proxy_core::protocol::codex::responses::CodexCompactRequest {
                model: "gpt-5.5".to_string(),
                input: Vec::new(),
                instructions: String::new(),
                tools: None,
                parallel_tool_calls: None,
                reasoning: None,
                text: None,
            },
            client::CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_compact",
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: Some("cf_clearance=old"),
                installation_id: Some("install-1"),
                session_id: None,
            },
        )
        .await
        .unwrap();

    let raw_request = server.await.unwrap();
    let header_names = read_header_names(&raw_request);
    assert_header_subsequence(
        &header_names,
        &[
            "authorization",
            "chatgpt-account-id",
            "originator",
            "user-agent",
            "sec-ch-ua",
            "sec-ch-ua-mobile",
            "sec-ch-ua-platform",
            "accept-encoding",
            "accept-language",
            "sec-fetch-site",
            "sec-fetch-mode",
            "sec-fetch-dest",
            "content-type",
            "cookie",
            "openai-beta",
            "x-openai-internal-codex-residency",
            "x-client-request-id",
            "x-codex-installation-id",
        ],
    );
}

#[tokio::test]
async fn build_reqwest_client_should_reuse_cached_connection_pool() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut first_stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut first_stream).await;
        write_empty_http_response(&mut first_stream).await;

        tokio::select! {
            request = read_http_request(&mut first_stream) => {
                write_empty_http_response(&mut first_stream).await;
                !request.is_empty()
            }
            accepted = listener.accept() => {
                let (mut second_stream, _) = accepted.unwrap();
                read_http_request(&mut second_stream).await;
                write_empty_http_response(&mut second_stream).await;
                false
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => false,
        }
    });

    let url = format!("http://{addr}/reuse");
    let first_client = client::build_reqwest_client(false).unwrap();
    first_client
        .get(&url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let second_client = client::build_reqwest_client(false).unwrap();
    second_client
        .get(&url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(server.await.unwrap());
}

fn read_header_names(request: &str) -> Vec<String> {
    request
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| {
            line.split_once(':')
                .map(|(name, _)| name.to_ascii_lowercase())
        })
        .collect()
}

fn assert_header_subsequence(actual: &[String], expected: &[&str]) {
    let mut offset = 0;
    for expected_name in expected {
        let Some(position) = actual[offset..]
            .iter()
            .position(|actual_name| actual_name == expected_name)
        else {
            panic!("missing header {expected_name}; actual order: {actual:?}");
        };
        offset += position + 1;
    }
}

async fn read_http_request(stream: &mut TcpStream) -> String {
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

fn websocket_opening_header(request: &str, name: &str) -> Option<String> {
    request.lines().find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        header_name
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

async fn write_empty_http_response(stream: &mut TcpStream) {
    stream
        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
        .await
        .unwrap();
}

async fn write_completed_sse_response(stream: &mut TcpStream) {
    let body = concat!(
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_order\",\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n",
        "\n",
    );
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}

fn request_context<'a>(
    request_id: &'a str,
    account_id: Option<&'a str>,
) -> client::CodexRequestContext<'a> {
    client::CodexRequestContext {
        access_token: "access-token",
        account_id,
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
        session_id: None,
    }
}

fn prepared_websocket_request(
    base_url: &str,
) -> codex_proxy_adapters::codex::websocket::connect::CodexWebSocketRequest {
    let request = codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
        "gpt-5.5",
        "be brief",
        Vec::new(),
    );
    codex_proxy_adapters::codex::websocket::connect::CodexWebSocketConnection::responses_create_request(
        base_url,
        "dGhlIHNhbXBsZSBub25jZQ==",
        vec![("authorization".to_string(), "Bearer access-token".to_string())],
        &request,
    )
    .expect("payload should serialize")
}

fn pooled_websocket_request(
    conversation_id: &str,
) -> codex_proxy_core::protocol::codex::responses::CodexResponsesRequest {
    let mut request =
        codex_proxy_core::protocol::codex::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.previous_response_id = Some("resp_previous".to_string());
    request.prompt_cache_key = Some(conversation_id.to_string());
    request
}

fn completed_websocket_response(
    response_id: &str,
    input_tokens: u64,
    output_tokens: u64,
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

fn assert_substrings_appear_in_order(haystack: &str, needles: &[&str]) {
    let mut cursor = 0;
    for needle in needles {
        let Some(offset) = haystack[cursor..].find(needle) else {
            panic!("expected substring {needle:?} after byte {cursor} in:\n{haystack}");
        };
        cursor += offset + needle.len();
    }
}

fn websocket_pool_config_for_tests(
    maintenance_interval: Option<Duration>,
    ping_interval: Option<Duration>,
    liveness_timeout: Option<Duration>,
) -> CodexWebSocketPoolConfig {
    CodexWebSocketPoolConfig {
        enabled: true,
        max_age: Duration::from_secs(60),
        max_per_account: 8,
        maintenance_interval,
        ping_interval,
        ping_timeout: Duration::from_secs(1),
        liveness_timeout,
    }
}

async fn write_compact_json_response(stream: &mut TcpStream) {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 15\r\n\r\n{\"id\":\"resp_1\"}",
        )
        .await
        .unwrap();
}
