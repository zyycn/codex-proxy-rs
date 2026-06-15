use std::{sync::Arc, time::Duration};

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
        http_sse_fallback_allowed, transport_for_request, CodexTransport, CodexWebSocketPool,
        CodexWebSocketPoolConfig,
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
        assert!(request.contains("Authorization: Bearer access-token"));
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
            "Sec-WebSocket-Extensions: permessage-deflate; client_max_window_bits\r\n",
            "Sec-WebSocket-Key: ",
            "Authorization: Bearer access-token\r\n",
            "ChatGPT-Account-Id: chatgpt-account\r\n",
            "originator: Codex Desktop\r\n",
            "User-Agent: Codex Desktop/26.519.81530 (darwin; arm64)\r\n",
            "sec-ch-ua: \"Chromium\";v=\"146\", \"Not:A-Brand\";v=\"24\"\r\n",
            "sec-ch-ua-mobile: ?0\r\n",
            "sec-ch-ua-platform: \"macOS\"\r\n",
            "Accept-Encoding: gzip, deflate, br, zstd\r\n",
            "Accept-Language: en-US,en;q=0.9\r\n",
            "sec-fetch-site: same-origin\r\n",
            "sec-fetch-mode: cors\r\n",
            "sec-fetch-dest: empty\r\n",
            "OpenAI-Beta: responses_websockets=2026-02-06\r\n",
            "x-openai-internal-codex-residency: us\r\n",
            "x-client-request-id: req_ws_extensions\r\n",
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
async fn websocket_one_shot_should_passthrough_connection_limit_failed_frame() {
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

    let response = client
        .create_response(&base_request(), request_context("req_ws_limit", None))
        .await
        .unwrap();

    server.await.unwrap();
    assert!(response.body.contains("event: response.failed"));
    assert!(response.body.contains("websocket_connection_limit_reached"));
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
async fn ordinary_response_should_fallback_to_http_sse_when_websocket_transport_fails() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut websocket_stream, _) = listener.accept().await.unwrap();
        let websocket_request = read_http_upgrade_request(&mut websocket_stream).await;
        assert!(websocket_request.starts_with("GET /codex/responses HTTP/1.1"));
        drop(websocket_stream);

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
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_ws_transport_fallback",
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
        .contains("websocket closed before terminal event"));

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
                "output_tokens": output_tokens
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
    let mut header = [0_u8; 2];
    stream.read_exact(&mut header).await.unwrap();
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
    payload
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
    let mut offset = 0;
    for expected in expected_headers {
        let Some(index) = raw_request[offset..].find(expected) else {
            panic!("missing websocket handshake header `{expected}` in:\n{raw_request}");
        };
        offset += index + expected.len();
    }
}
