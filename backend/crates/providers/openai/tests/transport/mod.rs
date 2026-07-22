use std::{
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use chrono::Utc;
use futures::{SinkExt, StreamExt};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use provider_openai::transport::protocol::responses::{
    CodexResponsesRequest, PreviousResponseScope,
};
use provider_openai::transport::websocket::{
    CodexWebSocketConnection, CodexWebSocketExchangeError, CodexWebSocketRequest,
    execute_response_create_request, responses_websocket_endpoint,
};
use provider_openai::transport::{
    CodexBackendClient, CodexBackendTransport, CodexClientError, CodexRequestContext,
    CodexTransportDecision, CodexWebSocketPool, CodexWebSocketPoolConfig,
};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::{
    accept_hdr_async_with_config,
    tungstenite::{
        Message,
        extensions::{ExtensionsConfig, compression::deflate::DeflateConfig},
        handshake::server::{
            Callback, ErrorResponse, Request as WebSocketRequest, Response as WebSocketResponse,
        },
        protocol::WebSocketConfig,
    },
};

mod canonical;
mod catalog;
mod client;
mod diagnostics;
mod endpoints;
mod headers;
mod http_client;
mod latency;
mod profile;
mod protocol;
mod request;
mod usage;
mod websocket;
mod websocket_pool;

fn websocket_accept_config() -> WebSocketConfig {
    let mut extensions = ExtensionsConfig::default();
    extensions.permessage_deflate = Some(DeflateConfig::default());
    let mut config = WebSocketConfig::default();
    config.extensions = extensions;
    config
}

struct TestWebSocketCallback<F>(F);

impl<F> Callback for TestWebSocketCallback<F>
where
    F: FnOnce(&WebSocketRequest, &mut WebSocketResponse) + Unpin,
{
    fn on_request(
        self,
        request: &WebSocketRequest,
        mut response: WebSocketResponse,
    ) -> Result<WebSocketResponse, ErrorResponse> {
        (self.0)(request, &mut response);
        Ok(response)
    }
}

pub(crate) async fn accept_codex_test_websocket(
    stream: TcpStream,
) -> tokio_tungstenite::WebSocketStream<TcpStream> {
    accept_codex_test_websocket_with(stream, |_request, response| {
        response.headers_mut().insert(
            "sec-websocket-extensions",
            "permessage-deflate"
                .parse()
                .expect("valid extension header"),
        );
    })
    .await
}

async fn accept_codex_test_websocket_with<F>(
    stream: TcpStream,
    callback: F,
) -> tokio_tungstenite::WebSocketStream<TcpStream>
where
    F: FnOnce(&WebSocketRequest, &mut WebSocketResponse) + Unpin,
{
    accept_hdr_async_with_config(
        stream,
        TestWebSocketCallback(callback),
        Some(websocket_accept_config()),
    )
    .await
    .expect("accept test WebSocket")
}

fn test_wire_profile() -> CodexWireProfileState {
    CodexWireProfileState::new(CodexWireProfile {
        originator: "codex_cli_rs".to_owned(),
        codex_version: "1.2.3".to_owned(),
        desktop_version: "1.2.3".to_owned(),
        desktop_build: "123".to_owned(),
        os_type: "linux".to_owned(),
        os_version: "6.8".to_owned(),
        arch: "x86_64".to_owned(),
        terminal: "transport-test".to_owned(),
        verified_at: Utc::now(),
    })
}

fn request_context<'a>(
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
        thread_id: None,
        client_request_id: None,
        turn_id: None,
    }
}

fn codex_request(
    model: impl Into<String>,
    instructions: impl Into<String>,
    input: Vec<Value>,
) -> CodexResponsesRequest {
    CodexResponsesRequest::from_body(codex_request_body(model, instructions, input))
}

fn codex_request_with_prompt_cache_key(
    model: impl Into<String>,
    instructions: impl Into<String>,
    input: Vec<Value>,
    prompt_cache_key: impl Into<String>,
) -> CodexResponsesRequest {
    let mut body = codex_request_body(model, instructions, input);
    body.insert(
        "prompt_cache_key".to_owned(),
        Value::String(prompt_cache_key.into()),
    );
    CodexResponsesRequest::from_body(body)
}

fn codex_request_body(
    model: impl Into<String>,
    instructions: impl Into<String>,
    input: Vec<Value>,
) -> Map<String, Value> {
    Map::from_iter([
        ("model".to_owned(), Value::String(model.into())),
        (
            "instructions".to_owned(),
            Value::String(instructions.into()),
        ),
        ("input".to_owned(), Value::Array(input)),
    ])
}

fn prepared_websocket_request(base_url: &str) -> CodexWebSocketRequest {
    let request = codex_request("gpt-test", "be brief", Vec::new());
    CodexWebSocketConnection::responses_create_request(
        base_url,
        "dGhlIHNhbXBsZSBub25jZQ==",
        vec![("authorization".to_owned(), "Bearer access-token".to_owned())],
        &request,
    )
    .expect("prepare WebSocket request")
}

fn pooled_websocket_request(conversation_id: &str) -> CodexResponsesRequest {
    let mut request =
        codex_request_with_prompt_cache_key("gpt-test", "be brief", Vec::new(), conversation_id);
    request.set_previous_response_id(Some("resp_previous".to_owned()));
    request.previous_response_scope = Some(PreviousResponseScope::Persisted);
    request.local_conversation_id = Some(conversation_id.to_owned());
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
            "model": "gpt-test",
            "status": "completed",
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

fn read_header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}

async fn read_http_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await.expect("read HTTP request");
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(request).expect("HTTP request is UTF-8")
}

async fn write_completed_sse_response(stream: &mut TcpStream) {
    let body = concat!(
        "event: response.created\n",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_http_fallback\",\"model\":\"gpt-test\"}}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_http_fallback\",\"model\":\"gpt-test\",\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1,\"total_tokens\":4}}\n\n",
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("write HTTP response");
}

async fn write_empty_http_response(stream: &mut TcpStream) {
    stream
        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
        .await
        .expect("write empty HTTP response");
}

fn websocket_pool_config_for_tests(
    maintenance_interval: Option<Duration>,
    ping_interval: Option<Duration>,
    liveness_timeout: Option<Duration>,
) -> CodexWebSocketPoolConfig {
    CodexWebSocketPoolConfig {
        enabled: true,
        max_age: Duration::from_mins(1),
        max_per_account: 8,
        max_total: 64,
        max_connecting: 16,
        maintenance_interval,
        ping_interval,
        ping_timeout: Duration::from_secs(1),
        liveness_timeout,
        initial_event_timeout: None,
    }
}

fn assert_substrings_appear_in_order(haystack: &str, needles: &[&str]) {
    let mut offset = 0;
    for needle in needles {
        let relative = haystack[offset..]
            .find(needle)
            .unwrap_or_else(|| panic!("missing ordered substring: {needle}"));
        offset += relative + needle.len();
    }
}
