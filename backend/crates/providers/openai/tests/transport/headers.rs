use chrono::{TimeZone, Utc};
use provider_openai::transport::build_codex_headers;
use provider_openai::transport::profile::CodexWireProfile;
use provider_openai::transport::websocket::CodexWebSocketConnection;
use serde_json::Value;

use super::*;

#[test]
fn codex_http_headers_should_use_the_wire_profile_without_browser_metadata() {
    let profile = CodexWireProfile {
        originator: "Codex Desktop".to_owned(),
        codex_version: "0.144.2".to_owned(),
        desktop_version: "26.707.72221".to_owned(),
        desktop_build: "72221".to_owned(),
        os_type: "Mac OS".to_owned(),
        os_version: "15.7.1".to_owned(),
        arch: "arm64".to_owned(),
        terminal: "unknown".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("valid fixture time"),
    };

    let headers = build_codex_headers(
        &profile,
        "access-token",
        Some("acct-1"),
        Some("turn-1"),
        "req-1",
    )
    .expect("valid profile headers");
    let value = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());

    assert_eq!(value("authorization"), Some("Bearer access-token"));
    assert_eq!(value("chatgpt-account-id"), Some("acct-1"));
    assert_eq!(value("originator"), Some("Codex Desktop"));
    assert_eq!(
        value("user-agent"),
        Some("Codex Desktop/0.144.2 (Mac OS 15.7.1; arm64) unknown (Codex Desktop; 26.707.72221)")
    );
    assert_eq!(value("x-client-request-id"), Some("req-1"));
    assert_eq!(value("x-codex-turn-state"), Some("turn-1"));
    assert_eq!(value("accept"), Some("text/event-stream"));
    for browser_header in [
        "sec-ch-ua",
        "sec-ch-ua-mobile",
        "sec-ch-ua-platform",
        "sec-fetch-site",
        "sec-fetch-mode",
        "sec-fetch-dest",
        "accept-language",
    ] {
        assert!(
            headers.get(browser_header).is_none(),
            "sent {browser_header}"
        );
    }
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
    let mut request = CodexResponsesRequest::new_http_sse("gpt-test", "be brief", Vec::new());
    request.use_websocket = true;
    request.responses_lite = Some("true".to_owned());
    request.memgen_request = Some("true".to_owned());
    request.set_prompt_cache_key(Some("client-thread".to_owned()));
    request.set_client_metadata(Some(json!({
        "safe": "yes",
        "x-openai-subagent": "review",
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
    .with_websocket_pool(Arc::new(CodexWebSocketPool::new(8, Duration::from_mins(1))));

    let response = backend
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
                thread_id: None,
                client_request_id: None,
                turn_id: None,
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
        ("x-codex-installation-id", "install-123"),
        ("x-codex-turn-state", "turn-state"),
        ("x-codex-beta-features", "feature-a"),
        ("version", "26.318.11754"),
        ("x-codex-parent-thread-id", "parent-456"),
        ("x-openai-subagent", "review"),
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
        "content-type",
        "accept",
        "session_id",
        "x-openai-internal-codex-responses-lite",
    ] {
        assert!(headers.iter().all(|(header, _)| header != forbidden));
    }
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
    let mut request = CodexResponsesRequest::new_http_sse("gpt-test", "", Vec::new());
    request.force_http_sse = true;
    request.turn_metadata = Some("turn-meta".to_owned());
    request.beta_features = Some("beta-a".to_owned());
    request.include_timing_metrics = Some("true".to_owned());
    request.version = Some("26.707.51957".to_owned());
    request.codex_window_id = Some("cw_1".to_owned());
    request.parent_thread_id = Some("parent-1".to_owned());
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
                thread_id: None,
                client_request_id: None,
                turn_id: None,
            },
        )
        .await
        .expect("HTTP response");

    let raw_request = server.await.expect("HTTP header server task");
    let header_names = read_header_names(&raw_request);
    for required in [
        "authorization",
        "chatgpt-account-id",
        "originator",
        "user-agent",
        "content-type",
        "cookie",
        "accept",
        "openai-beta",
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
    ] {
        assert!(header_names.iter().all(|name| name != forbidden));
    }
}
