//! 验证 HTTP 与 WebSocket 保留业务头部并过滤下游传输头

use base64::{Engine as _, engine::general_purpose::STANDARD};
use gateway_core::operation::{GenerateRequest, ProtocolPayload};
use provider_openai::encode_generate_request;
use tokio_tungstenite::tungstenite::http::HeaderMap as WsHeaderMap;

use super::super::*;

const DOWNSTREAM_TRANSPORT_HEADERS: &[(&str, &str)] = &[
    ("CF-Visitor", r#"{"scheme":"https"}"#),
    ("cf-connecting-ip", "192.0.2.1"),
    ("cf-connecting-ipv6", "2001:db8::1"),
    ("cf-pseudo-ipv4", "240.0.0.1"),
    ("cf-ray", "downstream-ray"),
    ("cf-ipcountry", "US"),
    ("cf-warp-tag-id", "downstream-warp"),
    ("cf-worker", "worker.example"),
    ("cf-ew-via", "15"),
    ("cf-future-proxy-field", "future"),
    ("x-forwarded-future-field", "future"),
    ("x-real-ip", "192.0.2.1"),
    ("true-client-ip", "192.0.2.1"),
    ("x-request-id", "proxy-id"),
    ("cdn-loop", "cloudflare; loops=1"),
    ("via", "1.1 downstream-proxy"),
    ("forwarded", "for=192.0.2.1;proto=https"),
    ("x-forwarded-for", "192.0.2.1"),
    ("x-forwarded-prefix", "/gateway"),
    ("accept-encoding", "br, gzip"),
    ("content-encoding", "downstream-encoding"),
];

const DOWNSTREAM_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Stainless-Runtime", "node"),
    ("x-stainless-future-field", "sdk-detail"),
    ("Origin", "https://client.invalid"),
    ("Referer", "https://client.invalid/workspace"),
    ("Sec-Ch-Ua", "synthetic-browser"),
    ("sec-ch-ua-platform", "synthetic-platform"),
    ("Sec-Fetch-Site", "same-origin"),
    ("session_id", "synthetic-alias"),
    ("sec-fetch-future", "future"),
    ("sec-ch-ua-future", "future"),
    ("x-grok-turn-idx", "7"),
    ("X-Grok-Model-Override", "synthetic-model"),
    ("x-grok-session-id", "synthetic-grok-session"),
    ("X-XAI-Token-Auth", "synthetic-token-auth"),
    ("x-xai-future-field", "future"),
];

fn request_with_opaque_headers(use_websocket: bool) -> CodexResponsesRequest {
    let mut context = Map::from_iter([
        (
            "opaque_request_headers".to_owned(),
            json!([
                ["x-openai-future-mode", STANDARD.encode(b"future-ascii")],
                ["x-openai-future-mode", STANDARD.encode(b"\x80\xff")],
                [
                    "accept",
                    STANDARD.encode(b"application/vnd.openai.future+json")
                ],
                [
                    "content-type",
                    STANDARD.encode(b"application/vnd.openai.request+json")
                ],
                ["user-agent", STANDARD.encode(b"Codex future-client")],
                ["originator", STANDARD.encode(b"future-originator")],
                ["version", STANDARD.encode(b"26.999.10001")],
                ["version", STANDARD.encode(b"26.999.10002")],
                ["openai-beta", STANDARD.encode(b"future_responses=v2")],
                ["openai-beta", STANDARD.encode(b"future_tools=v3")],
                [
                    "x-openai-internal-codex-residency",
                    STANDARD.encode(b"future-region")
                ],
                ["x-codex-turn-state", STANDARD.encode(b"turn-ascii")],
                ["x-codex-turn-state", STANDARD.encode(b"turn-\x80")],
                ["bad header name", STANDARD.encode(b"ignored")],
                ["x-invalid-base64", "%%%"],
                ["x-still-valid", STANDARD.encode(b"after-invalid")],
                ["traceparent", STANDARD.encode(b"synthetic-trace")],
                ["x-business-origin", STANDARD.encode(b"keep")],
                ["x-cf-business-field", STANDARD.encode(b"keep")],
                ["sec-ch-business", STANDARD.encode(b"keep")],
                ["x-stainlessbusiness", STANDARD.encode(b"keep")],
                [
                    "chatgpt-organization-id",
                    STANDARD.encode(b"unclassified-org")
                ],
                ["chatgpt-org-id", STANDARD.encode(b"unclassified-org")],
                [
                    "x-openai-organization",
                    STANDARD.encode(b"unclassified-org")
                ],
                ["x-openai-project", STANDARD.encode(b"unclassified-project")],
                ["authorization", STANDARD.encode(b"Bearer client-secret")],
                [
                    "X-OpenAI-Actor-Authorization",
                    STANDARD.encode(b"proxy-managed")
                ],
                ["chatgpt-account-id", STANDARD.encode(b"client-account")],
                [
                    "x-codex-installation-id",
                    STANDARD.encode(b"client-installation")
                ],
                [
                    "X-Codex-Installation-Id",
                    STANDARD.encode(b"second-client-installation")
                ],
                ["x-oai-attestation", STANDARD.encode(b"client-attestation")],
                ["x-oai-is", STANDARD.encode(b"client-is")],
                ["x-oai-is-update", STANDARD.encode(b"client-is-update")]
            ]),
        ),
        ("turn_state".to_owned(), json!("typed-turn-state")),
        ("session_id".to_owned(), json!("synthetic-alias")),
    ]);
    context
        .get_mut("opaque_request_headers")
        .and_then(Value::as_array_mut)
        .expect("opaque headers")
        .extend(
            DOWNSTREAM_TRANSPORT_HEADERS
                .iter()
                .chain(DOWNSTREAM_CLIENT_HEADERS)
                .map(|(name, value)| json!([name, STANDARD.encode(value.as_bytes())])),
        );
    let payload = ProtocolPayload::json_object(
        "openai",
        json!({"model": "gpt-test", "input": "hello"})
            .as_object()
            .expect("request object")
            .clone(),
    )
    .expect("OpenAI payload")
    .with_context(context);
    let mut request = encode_generate_request(
        &GenerateRequest::from_protocol_payload(payload),
        "gpt-routed",
        None,
    )
    .expect("opaque request headers");
    request.use_websocket = use_websocket;
    request.force_http_sse = !use_websocket;
    request
}

async fn read_http_request_head(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await.expect("read HTTP request");
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            request.truncate(end + 4);
            break;
        }
    }
    request
}

fn raw_header_values(request: &[u8], target: &str) -> Vec<Vec<u8>> {
    request
        .split(|byte| *byte == b'\n')
        .skip(1)
        .take_while(|line| *line != b"\r" && !line.is_empty())
        .filter_map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let colon = line.iter().position(|byte| *byte == b':')?;
            line[..colon]
                .eq_ignore_ascii_case(target.as_bytes())
                .then(|| {
                    line[colon + 1..]
                        .iter()
                        .copied()
                        .skip_while(|byte| matches!(byte, b' ' | b'\t'))
                        .collect()
                })
        })
        .collect()
}

#[tokio::test]
async fn backend_http_should_preserve_business_headers_without_downstream_transport_headers() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind opaque HTTP server");
    let address = listener.local_addr().expect("opaque HTTP server address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept opaque HTTP client");
        let request = read_http_request_head(&mut stream).await;
        write_completed_sse_response(&mut stream).await;
        request
    });
    // 不经 API 解码，直接构造协议上下文，验证 Provider 自身的过滤边界
    let request = request_with_opaque_headers(false);
    let profile = test_wire_profile();
    let profile_snapshot = profile.snapshot();
    let expected_user_agent = profile_snapshot.user_agent();
    let expected_core_version = profile_snapshot.codex_version;
    let client = CodexBackendClient::new(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("HTTP client"),
        format!("http://{address}"),
        profile,
    );

    client
        .create_response(
            &request,
            CodexRequestContext {
                trace: None,
                authorization: "Bearer lease-token",
                account_id: Some("lease-account"),
                installation_id: Some("lease-installation"),
                turn_state: request.turn_state.as_deref(),
                version: Some("26.825.51511"),
                session_id: request.client_session_id.as_deref(),
                ..request_context("req_opaque_http", Some("lease-account"))
            },
        )
        .await
        .expect("opaque HTTP response");
    let raw = server.await.expect("opaque HTTP server task");

    for &(name, _) in DOWNSTREAM_CLIENT_HEADERS {
        assert!(raw_header_values(&raw, name).is_empty(), "leaked {name}");
    }
    for &(name, _) in DOWNSTREAM_TRANSPORT_HEADERS {
        // HTTP 正文由 transport 重编码为 zstd，不能沿用下游 Content-Encoding
        if name == "content-encoding" {
            assert_eq!(raw_header_values(&raw, name), vec![b"zstd".to_vec()]);
        } else {
            assert!(
                raw_header_values(&raw, name).is_empty(),
                "unexpected {name}"
            );
        }
    }
    assert_eq!(
        raw_header_values(&raw, "x-openai-future-mode"),
        vec![b"future-ascii".to_vec(), b"\x80\xff".to_vec()]
    );
    assert!(raw_header_values(&raw, "openai-beta").is_empty());
    assert_eq!(
        raw_header_values(&raw, "x-codex-turn-state"),
        vec![b"turn-ascii".to_vec(), b"turn-\x80".to_vec()]
    );
    for (name, value) in [
        ("accept", b"text/event-stream".as_slice()),
        ("content-type", b"application/json".as_slice()),
        ("x-still-valid", b"after-invalid".as_slice()),
        ("session-id", b"synthetic-alias".as_slice()),
        ("authorization", b"Bearer lease-token".as_slice()),
        ("chatgpt-account-id", b"lease-account".as_slice()),
        ("chatgpt-organization-id", b"unclassified-org".as_slice()),
        ("chatgpt-org-id", b"unclassified-org".as_slice()),
        ("x-openai-organization", b"unclassified-org".as_slice()),
        ("x-openai-project", b"unclassified-project".as_slice()),
    ] {
        assert_eq!(raw_header_values(&raw, name), vec![value.to_vec()]);
    }
    // 指纹头由运行时画像生成，不透传客户端值
    assert_eq!(
        raw_header_values(&raw, "user-agent"),
        vec![expected_user_agent.as_bytes().to_vec()]
    );
    assert_eq!(
        raw_header_values(&raw, "originator"),
        vec![b"codex_cli_rs".to_vec()]
    );
    assert_eq!(
        raw_header_values(&raw, "version"),
        vec![expected_core_version.into_bytes()]
    );
    for dropped in [
        "openai-beta",
        "x-codex-installation-id",
        "x-openai-actor-authorization",
        "x-oai-attestation",
        "x-oai-is",
        "x-oai-is-update",
    ] {
        assert!(
            raw_header_values(&raw, dropped).is_empty(),
            "unexpected {dropped}"
        );
    }
    for name in [
        "traceparent",
        "x-business-origin",
        "x-cf-business-field",
        "sec-ch-business",
        "x-stainlessbusiness",
    ] {
        assert_eq!(raw_header_values(&raw, name).len(), 1, "missing {name}");
    }
    for secret in [b"client-secret".as_slice(), b"client-account"] {
        assert!(!raw.windows(secret.len()).any(|window| window == secret));
    }
    for omitted in ["bad header name", "x-invalid-base64"] {
        assert!(raw_header_values(&raw, omitted).is_empty());
    }
}

#[tokio::test]
async fn backend_websocket_should_preserve_business_headers_without_downstream_transport_headers() {
    let received = Arc::new(Mutex::new(WsHeaderMap::new()));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind opaque WebSocket server");
    let address = listener
        .local_addr()
        .expect("opaque WebSocket server address");
    let received_for_server = Arc::clone(&received);
    let server = tokio::spawn(async move {
        let (stream, _) = listener
            .accept()
            .await
            .expect("accept opaque WebSocket client");
        let mut websocket = accept_codex_test_websocket_with(stream, move |request, response| {
            response.headers_mut().insert(
                "sec-websocket-extensions",
                "permessage-deflate".parse().expect("extension header"),
            );
            *received_for_server.lock().expect("opaque headers lock") = request.headers().clone();
        })
        .await;
        let _ = websocket
            .next()
            .await
            .expect("opaque response.create")
            .expect("valid opaque response.create");
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_opaque_headers", 1, 1).into(),
            ))
            .await
            .expect("send opaque terminal event");
    });
    let request = request_with_opaque_headers(true);
    let client = CodexBackendClient::new(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("HTTP client"),
        format!("http://{address}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::new(Duration::from_mins(1))));

    client
        .create_response(
            &request,
            CodexRequestContext {
                trace: None,
                authorization: "Bearer lease-token",
                account_id: Some("lease-account"),
                installation_id: Some("lease-installation"),
                turn_state: request.turn_state.as_deref(),
                version: Some("26.825.51511"),
                session_id: request.client_session_id.as_deref(),
                ..request_context("req_opaque_ws", Some("lease-account"))
            },
        )
        .await
        .expect("opaque WebSocket response");
    server.await.expect("opaque WebSocket server task");
    let received = received.lock().expect("opaque headers lock");
    let values = |name: &str| {
        received
            .get_all(name)
            .iter()
            .map(|value| value.as_bytes().to_vec())
            .collect::<Vec<_>>()
    };

    for (name, _) in DOWNSTREAM_TRANSPORT_HEADERS
        .iter()
        .chain(DOWNSTREAM_CLIENT_HEADERS)
    {
        assert!(values(name).is_empty(), "unexpected {name}");
    }
    assert!(
        received
            .get("sec-websocket-extensions")
            .expect("upstream WebSocket compression negotiation")
            .to_str()
            .expect("extension value")
            .contains("permessage-deflate")
    );
    assert_eq!(
        values("x-openai-future-mode"),
        vec![b"future-ascii".to_vec()]
    );
    assert_eq!(
        values("openai-beta"),
        vec![b"responses_websockets=2026-02-06".to_vec()]
    );
    assert!(values("accept").is_empty());
    assert!(values("content-type").is_empty());
    assert_eq!(
        values("user-agent"),
        vec![test_wire_profile().snapshot().user_agent().into_bytes()]
    );
    assert_eq!(values("originator"), vec![b"codex_cli_rs".to_vec()]);
    assert_eq!(values("version"), vec![b"1.2.3".to_vec()]);
    assert!(values("x-openai-internal-codex-residency").is_empty());
    for dropped in [
        "x-codex-installation-id",
        "x-openai-actor-authorization",
        "x-oai-attestation",
        "x-oai-is",
        "x-oai-is-update",
    ] {
        assert!(values(dropped).is_empty(), "unexpected {dropped}");
    }
    assert_eq!(
        values("authorization"),
        vec![b"Bearer lease-token".to_vec()]
    );
    assert_eq!(
        values("chatgpt-account-id"),
        vec![b"lease-account".to_vec()]
    );
    for name in [
        "traceparent",
        "x-business-origin",
        "x-cf-business-field",
        "sec-ch-business",
        "x-stainlessbusiness",
    ] {
        assert_eq!(values(name).len(), 1, "missing {name}");
    }
    for (name, value) in [
        ("chatgpt-organization-id", b"unclassified-org".as_slice()),
        ("chatgpt-org-id", b"unclassified-org".as_slice()),
        ("x-openai-organization", b"unclassified-org".as_slice()),
        ("x-openai-project", b"unclassified-project".as_slice()),
    ] {
        assert_eq!(values(name), vec![value.to_vec()], "missing {name}");
    }
    assert_eq!(values("session-id"), vec![b"synthetic-alias".to_vec()]);
    assert_eq!(values("x-codex-turn-state"), vec![b"turn-ascii".to_vec()]);
}
