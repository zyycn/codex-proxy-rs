//! 验证 Codex usage 查询、响应体限制和错误元数据

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use provider_openai::profile::{CodexWireProfile, CodexWireProfileState};
use provider_openai::transport::{
    CodexBackendClient, CodexClientError, CodexRequestContext, MAX_CODEX_USAGE_BODY_BYTES,
    build_reqwest_client,
};
use reqwest::StatusCode;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OVERSIZED_BODY_ERROR: &str = "upstream usage response exceeded the body limit";

#[tokio::test]
async fn exact_limit_success_body_should_be_accepted() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(usage_body(MAX_CODEX_USAGE_BODY_BYTES), "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let usage = client(&server.uri())
        .fetch_usage(context())
        .await
        .expect("exact-limit usage body");

    assert!(usage["rate_limit"].is_object());
}

#[tokio::test]
async fn fetch_should_use_wham_usage_headers_only() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "rate_limit": { "limit_reached": false }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let usage = client(&server.uri())
        .fetch_usage(CodexRequestContext {
            response_control: None,
            trace: None,
            authorization: "Bearer oauth-access",
            account_id: Some("acct_123"),
            request_id: "req_usage_headers",
            turn_state: Some("turn-state"),
            turn_metadata: Some("turn-meta"),
            version: Some("26.318.11754"),
            codex_window_id: Some("cw_1"),
            parent_thread_id: Some("parent-1"),
            cookie_header: Some("session=old"),
            installation_id: Some("install-1"),
            session_id: Some("session-1"),
            thread_id: Some("thread-1"),
            client_request_id: Some("client-request-1"),
            account_selection: Default::default(),
        })
        .await
        .expect("usage response");

    assert_eq!(usage["rate_limit"]["limit_reached"], false);
    let requests = server
        .received_requests()
        .await
        .expect("received usage request");
    let headers = &requests[0].headers;
    for (name, expected) in [
        ("authorization", "Bearer oauth-access"),
        ("chatgpt-account-id", "acct_123"),
        ("accept", "*/*"),
        ("cookie", "session=old"),
    ] {
        assert_eq!(
            headers.get(name).and_then(|value| value.to_str().ok()),
            Some(expected),
            "unexpected {name} header"
        );
    }
    assert_eq!(
        headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok()),
        Some("codex_cli_rs/0.144.0 (linux 6.8; x86_64) xterm (codex_cli_rs; 1.0.0)")
    );
    let quota_header_names = headers
        .keys()
        .map(|name| name.as_str())
        .filter(|name| !matches!(*name, "accept-encoding" | "host"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        quota_header_names,
        BTreeSet::from([
            "accept",
            "authorization",
            "chatgpt-account-id",
            "cookie",
            "user-agent",
        ])
    );
    for forbidden in [
        "content-type",
        "originator",
        "sec-ch-ua",
        "x-openai-internal-codex-residency",
        "x-client-request-id",
        "x-codex-installation-id",
        "session_id",
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
    ] {
        assert!(
            headers.get(forbidden).is_none(),
            "unexpected {forbidden} header"
        );
    }
}

#[tokio::test]
async fn usage_not_found_should_preserve_status_without_a_nonofficial_fallback() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server.uri())
        .with_official_base_url(server.uri())
        .fetch_usage(context())
        .await
        .expect_err("usage 404");
    let CodexClientError::Upstream { status, .. } = error else {
        panic!("expected upstream status");
    };
    assert_eq!(status, StatusCode::NOT_FOUND);

    let requests = server.received_requests().await.expect("usage requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/api/codex/usage");
}

#[tokio::test]
async fn content_length_over_limit_success_body_should_be_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            usage_body(MAX_CODEX_USAGE_BODY_BYTES + 1),
            "application/json",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server.uri())
        .fetch_usage(context())
        .await
        .expect_err("over-limit success body");

    assert_oversized_error(error, StatusCode::BAD_GATEWAY, None);
}

#[tokio::test]
async fn content_length_over_limit_error_body_should_keep_only_safe_metadata() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "7")
                .set_body_bytes(vec![b's'; MAX_CODEX_USAGE_BODY_BYTES + 1]),
        )
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server.uri())
        .fetch_usage(context())
        .await
        .expect_err("over-limit error body");

    assert_oversized_error(error, StatusCode::TOO_MANY_REQUESTS, Some(7));
}

#[tokio::test]
async fn retry_after_http_date_should_be_converted_to_remaining_seconds() {
    let server = MockServer::start().await;
    let retry_at = (Utc::now() + chrono::TimeDelta::seconds(90))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", retry_at.as_str())
                .set_body_raw(
                    r#"{"error":{"message":"rate limited"}}"#,
                    "application/json",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server.uri())
        .fetch_usage(context())
        .await
        .expect_err("http-date rate limit");

    let CodexClientError::Upstream {
        status,
        retry_after_seconds,
        ..
    } = error
    else {
        panic!("expected an upstream rate-limit error");
    };
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let seconds = retry_after_seconds.expect("http-date retry-after should yield seconds");
    assert!(
        (1..=90).contains(&seconds),
        "expected remaining seconds within 90s, got {seconds}"
    );
}

#[tokio::test]
async fn retry_after_http_date_in_the_past_should_preserve_zero_delay() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "Mon, 01 Jan 2001 00:00:00 GMT")
                .set_body_raw(
                    r#"{"error":{"message":"rate limited"}}"#,
                    "application/json",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server.uri())
        .fetch_usage(context())
        .await
        .expect_err("http-date rate limit");

    let CodexClientError::Upstream {
        retry_after_seconds,
        ..
    } = error
    else {
        panic!("expected an upstream rate-limit error");
    };
    assert_eq!(retry_after_seconds, Some(0));
}

#[tokio::test]
async fn chunked_body_over_limit_should_be_rejected_without_content_length() {
    let (base_url, server) = spawn_chunked_server(usage_body(MAX_CODEX_USAGE_BODY_BYTES + 1));

    let error = client(&base_url)
        .fetch_usage(context())
        .await
        .expect_err("over-limit chunked body");
    server.join().expect("chunked server thread");

    assert_oversized_error(error, StatusCode::BAD_GATEWAY, None);
}

#[tokio::test]
async fn fetch_usage_should_fallback_to_official_endpoint_when_custom_upstream_returns_404() {
    let custom_server = MockServer::start().await;
    let official_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(StatusCode::NOT_FOUND))
        .expect(1)
        .mount(&custom_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(
            ResponseTemplate::new(StatusCode::OK)
                .set_body_json(serde_json::json!({"rate_limit": {"limit": 100}})),
        )
        .expect(1)
        .mount(&official_server)
        .await;

    let test_client = client(&custom_server.uri())
        .with_official_base_url(format!("{}/backend-api", official_server.uri()));
    let usage = test_client
        .fetch_usage(context())
        .await
        .expect("usage after fallback");
    assert_eq!(usage["rate_limit"]["limit"], 100);
}

#[tokio::test]
async fn fetch_usage_should_not_fallback_when_error_is_not_404() {
    let custom_server = MockServer::start().await;
    let official_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(StatusCode::UNAUTHORIZED))
        .expect(1)
        .mount(&custom_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(StatusCode::OK))
        .expect(0)
        .mount(&official_server)
        .await;

    let test_client = client(&custom_server.uri()).with_official_base_url(official_server.uri());
    let error = test_client
        .fetch_usage(context())
        .await
        .expect_err("should return 401");
    let CodexClientError::Upstream { status, .. } = error else {
        panic!("expected upstream error");
    };
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

fn assert_oversized_error(
    error: CodexClientError,
    expected_status: StatusCode,
    expected_retry_after: Option<u64>,
) {
    let CodexClientError::Upstream {
        status,
        retry_after_seconds,
        body,
        ..
    } = error
    else {
        panic!("expected a bounded upstream error");
    };
    assert_eq!(status, expected_status);
    assert_eq!(retry_after_seconds, expected_retry_after);
    assert_eq!(body, OVERSIZED_BODY_ERROR);
}

fn usage_body(size: usize) -> Vec<u8> {
    const PREFIX: &[u8] = br#"{"rate_limit":{},"padding":""#;
    const SUFFIX: &[u8] = br#""}"#;
    assert!(size >= PREFIX.len() + SUFFIX.len());
    let mut body = Vec::with_capacity(size);
    body.extend_from_slice(PREFIX);
    body.resize(size - SUFFIX.len(), b'p');
    body.extend_from_slice(SUFFIX);
    assert_eq!(body.len(), size);
    body
}

fn spawn_chunked_server(body: Vec<u8>) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind chunked test server");
    let address = listener.local_addr().expect("chunked server address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept usage request");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set request timeout");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4 * 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("read usage request");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
            )
            .expect("write chunked headers");
        for chunk in body.chunks(8 * 1024) {
            if write!(stream, "{:x}\r\n", chunk.len()).is_err()
                || stream.write_all(chunk).is_err()
                || stream.write_all(b"\r\n").is_err()
            {
                return;
            }
        }
        let _ = stream.write_all(b"0\r\n\r\n");
    });
    (format!("http://{address}"), server)
}

fn client(base_url: &str) -> CodexBackendClient {
    CodexBackendClient::new(
        build_reqwest_client().expect("build Codex client"),
        base_url,
        profile(),
    )
}

fn profile() -> CodexWireProfileState {
    CodexWireProfileState::new(CodexWireProfile {
        client_kind: provider_openai::profile::selection::ClientKind::Desktop,
        originator: "codex_cli_rs".to_owned(),
        codex_version: "0.144.0".to_owned(),
        desktop_version: "1.0.0".to_owned(),
        desktop_build: "1".to_owned(),
        os_type: "linux".to_owned(),
        os_version: "6.8".to_owned(),
        arch: "x86_64".to_owned(),
        terminal: "xterm".to_owned(),
        exact_user_agent: None,
        installation_seed: None,
        residency: None,
        account_routing_override: None,
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("valid fixture time"),
    })
}

fn context() -> CodexRequestContext<'static> {
    CodexRequestContext {
        response_control: None,
        trace: None,
        authorization: "Bearer oauth-access",
        account_id: Some("acct_123"),
        request_id: "req_usage_limit",
        turn_state: None,
        turn_metadata: None,
        version: None,
        codex_window_id: None,
        parent_thread_id: None,
        cookie_header: None,
        installation_id: None,
        session_id: None,
        thread_id: None,
        client_request_id: None,
        account_selection: Default::default(),
    }
}
