use base64::{Engine as _, engine::general_purpose::STANDARD};
use gateway_core::operation::{GenerateRequest, ProtocolPayload};
use provider_openai::encode_generate_request;
use provider_openai::transport::websocket::CodexWebSocketConnection;
use serde_json::Value;
use tokio_tungstenite::tungstenite::http::HeaderMap as WsHeaderMap;

use super::*;

fn request_with_opaque_headers(use_websocket: bool) -> CodexResponsesRequest {
    let payload = ProtocolPayload::json_object(
        "openai",
        json!({"model": "gpt-test", "input": "hello"})
            .as_object()
            .expect("request object")
            .clone(),
    )
    .expect("OpenAI payload")
    .with_context(Map::from_iter([
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
                ["authorization", STANDARD.encode(b"Bearer client-secret")],
                ["chatgpt-account-id", STANDARD.encode(b"client-account")],
                [
                    "x-codex-installation-id",
                    STANDARD.encode(b"client-installation")
                ],
                ["x-oai-attestation", STANDARD.encode(b"client-attestation")],
                ["x-oai-is", STANDARD.encode(b"client-is")],
                ["x-oai-is-update", STANDARD.encode(b"client-is-update")]
            ]),
        ),
        ("turn_state".to_owned(), json!("typed-turn-state")),
    ]));
    let mut request = encode_generate_request(
        &GenerateRequest::from_protocol_payload(payload),
        "gpt-routed",
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

#[test]
fn websocket_connection_should_preserve_endpoint_and_header_order() {
    let connection = CodexWebSocketConnection::new(
        "wss://chatgpt.com/backend-api/codex",
        vec![
            ("authorization".to_owned(), "Bearer token".to_owned()),
            ("user-agent".to_owned(), "Codex Desktop/test".to_owned()),
        ],
    );

    assert_eq!(
        (
            connection.endpoint(),
            connection.opening_audit_snapshot().header_order,
        ),
        (
            "wss://chatgpt.com/backend-api/codex",
            vec!["authorization".to_owned(), "user-agent".to_owned()],
        )
    );
}

#[test]
fn websocket_connection_should_build_standard_headers_around_business_headers() {
    let connection = CodexWebSocketConnection::responses(
        "https://chatgpt.com/backend-api",
        "test-websocket-key",
        vec![
            (
                "chatgpt-account-id".to_owned(),
                "chatgpt-account".to_owned(),
            ),
            ("authorization".to_owned(), "Bearer access-token".to_owned()),
            ("user-agent".to_owned(), "Codex Desktop/test".to_owned()),
            (
                "openai-beta".to_owned(),
                "responses_websockets=2026-02-06".to_owned(),
            ),
        ],
    );

    assert_eq!(
        connection.opening_audit_snapshot().header_order,
        vec![
            "Host",
            "Connection",
            "Upgrade",
            "Sec-WebSocket-Version",
            "Sec-WebSocket-Key",
            "chatgpt-account-id",
            "authorization",
            "user-agent",
            "openai-beta",
            "sec-websocket-extensions",
        ]
    );
}

#[test]
fn websocket_opening_audit_should_redact_sensitive_headers() {
    let connection = CodexWebSocketConnection::new(
        "wss://chatgpt.com/backend-api/codex/responses?source=audit",
        vec![
            (
                "authorization".to_owned(),
                "Bearer access-secret".to_owned(),
            ),
            ("chatgpt-account-id".to_owned(), "acct-secret".to_owned()),
            ("user-agent".to_owned(), "Codex Desktop/test".to_owned()),
            ("x-client-request-id".to_owned(), "req-secret".to_owned()),
            (
                "x-codex-turn-metadata".to_owned(),
                "{\"secret\":true}".to_owned(),
            ),
            ("x-oai-attestation".to_owned(), "device-token".to_owned()),
        ],
    );
    let audit = connection.opening_audit_snapshot();

    assert_eq!(
        audit.header_order,
        vec![
            "authorization",
            "chatgpt-account-id",
            "user-agent",
            "x-client-request-id",
            "x-codex-turn-metadata",
            "x-oai-attestation",
        ]
    );
    assert!(
        audit
            .headers
            .iter()
            .filter(|header| header.name != "user-agent")
            .all(|header| header.value == "<redacted>")
    );
}

#[tokio::test]
async fn backend_websocket_should_forward_context_headers_and_preserve_payload_fields() {
    let received_headers = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind header server");
    let address = listener.local_addr().expect("header server address");
    let headers_for_server = Arc::clone(&received_headers);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept client");
        let mut websocket = accept_codex_test_websocket_with(stream, move |request, response| {
            response.headers_mut().insert(
                "sec-websocket-extensions",
                "permessage-deflate".parse().expect("extension header"),
            );
            *headers_for_server.lock().expect("headers lock") = request
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        value.to_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect();
        })
        .await;
        let message = websocket
            .next()
            .await
            .expect("response.create")
            .expect("valid response.create");
        let payload = serde_json::from_str::<serde_json::Value>(
            message.to_text().expect("response.create text"),
        )
        .expect("response.create JSON");
        websocket
            .send(Message::Text(
                completed_websocket_response("resp_ws_security", 1, 1).into(),
            ))
            .await
            .expect("send terminal event");
        payload
    });
    let mut request =
        codex_request_with_prompt_cache_key("gpt-test", "be brief", Vec::new(), "client-thread");
    request.use_websocket = true;
    request.responses_lite = Some("true".to_owned());
    request.memgen_request = Some("true".to_owned());
    request.set_client_metadata(Some(json!({
        "safe": "yes",
        "x-openai-subagent": "future_codex_mode",
        "ignored_non_string": 42
    })));
    let backend = CodexBackendClient::new(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("HTTP client"),
        format!("http://{address}"),
        test_wire_profile(),
    )
    .with_websocket_pool(Arc::new(CodexWebSocketPool::new(Duration::from_mins(1))));

    let response = backend
        .create_response(
            &request,
            CodexRequestContext {
                authorization: "Bearer access-token",
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
                thread_id: None,
                client_request_id: None,
                turn_id: None,
                account_selection: Default::default(),
            },
        )
        .await
        .expect("websocket response");
    let payload = server.await.expect("header server task");

    assert!(response.body.contains("resp_ws_security"));
    assert_eq!(payload["prompt_cache_key"], "client-thread");
    let metadata = payload["client_metadata"]
        .as_object()
        .expect("client metadata");
    assert_eq!(
        metadata.get("ws_request_header_x_openai_internal_codex_responses_lite"),
        Some(&json!("true"))
    );
    assert!(
        metadata
            .get("x-codex-ws-stream-request-start-ms")
            .and_then(Value::as_str)
            .is_some_and(|value| value.parse::<u128>().is_ok_and(|value| value > 0))
    );
    let headers = received_headers.lock().expect("headers lock");
    for (name, expected) in [
        ("x-client-request-id", "req_ws_security"),
        ("content-type", "application/json"),
        ("accept", "text/event-stream"),
        ("x-codex-installation-id", "install-123"),
        ("openai-beta", "responses_websockets=2026-02-06"),
        ("x-openai-internal-codex-residency", "us"),
        ("x-codex-turn-state", "turn-state"),
        ("x-codex-turn-metadata", "{\"thread_source\":\"subagent\"}"),
        ("x-codex-beta-features", "feature-a"),
        ("x-responsesapi-include-timing-metrics", "true"),
        ("version", "1.2.3"),
        ("x-codex-window-id", "cw_derived"),
        ("x-codex-parent-thread-id", "parent-456"),
        ("x-openai-subagent", "future_codex_mode"),
        ("x-openai-memgen-request", "true"),
        ("session-id", "cp_derived"),
    ] {
        assert!(
            headers
                .iter()
                .any(|(header, value)| { header == name && value == expected })
        );
    }
    for forbidden in [
        "session_id",
        "thread-id",
        "x-openai-internal-codex-responses-lite",
    ] {
        assert!(headers.iter().all(|(header, _)| header != forbidden));
    }
}

#[tokio::test]
async fn backend_http_should_restore_opaque_multivalue_header_bytes_and_lease_identity() {
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
    let request = request_with_opaque_headers(false);
    let profile = test_wire_profile();
    let profile_snapshot = profile.snapshot();
    let expected_user_agent = profile_snapshot.user_agent();
    let expected_desktop_version = profile_snapshot.desktop_version;
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
                authorization: "Bearer lease-token",
                account_id: Some("lease-account"),
                installation_id: Some("lease-installation"),
                turn_state: request.turn_state.as_deref(),
                version: Some("26.825.51511"),
                ..request_context("req_opaque_http", Some("lease-account"))
            },
        )
        .await
        .expect("opaque HTTP response");
    let raw = server.await.expect("opaque HTTP server task");

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
        ("accept", b"application/vnd.openai.future+json".as_slice()),
        (
            "content-type",
            b"application/vnd.openai.request+json".as_slice(),
        ),
        ("x-openai-internal-codex-residency", b"us".as_slice()),
        ("x-still-valid", b"after-invalid".as_slice()),
        ("authorization", b"Bearer lease-token".as_slice()),
        ("chatgpt-account-id", b"lease-account".as_slice()),
        ("x-codex-installation-id", b"lease-installation".as_slice()),
    ] {
        assert_eq!(raw_header_values(&raw, name), vec![value.to_vec()]);
    }
    // 指纹头由运行时画像生成，不透传客户端值。
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
        vec![expected_desktop_version.into_bytes()]
    );
    for dropped in [
        "openai-beta",
        "x-oai-attestation",
        "x-oai-is",
        "x-oai-is-update",
    ] {
        assert!(
            raw_header_values(&raw, dropped).is_empty(),
            "unexpected {dropped}"
        );
    }
    for secret in [
        b"client-secret".as_slice(),
        b"client-account",
        b"client-installation",
    ] {
        assert!(!raw.windows(secret.len()).any(|window| window == secret));
    }
    for omitted in ["bad header name", "x-invalid-base64"] {
        assert!(raw_header_values(&raw, omitted).is_empty());
    }
}

#[tokio::test]
async fn backend_websocket_should_drop_only_unrepresentable_opaque_header_values() {
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
                authorization: "Bearer lease-token",
                account_id: Some("lease-account"),
                installation_id: Some("lease-installation"),
                turn_state: request.turn_state.as_deref(),
                version: Some("26.825.51511"),
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

    assert_eq!(
        values("x-openai-future-mode"),
        vec![b"future-ascii".to_vec()]
    );
    assert_eq!(
        values("openai-beta"),
        vec![b"responses_websockets=2026-02-06".to_vec()]
    );
    assert_eq!(
        values("accept"),
        vec![b"application/vnd.openai.future+json".to_vec()]
    );
    assert_eq!(
        values("content-type"),
        vec![b"application/vnd.openai.request+json".to_vec()]
    );
    assert_eq!(
        values("user-agent"),
        vec![test_wire_profile().snapshot().user_agent().into_bytes()]
    );
    assert_eq!(values("originator"), vec![b"codex_cli_rs".to_vec()]);
    assert_eq!(values("version"), vec![b"1.2.3".to_vec()]);
    assert_eq!(
        values("x-openai-internal-codex-residency"),
        vec![b"us".to_vec()]
    );
    for dropped in ["x-oai-attestation", "x-oai-is", "x-oai-is-update"] {
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
    assert_eq!(
        values("x-codex-installation-id"),
        vec![b"lease-installation".to_vec()]
    );
    assert_eq!(values("x-codex-turn-state"), vec![b"turn-ascii".to_vec()]);
}

#[tokio::test]
async fn backend_http_should_send_codex_context_without_browser_headers() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind HTTP header server");
    let address = listener.local_addr().expect("HTTP header server address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept HTTP client");
        let request = read_http_request(&mut stream).await;
        write_completed_sse_response(&mut stream).await;
        request
    });
    let mut request = codex_request("gpt-test", "", Vec::new());
    request.force_http_sse = true;
    request.turn_metadata = Some("turn-meta".to_owned());
    request.beta_features = Some("beta-a".to_owned());
    request.include_timing_metrics = Some("true".to_owned());
    request.version = Some("26.707.51957".to_owned());
    request.codex_window_id = Some("cw_1".to_owned());
    request.parent_thread_id = Some("parent-1".to_owned());
    let profile = test_wire_profile();
    let profile_snapshot = profile.snapshot();
    let expected_user_agent = profile_snapshot.user_agent();
    let expected_desktop_version = profile_snapshot.desktop_version;
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
                authorization: "Bearer access-token",
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
                thread_id: None,
                client_request_id: None,
                turn_id: None,
                account_selection: Default::default(),
            },
        )
        .await
        .expect("HTTP response");

    let raw_request = server.await.expect("HTTP header server task");
    let header_names = read_header_names(&raw_request);
    assert_eq!(
        (
            read_header_value(&raw_request, "authorization"),
            read_header_value(&raw_request, "chatgpt-account-id"),
            read_header_value(&raw_request, "originator"),
            read_header_value(&raw_request, "user-agent"),
            read_header_value(&raw_request, "accept"),
            read_header_value(&raw_request, "x-openai-internal-codex-residency"),
            read_header_value(&raw_request, "x-client-request-id"),
            read_header_value(&raw_request, "x-codex-turn-state"),
        ),
        (
            Some("Bearer access-token"),
            Some("chatgpt-account"),
            Some("codex_cli_rs"),
            Some(expected_user_agent.as_str()),
            Some("text/event-stream"),
            Some("us"),
            Some("req_order"),
            Some("turn-state"),
        )
    );
    assert_eq!(
        read_header_value(&raw_request, "version"),
        Some(expected_desktop_version.as_str())
    );
    for required in [
        "authorization",
        "chatgpt-account-id",
        "originator",
        "user-agent",
        "content-type",
        "cookie",
        "accept",
        "x-openai-internal-codex-residency",
        "x-client-request-id",
        "x-codex-installation-id",
        "session-id",
        "x-codex-window-id",
        "x-codex-turn-state",
        "x-codex-turn-metadata",
        "x-codex-beta-features",
        "x-responsesapi-include-timing-metrics",
        "version",
        "x-codex-parent-thread-id",
    ] {
        assert!(header_names.iter().any(|name| name == required));
    }
    for forbidden in [
        "sec-ch-ua",
        "sec-ch-ua-mobile",
        "sec-ch-ua-platform",
        "accept-language",
        "sec-fetch-site",
        "sec-fetch-mode",
        "sec-fetch-dest",
        "openai-beta",
    ] {
        assert!(header_names.iter().all(|name| name != forbidden));
    }
}

#[tokio::test]
async fn backend_http_should_ignore_unrepresentable_protocol_headers_without_blocking_body() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind HTTP header server");
    let address = listener.local_addr().expect("HTTP header server address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept HTTP client");
        let request = read_http_request(&mut stream).await;
        write_completed_sse_response(&mut stream).await;
        request
    });
    let mut request = codex_request("gpt-test", "", Vec::new());
    request.force_http_sse = true;
    request.turn_state = Some("opaque\nturn-state".to_owned());
    request.turn_metadata = Some("opaque\nturn-metadata".to_owned());
    request.responses_lite = Some("opaque\nresponses-lite".to_owned());
    request.memgen_request = Some("opaque\nmemgen".to_owned());
    request.set_client_metadata(Some(json!({
        "x-openai-subagent": "opaque\nsubagent",
        "future": "preserved"
    })));
    let original_metadata = request.client_metadata().cloned();
    let client = CodexBackendClient::new(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("HTTP client"),
        format!("http://{address}"),
        test_wire_profile(),
    );

    client
        .create_response(
            &request,
            CodexRequestContext {
                client_request_id: Some("opaque\nclient-request-id"),
                session_id: Some("opaque\nsession"),
                thread_id: Some("opaque\nthread"),
                turn_id: Some("opaque\nturn"),
                codex_window_id: Some("opaque\nwindow"),
                parent_thread_id: Some("opaque\nparent"),
                beta_features: Some("opaque\nbeta"),
                include_timing_metrics: Some("opaque\ntiming"),
                version: Some("opaque\nversion"),
                turn_state: request.turn_state.as_deref(),
                turn_metadata: request.turn_metadata.as_deref(),
                ..request_context("req_protocol_fallback", Some("chatgpt-account"))
            },
        )
        .await
        .expect("invalid optional projections must not block the request");

    let raw_request = server.await.expect("HTTP header server task");
    assert_eq!(
        read_header_value(&raw_request, "x-client-request-id"),
        Some("req_protocol_fallback")
    );
    for omitted in [
        "session-id",
        "thread-id",
        "x-codex-turn-id",
        "x-codex-window-id",
        "x-codex-turn-state",
        "x-codex-turn-metadata",
        "x-codex-beta-features",
        "x-responsesapi-include-timing-metrics",
        "version",
        "x-codex-parent-thread-id",
        "x-openai-subagent",
        "x-openai-internal-codex-responses-lite",
        "x-openai-memgen-request",
    ] {
        assert!(read_header_value(&raw_request, omitted).is_none());
    }
    assert_eq!(request.client_metadata(), original_metadata.as_ref());
}

#[tokio::test]
async fn backend_http_should_zstd_compress_codex_responses_request_body() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind zstd HTTP server");
    let address = listener.local_addr().expect("zstd HTTP server address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept zstd HTTP client");
        let request = read_http_request_with_body(&mut stream).await;
        write_completed_sse_response(&mut stream).await;
        request
    });
    let mut request = codex_request("gpt-test", "compress me", Vec::new());
    request.force_http_sse = true;
    let client = CodexBackendClient::new(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("HTTP client"),
        format!("http://{address}"),
        test_wire_profile(),
    );

    client
        .create_response(
            &request,
            request_context("req_zstd_compress", Some("acct-zstd")),
        )
        .await
        .expect("zstd compressed response");

    let raw = server.await.expect("zstd HTTP server task");
    let separator = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP head/body separator");
    let (head, body) = raw.split_at(separator);
    let body = &body[4..];
    let head = std::str::from_utf8(head).expect("HTTP head is UTF-8");
    assert_eq!(read_header_value(head, "content-encoding"), Some("zstd"));
    assert_eq!(
        read_header_value(head, "content-type"),
        Some("application/json")
    );
    let decompressed =
        zstd::stream::decode_all(std::io::Cursor::new(body)).expect("zstd body should decode");
    let json: serde_json::Value =
        serde_json::from_slice(&decompressed).expect("decompressed body should be valid JSON");
    assert_eq!(json["model"], "gpt-test");
    assert_eq!(json["instructions"], "compress me");
}

#[tokio::test]
async fn backend_http_should_force_upstream_sse_for_non_streaming_client_request() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind non-streaming HTTP server");
    let address = listener
        .local_addr()
        .expect("non-streaming HTTP server address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept HTTP client");
        let request = read_http_request_with_body(&mut stream).await;
        write_completed_sse_response(&mut stream).await;
        request
    });
    let mut request = CodexResponsesRequest::from_body(
        json!({
            "model": "gpt-test",
            "instructions": "collect this response",
            "input": [],
            "stream": false
        })
        .as_object()
        .expect("request body")
        .clone(),
    );
    request.force_http_sse = true;
    assert!(
        !request.stream(),
        "client-facing preference remains non-streaming"
    );
    let client = CodexBackendClient::new(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("HTTP client"),
        format!("http://{address}"),
        test_wire_profile(),
    );

    client
        .create_response(
            &request,
            request_context("req_non_streaming_client", Some("acct-non-streaming")),
        )
        .await
        .expect("upstream SSE response");

    let raw = server.await.expect("non-streaming HTTP server task");
    let separator = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP head/body separator");
    let compressed_body = &raw[separator + 4..];
    let decompressed = zstd::stream::decode_all(std::io::Cursor::new(compressed_body))
        .expect("zstd body should decode");
    let json: serde_json::Value =
        serde_json::from_slice(&decompressed).expect("decompressed body should be valid JSON");
    assert_eq!(json["stream"], true);
    assert!(
        !request.stream(),
        "upstream normalization must not mutate local metadata"
    );
}

#[tokio::test]
async fn websocket_should_keep_an_exact_chain_while_new_connections_adopt_the_latest_codex_core_wire_profile()
 {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind profile server");
    let address = listener.local_addr().expect("profile server address");
    let server = tokio::spawn(async move {
        let (first_stream, _) = listener.accept().await.expect("first websocket");
        let mut first_user_agent = String::new();
        let mut first = accept_codex_test_websocket_with(first_stream, |request, _response| {
            first_user_agent = request
                .headers()
                .get("user-agent")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
        })
        .await;
        let _ = first
            .next()
            .await
            .expect("first response.create")
            .expect("valid first response.create");
        first
            .send(Message::Text(
                completed_websocket_response("resp_profile_first", 1, 1).into(),
            ))
            .await
            .expect("first response.completed");

        let _ = timeout(Duration::from_secs(2), first.next())
            .await
            .expect("exact continuation should keep the original websocket")
            .expect("continuation response.create")
            .expect("valid continuation response.create");
        first
            .send(Message::Text(
                completed_websocket_response("resp_profile_continued", 1, 1).into(),
            ))
            .await
            .expect("continuation response.completed");

        let (second_stream, _) = timeout(Duration::from_secs(2), listener.accept())
            .await
            .expect("new chain should use a new wire profile connection")
            .expect("second websocket");
        let mut second_user_agent = String::new();
        let mut second = accept_codex_test_websocket_with(second_stream, |request, _response| {
            second_user_agent = request
                .headers()
                .get("user-agent")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
        })
        .await;
        let _ = second
            .next()
            .await
            .expect("second response.create")
            .expect("valid second response.create");
        second
            .send(Message::Text(
                completed_websocket_response("resp_profile_second", 1, 1).into(),
            ))
            .await
            .expect("second response.completed");

        (first_user_agent, second_user_agent)
    });

    let profile = test_wire_profile();
    let pool = Arc::new(CodexWebSocketPool::new(Duration::from_mins(1)));
    let client = CodexBackendClient::new(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("HTTP client"),
        format!("http://{address}"),
        profile.clone(),
    )
    .with_websocket_pool(pool);
    let mut request = codex_request("gpt-test", "", Vec::new());
    request.use_websocket = true;
    request.local_conversation_id = Some("profile-rotation".to_owned());

    client
        .create_response(
            &request,
            request_context("req_profile_first", Some("acct-profile")),
        )
        .await
        .expect("first response");
    profile.update_bundled_release(&CodexBundledReleaseProfile {
        codex_version: "1.2.4".to_owned(),
        desktop_version: "1.2.4".to_owned(),
        desktop_build: "124".to_owned(),
        verified_at: Utc::now(),
    });

    let mut continuation = request.clone();
    continuation.set_previous_response_id(Some("resp_profile_first".to_owned()));
    continuation.previous_response_scope = Some(PreviousResponseScope::ConnectionLocal);
    client
        .create_response(
            &continuation,
            request_context("req_profile_continued", Some("acct-profile")),
        )
        .await
        .expect("continued response");
    client
        .create_response(
            &request,
            request_context("req_profile_second", Some("acct-profile")),
        )
        .await
        .expect("second response");

    let (first_user_agent, second_user_agent) = server.await.expect("profile server task");
    assert!(first_user_agent.contains("1.2.3"));
    assert!(second_user_agent.contains("1.2.4"));
    assert_ne!(first_user_agent, second_user_agent);
}
