use std::{
    io::Write,
    process::Command,
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

#[path = "codex/codex_fingerprint.rs"]
mod codex_fingerprint;
#[path = "codex/codex_headers.rs"]
mod codex_headers;
#[path = "codex/codex_http_client.rs"]
mod codex_http_client;
#[path = "codex/codex_websocket.rs"]
mod codex_websocket;
#[path = "codex/codex_websocket_pool.rs"]
mod codex_websocket_pool;

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
