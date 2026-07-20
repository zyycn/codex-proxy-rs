use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use provider_openai::transport::{
    CodexBackendClient, CodexClientError, CodexRequestContext, MAX_CODEX_USAGE_BODY_BYTES,
    build_reqwest_client, openai_billing_breakdown,
};
use reqwest::StatusCode;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OVERSIZED_BODY_ERROR: &str = "upstream usage response exceeded the body limit";

#[test]
fn billing_breakdown_should_preserve_input_output_and_cache_components() {
    let breakdown =
        openai_billing_breakdown("gpt-5.6-sol", 100, 5, 20, 10, None).expect("known model pricing");

    assert_eq!(breakdown.input_amount().amount().scaled(), 3_500_000);
    assert_eq!(breakdown.output_amount().amount().scaled(), 1_500_000);
    assert_eq!(breakdown.cache_read_amount().amount().scaled(), 100_000);
    assert_eq!(breakdown.cache_write_amount().amount().scaled(), 625_000);
    assert_eq!(breakdown.total_amount().amount().scaled(), 5_725_000);
    assert_eq!(breakdown.service_tier(), Some("default"));
    assert_eq!(breakdown.multiplier_percent(), 100);
}

#[test]
fn billing_breakdown_should_apply_fast_and_flex_tiers_without_guessing_unknown_models() {
    let fast = openai_billing_breakdown("gpt-5.4", 1, 1, 0, 0, Some("fast")).expect("fast pricing");
    let flex = openai_billing_breakdown("gpt-5.4", 1, 1, 0, 0, Some("flex")).expect("flex pricing");

    assert_eq!(fast.total_amount().amount().scaled(), 350_000);
    assert_eq!(fast.service_tier(), Some("fast"));
    assert_eq!(fast.multiplier_percent(), 100);
    assert_eq!(flex.total_amount().amount().scaled(), 87_500);
    assert_eq!(flex.multiplier_percent(), 50);
    assert!(openai_billing_breakdown("unknown-model", 1, 1, 0, 0, None).is_none());
}

#[test]
fn billing_breakdown_should_switch_only_after_the_long_context_threshold() {
    let boundary = openai_billing_breakdown("gpt-5.4", 272_000, 0, 0, 0, None)
        .expect("short-context boundary");
    let long =
        openai_billing_breakdown("gpt-5.4", 272_001, 0, 0, 0, None).expect("long-context pricing");

    assert_eq!(
        boundary.input_price_per_million().amount().scaled(),
        25_000_000_000
    );
    assert_eq!(
        long.input_price_per_million().amount().scaled(),
        50_000_000_000
    );
    assert!(openai_billing_breakdown("gpt-5.4", 1, 0, 2, 0, None).is_none());
}

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
            access_token: "oauth-access",
            account_id: Some("acct_123"),
            request_id: "req_usage_headers",
            turn_state: Some("turn-state"),
            turn_metadata: Some("turn-meta"),
            beta_features: Some("feature-a"),
            include_timing_metrics: Some("true"),
            version: Some("26.318.11754"),
            codex_window_id: Some("cw_1"),
            parent_thread_id: Some("parent-1"),
            cookie_header: Some("session=old"),
            installation_id: Some("install-1"),
            session_id: Some("session-1"),
            thread_id: Some("thread-1"),
            client_request_id: Some("client-request-1"),
            turn_id: Some("turn-1"),
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
        ("originator", "codex_cli_rs"),
        ("accept", "application/json"),
        ("cookie", "session=old"),
    ] {
        assert_eq!(
            headers.get(name).and_then(|value| value.to_str().ok()),
            Some(expected),
            "unexpected {name} header"
        );
    }
    assert!(headers.get("user-agent").is_some());
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
            "originator",
            "user-agent",
        ])
    );
    for forbidden in [
        "content-type",
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
async fn chunked_body_over_limit_should_be_rejected_without_content_length() {
    let (base_url, server) = spawn_chunked_server(usage_body(MAX_CODEX_USAGE_BODY_BYTES + 1));

    let error = client(&base_url)
        .fetch_usage(context())
        .await
        .expect_err("over-limit chunked body");
    server.join().expect("chunked server thread");

    assert_oversized_error(error, StatusCode::BAD_GATEWAY, None);
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
        originator: "codex_cli_rs".to_owned(),
        codex_version: "0.144.0".to_owned(),
        desktop_version: "1.0.0".to_owned(),
        desktop_build: "1".to_owned(),
        os_type: "linux".to_owned(),
        os_version: "6.8".to_owned(),
        arch: "x86_64".to_owned(),
        terminal: "xterm".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("valid fixture time"),
    })
}

fn context() -> CodexRequestContext<'static> {
    CodexRequestContext {
        access_token: "oauth-access",
        account_id: Some("acct_123"),
        request_id: "req_usage_limit",
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
