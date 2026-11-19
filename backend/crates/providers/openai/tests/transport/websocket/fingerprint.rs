//! 验证 Codex WebSocket 线协议指纹

use std::io::Read as _;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use flate2::read::DeflateDecoder;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

use super::*;

#[derive(Debug)]
struct ClientFrame {
    first_byte: u8,
    masked: bool,
    mask_len: usize,
    payload: Vec<u8>,
}

#[tokio::test]
async fn official_websocket_wire_fingerprint_should_remain_aligned() {
    let mut request = encode_request_with_headers(
        json!({
            "model": "gpt-5.5",
            "instructions": "be brief",
            "input": [],
            "tools": [],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "reasoning": {"effort": "medium", "summary": "auto"},
            "store": false,
            "stream": true,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": "conversation-fingerprint",
            "generate": false,
            "client_metadata": {"request_kind": "prewarm"}
        })
        .as_object()
        .expect("request object")
        .clone(),
        &[("x-codex-beta-features", "feature-a")],
    );
    request.use_websocket = true;
    request.local_conversation_id = Some("conversation-fingerprint".to_owned());
    request.turn_metadata = Some(r#"{"kind":"fingerprint"}"#.to_owned());
    request.version = Some("26.908.40834".to_owned());
    request.codex_window_id = Some("window-fingerprint".to_owned());
    let context = CodexRequestContext {
        turn_metadata: request.turn_metadata.as_deref(),
        version: request.version.as_deref(),
        codex_window_id: request.codex_window_id.as_deref(),
        session_id: Some("session-fingerprint"),
        thread_id: Some("thread-fingerprint"),
        client_request_id: Some("request-fingerprint"),
        installation_id: Some("3b11bc82-c666-417d-b9ff-2a34fd040385"),
        ..request_context("req_wire_fingerprint", Some("account-fingerprint"))
    };
    let (opening, frames) = capture_response_create(&request, context, true).await;
    let [frame] = frames.as_slice() else {
        panic!("expected a single text frame")
    };
    let websocket_key = read_header_value(&opening, "sec-websocket-key").unwrap();
    assert_eq!(
        opening.lines().next(),
        Some("GET /backend-api/codex/responses HTTP/1.1")
    );
    // 固定身份字段沿用官方画像，锁定默认不请求专项计时的完整 opening
    assert_eq!(
        read_header_names(&opening),
        vec![
            "host",
            "connection",
            "upgrade",
            "sec-websocket-version",
            "sec-websocket-key",
            "chatgpt-account-id",
            "authorization",
            "user-agent",
            "originator",
            "openai-beta",
            "version",
            "x-codex-beta-features",
            "x-client-request-id",
            "session-id",
            "thread-id",
            "x-codex-window-id",
            "x-codex-turn-metadata",
            "x-codex-routing-hint",
            "sec-websocket-extensions",
        ]
    );
    assert_eq!(
        read_header_value(&opening, "x-responsesapi-include-timing-metrics"),
        None,
    );
    assert_eq!(
        read_header_value(&opening, "sec-websocket-extensions"),
        Some("permessage-deflate; client_max_window_bits")
    );
    assert_eq!(
        (
            read_header_value(&opening, "chatgpt-account-id"),
            read_header_value(&opening, "authorization"),
            read_header_value(&opening, "originator"),
            read_header_value(&opening, "version"),
            read_header_value(&opening, "openai-beta"),
            read_header_value(&opening, "x-codex-routing-hint"),
        ),
        (
            Some("account-fingerprint"),
            Some("Bearer access-token"),
            Some("codex_cli_rs"),
            Some("1.2.3"),
            Some("responses_websockets=2026-02-06"),
            Some("model=gpt-5.5"),
        )
    );
    let request_id = read_header_value(&opening, "x-client-request-id").unwrap();
    assert_ne!(request_id, "request-fingerprint");
    assert!(uuid::Uuid::parse_str(request_id).is_ok());
    assert_eq!(
        STANDARD
            .decode(websocket_key)
            .expect("base64 WebSocket key")
            .len(),
        16
    );

    assert_eq!(frame.first_byte, 0b1100_0001, "FIN + RSV1 + text opcode");
    assert!(frame.masked, "official client frames are masked");
    assert_eq!(frame.mask_len, 4);
    let payload = decompress_permessage_deflate(&frame.payload);
    let payload: Value = serde_json::from_slice(&payload).expect("response.create JSON");
    assert_eq!(payload["type"], "response.create");
    assert_eq!(payload["generate"], false);
    assert_eq!(
        payload
            .as_object()
            .expect("response.create object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![
            "type",
            "model",
            "instructions",
            "input",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "reasoning",
            "store",
            "stream",
            "include",
            "prompt_cache_key",
            "generate",
            "client_metadata",
        ]
    );
}

#[tokio::test]
async fn large_responses_use_continuation_frames_for_one_text_message() {
    const MIB: usize = 1024 * 1024;
    let make_request = |instructions: &str| {
        let mut request = codex_request("gpt-test", instructions, Vec::new());
        request.use_websocket = true;
        request.local_conversation_id = Some("fragment-test".to_owned());
        request
    };
    let context = request_context("fragment-test", Some("account-test"));

    // 用实际序列化的空指令测量 JSON 开销，边界包含元数据
    let (_, baseline) = capture_response_create(&make_request(""), context, false).await;
    let empty_payload = std::str::from_utf8(&baseline[0].payload).unwrap();
    let instructions_key = r#""instructions":""#;
    let start = empty_payload.find(instructions_key).unwrap() + instructions_key.len();
    // FIN 只在末帧，文本操作码和压缩标记只在首帧
    for (size, deflate, headers) in [
        (MIB, false, &[0x81][..]),
        (MIB + 1, false, &[0x01, 0x80]),
        (MIB + 1, true, &[0x41, 0x80]),
        (2 * MIB + 1, true, &[0x41, 0x00, 0x80]),
    ] {
        let mut instructions = "a".repeat(size - empty_payload.len());
        if size > 2 * MIB {
            // 分帧边界穿过一个 UTF-8 字符
            instructions.replace_range(MIB - start - 1..MIB - start + 3, "🌱");
        }
        let request = make_request(&instructions);
        let (_, frames) = capture_response_create(&request, context, deflate).await;
        let actual_headers: Vec<_> = frames.iter().map(|frame| frame.first_byte).collect();
        assert_eq!(actual_headers, headers);
        let mut payload = Vec::new();
        for (index, frame) in frames.iter().enumerate() {
            assert!(frame.masked);
            if !deflate {
                assert_eq!(frame.payload.len(), (size - index * MIB).min(MIB));
            }
            payload.extend_from_slice(&frame.payload);
        }
        if deflate {
            payload = decompress_permessage_deflate(&payload);
        }
        assert_eq!(payload.len(), size);
        let decoded: Value = serde_json::from_slice(&payload).unwrap();
        assert!(decoded["instructions"].as_str() == Some(&instructions));
    }
}

async fn capture_response_create(
    request: &CodexResponsesRequest,
    context: CodexRequestContext<'_>,
    deflate: bool,
) -> (String, Vec<ClientFrame>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fingerprint server");
    let address = listener.local_addr().expect("fingerprint server address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept fingerprint client");
        let opening = read_http_request(&mut stream).await;
        let websocket_key =
            read_header_value(&opening, "sec-websocket-key").expect("WebSocket key");
        let extension = if deflate {
            "Sec-WebSocket-Extensions: permessage-deflate\r\n"
        } else {
            ""
        };
        let response = format!(
            concat!(
                "HTTP/1.1 101 Switching Protocols\r\n",
                "Connection: Upgrade\r\n",
                "Upgrade: websocket\r\n",
                "Sec-WebSocket-Accept: {}\r\n",
                "{}",
                "\r\n"
            ),
            derive_accept_key(websocket_key.as_bytes()),
            extension
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("write upgrade response");

        let mut frames = Vec::new();
        loop {
            let frame = read_client_frame(&mut stream).await;
            let finished = frame.first_byte & 0x80 != 0;
            frames.push(frame);
            if finished {
                break;
            }
        }
        write_server_text_frame(
            &mut stream,
            &completed_websocket_response("resp_wire_fingerprint", 1, 1),
        )
        .await;
        (opening, frames)
    });

    let client = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{address}/backend-api"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::new(Duration::from_mins(1))));
    client
        .create_response(request, context)
        .await
        .expect("fingerprint WebSocket response");
    server.await.expect("fingerprint server task")
}

async fn read_client_frame(stream: &mut TcpStream) -> ClientFrame {
    let first_byte = stream.read_u8().await.expect("frame first byte");
    let second_byte = stream.read_u8().await.expect("frame second byte");
    let payload_len = match second_byte & 0x7f {
        len @ 0..=125 => u64::from(len),
        126 => u64::from(stream.read_u16().await.expect("16-bit frame length")),
        127 => stream.read_u64().await.expect("64-bit frame length"),
        _ => unreachable!(),
    };
    let masked = second_byte & 0x80 != 0;
    let mut mask = [0_u8; 4];
    if masked {
        stream.read_exact(&mut mask).await.expect("frame mask");
    }
    let mut payload = vec![0_u8; usize::try_from(payload_len).expect("frame length fits usize")];
    stream
        .read_exact(&mut payload)
        .await
        .expect("frame payload");
    if masked {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % mask.len()];
        }
    }
    ClientFrame {
        first_byte,
        masked,
        mask_len: if masked { mask.len() } else { 0 },
        payload,
    }
}

fn decompress_permessage_deflate(payload: &[u8]) -> Vec<u8> {
    let mut compressed = payload.to_vec();
    // 补回 permessage-deflate 的同步刷新尾部，再加空终止块供完整流解码器读取
    compressed.extend_from_slice(&[0, 0, 0xff, 0xff, 1, 0, 0, 0xff, 0xff]);
    let mut output = Vec::new();
    DeflateDecoder::new(compressed.as_slice())
        .read_to_end(&mut output)
        .unwrap();
    output
}

async fn write_server_text_frame(stream: &mut TcpStream, payload: &str) {
    let payload = payload.as_bytes();
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x81);
    match payload.len() {
        len @ 0..=125 => frame.push(u8::try_from(len).expect("short frame length")),
        len @ 126..=65_535 => {
            frame.push(126);
            frame.extend_from_slice(
                &u16::try_from(len)
                    .expect("16-bit frame length")
                    .to_be_bytes(),
            );
        }
        len => {
            frame.push(127);
            frame.extend_from_slice(
                &u64::try_from(len)
                    .expect("64-bit frame length")
                    .to_be_bytes(),
            );
        }
    }
    frame.extend_from_slice(payload);
    stream
        .write_all(&frame)
        .await
        .expect("write completed frame");
}
