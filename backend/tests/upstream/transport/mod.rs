use std::{
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use codex_proxy_rs::upstream::{
    transport::websocket::{
        execute_response_create_request, responses_websocket_endpoint, CodexWebSocketConnection,
        CodexWebSocketExchangeError, CodexWebSocketRequest,
    },
    transport::websocket_pool::{CodexWebSocketPool, CodexWebSocketPoolConfig},
    transport::{CodexBackendClient, CodexClientError, CodexRequestContext},
};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{
    accept_hdr_async_with_config,
    tungstenite::{
        extensions::{compression::deflate::DeflateConfig, ExtensionsConfig},
        handshake::server::{
            Callback, ErrorResponse, Request as WsRequest, Response as WsResponse,
        },
        protocol::WebSocketConfig,
        Message,
    },
};

use crate::support::assertions::assert_substrings_appear_in_order;

pub(crate) mod client;
pub(crate) mod diagnostics;
pub(crate) mod headers;
pub(crate) mod http_client;
pub(crate) mod websocket;
pub(crate) mod websocket_pool;

fn websocket_accept_config() -> WebSocketConfig {
    let mut extensions = ExtensionsConfig::default();
    extensions.permessage_deflate = Some(DeflateConfig::default());

    let mut config = WebSocketConfig::default();
    config.extensions = extensions;
    config
}

pub(crate) async fn accept_codex_test_websocket(
    stream: TcpStream,
) -> tokio_tungstenite::WebSocketStream<TcpStream> {
    accept_codex_test_websocket_with(stream, |_request, response| {
        response.headers_mut().insert(
            "sec-websocket-extensions",
            "permessage-deflate".parse().unwrap(),
        );
    })
    .await
}

struct TestWebSocketCallback<F>(F);

impl<F> Callback for TestWebSocketCallback<F>
where
    F: FnOnce(&WsRequest, &mut WsResponse) + Unpin,
{
    fn on_request(
        self,
        request: &WsRequest,
        mut response: WsResponse,
    ) -> Result<WsResponse, ErrorResponse> {
        (self.0)(request, &mut response);
        Ok(response)
    }
}

pub(crate) async fn accept_codex_test_websocket_with<F>(
    stream: TcpStream,
    callback: F,
) -> tokio_tungstenite::WebSocketStream<TcpStream>
where
    F: FnOnce(&WsRequest, &mut WsResponse) + Unpin,
{
    accept_hdr_async_with_config(
        stream,
        TestWebSocketCallback(callback),
        Some(websocket_accept_config()),
    )
    .await
    .unwrap()
}

pub(crate) fn read_header_names(request: &str) -> Vec<String> {
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

pub(crate) fn assert_header_subsequence(actual: &[String], expected: &[&str]) {
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

pub(crate) async fn read_http_request(stream: &mut TcpStream) -> String {
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

pub(crate) async fn write_empty_http_response(stream: &mut TcpStream) {
    stream
        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
        .await
        .unwrap();
}

pub(crate) async fn write_completed_sse_response(stream: &mut TcpStream) {
    let body = include_str!("../../fixtures/responses/http_sse/completed_order_usage.sse");
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

pub(crate) fn request_context<'a>(
    request_id: &'a str,
    account_id: Option<&'a str>,
) -> CodexRequestContext<'a> {
    CodexRequestContext {
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

pub(crate) fn prepared_websocket_request(base_url: &str) -> CodexWebSocketRequest {
    let request =
        codex_proxy_rs::upstream::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    CodexWebSocketConnection::responses_create_request(
        base_url,
        "dGhlIHNhbXBsZSBub25jZQ==",
        vec![(
            "authorization".to_string(),
            "Bearer access-token".to_string(),
        )],
        &request,
    )
    .expect("payload should serialize")
}

pub(crate) fn pooled_websocket_request(
    conversation_id: &str,
) -> codex_proxy_rs::upstream::protocol::responses::CodexResponsesRequest {
    let mut request =
        codex_proxy_rs::upstream::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "be brief",
            Vec::new(),
        );
    request.set_previous_response_id(Some("resp_previous".to_string()));
    request.set_prompt_cache_key(Some(conversation_id.to_string()));
    request.client_conversation_id = Some(conversation_id.to_string());
    request
}

pub(crate) fn completed_websocket_response(
    response_id: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> String {
    json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "object": "response",
            "output": [],
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "total_tokens": input_tokens + output_tokens
            }
        }
    })
    .to_string()
}

pub(crate) fn websocket_pool_config_for_tests(
    maintenance_interval: Option<Duration>,
    ping_interval: Option<Duration>,
    liveness_timeout: Option<Duration>,
) -> CodexWebSocketPoolConfig {
    CodexWebSocketPoolConfig {
        enabled: true,
        max_age: Duration::from_mins(1),
        max_per_account: 8,
        maintenance_interval,
        ping_interval,
        ping_timeout: Duration::from_secs(1),
        liveness_timeout,
        first_token_timeout: None,
    }
}

pub(crate) async fn write_compact_json_response(stream: &mut TcpStream) {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 15\r\n\r\n{\"id\":\"resp_1\"}",
        )
        .await
        .unwrap();
}
