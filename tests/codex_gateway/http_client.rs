use std::time::Duration;

use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use wiremock::{
    matchers::{body_json, header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::{
    codex::gateway::fingerprint::model::Fingerprint,
    codex::gateway::transport::{
        http_client::{
            build_reqwest_client, CodexBackendClient, CodexClientError, CodexRequestContext,
        },
        types::{CodexCompactRequest, CodexResponsesRequest},
        usage_events::TokenUsage,
    },
};

#[tokio::test]
async fn codex_backend_client_should_send_desktop_headers_and_capture_response_metadata() {
    let server = MockServer::start().await;
    let sse_body = concat!(
        "event: response.completed\n",
        "data: {\"response\":{\"id\":\"resp_1\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"input_tokens_details\":{\"cached_tokens\":1}}}}\n",
        "\n",
    );
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer access-token"))
        .and(header("chatgpt-account-id", "chatgpt-account"))
        .and(header("originator", "Codex Desktop"))
        .and(header("x-client-request-id", "req_1"))
        .and(header("x-codex-turn-state", "turn_1"))
        .and(header("cookie", "cf_clearance=old"))
        .and(body_json(json!({
            "model": "gpt-5.5",
            "instructions": "",
            "input": [],
            "stream": true,
            "store": false
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header(
                    "set-cookie",
                    "cf_clearance=new; Domain=.chatgpt.com; Path=/",
                )
                .insert_header("x-codex-turn-state", "turn_2")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        server.uri(),
        Fingerprint::default_for_tests(),
    );

    let response = client
        .create_response(
            &CodexResponsesRequest::new_http_sse("gpt-5.5", "", Vec::new()),
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_1",
                turn_state: Some("turn_1"),
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: Some("cf_clearance=old"),
                installation_id: None,
                session_id: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        response.usage,
        Some(TokenUsage {
            input_tokens: 2,
            output_tokens: 3,
            cached_tokens: 1,
            image_input_tokens: 0,
            image_output_tokens: 0,
            total_tokens: 5,
        })
    );
    assert_eq!(response.turn_state.as_deref(), Some("turn_2"));
    assert_eq!(
        response.set_cookie_headers,
        vec!["cf_clearance=new; Domain=.chatgpt.com; Path=/".to_string()]
    );
}

#[tokio::test]
async fn codex_backend_client_usage_should_use_original_auxiliary_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "limit_reached": false
            }
        })))
        .mount(&server)
        .await;
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        server.uri(),
        Fingerprint::default_for_tests(),
    );

    let usage = client
        .fetch_usage(CodexRequestContext {
            access_token: "access-token",
            account_id: Some("chatgpt-account"),
            request_id: "req_aux",
            turn_state: Some("turn-state"),
            turn_metadata: Some("turn-meta"),
            beta_features: Some("feature-a"),
            include_timing_metrics: Some("true"),
            version: Some("26.318.11754"),
            codex_window_id: Some("cw_1"),
            parent_thread_id: Some("parent-1"),
            cookie_header: Some("cf_clearance=old"),
            installation_id: Some("install-1"),
            session_id: Some("session-1"),
        })
        .await
        .unwrap();

    assert_eq!(usage["rate_limit"]["limit_reached"], false);
    let requests = server.received_requests().await.unwrap();
    let headers = &requests[0].headers;
    assert_eq!(
        headers.get("accept").and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        headers
            .get("accept-encoding")
            .and_then(|value| value.to_str().ok()),
        Some("gzip, deflate")
    );
    assert!(headers.get("content-type").is_none());
    assert!(headers.get("openai-beta").is_none());
    assert!(headers.get("x-openai-internal-codex-residency").is_none());
    assert!(headers.get("x-client-request-id").is_none());
    assert!(headers.get("x-codex-installation-id").is_none());
    assert!(headers.get("session_id").is_none());
}

#[tokio::test]
async fn codex_backend_client_models_should_use_original_auxiliary_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [
                {"slug": "gpt-5.5", "title": "GPT 5.5"}
            ]
        })))
        .mount(&server)
        .await;
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        server.uri(),
        Fingerprint::default_for_tests(),
    );

    let models = client
        .fetch_models(CodexRequestContext {
            access_token: "access-token",
            account_id: Some("chatgpt-account"),
            request_id: "req_models",
            turn_state: Some("turn-state"),
            turn_metadata: None,
            beta_features: None,
            include_timing_metrics: None,
            version: None,
            codex_window_id: None,
            parent_thread_id: None,
            cookie_header: None,
            installation_id: Some("install-1"),
            session_id: Some("session-1"),
        })
        .await
        .unwrap();

    assert_eq!(models.len(), 1);
    let requests = server.received_requests().await.unwrap();
    let models_request = requests
        .iter()
        .find(|request| request.url.path() == "/codex/models")
        .unwrap();
    let headers = &models_request.headers;
    assert_eq!(
        headers.get("accept").and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert!(headers.get("content-type").is_none());
    assert!(headers.get("openai-beta").is_none());
    assert!(headers.get("x-openai-internal-codex-residency").is_none());
    assert!(headers.get("x-client-request-id").is_none());
    assert!(headers.get("x-codex-installation-id").is_none());
    assert!(headers.get("session_id").is_none());
}

#[tokio::test]
async fn codex_backend_client_should_cap_non_success_error_body_at_one_mib() {
    let server = MockServer::start().await;
    let large_error_body = "x".repeat(1024 * 1024 + 17);
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(500).set_body_string(large_error_body))
        .mount(&server)
        .await;
    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        server.uri(),
        Fingerprint::default_for_tests(),
    );
    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "", Vec::new());
    request.force_http_sse = true;

    let result = client
        .create_response(
            &request,
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_large_error",
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
        .await;

    let Err(CodexClientError::Upstream { status, body, .. }) = result else {
        panic!("expected upstream error");
    };
    assert_eq!(status, reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body.len(), 1024 * 1024);
}

#[tokio::test]
async fn codex_backend_client_should_send_http_sse_headers_in_fingerprint_order() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        write_completed_sse_response(&mut stream).await;
        request
    });

    let mut request = CodexResponsesRequest::new_http_sse("gpt-5.5", "", Vec::new());
    request.force_http_sse = true;
    request.turn_metadata = Some("turn-meta".to_string());
    request.beta_features = Some("beta-a".to_string());
    request.include_timing_metrics = Some("true".to_string());
    request.version = Some("26.519.81530".to_string());
    request.codex_window_id = Some("cw_1".to_string());
    request.parent_thread_id = Some("parent-1".to_string());
    let client = CodexBackendClient::new(
        build_reqwest_client(true).unwrap(),
        format!("http://{addr}"),
        Fingerprint::default_for_tests(),
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
            },
        )
        .await
        .unwrap();

    let raw_request = server.await.unwrap();
    let header_names = header_names(&raw_request);
    assert_header_subsequence(
        &header_names,
        &[
            "authorization",
            "chatgpt-account-id",
            "originator",
            "user-agent",
            "sec-ch-ua",
            "sec-ch-ua-mobile",
            "sec-ch-ua-platform",
            "accept-encoding",
            "accept-language",
            "sec-fetch-site",
            "sec-fetch-mode",
            "sec-fetch-dest",
            "content-type",
            "cookie",
            "accept",
            "openai-beta",
            "x-openai-internal-codex-residency",
            "x-client-request-id",
            "x-codex-installation-id",
            "session_id",
            "x-codex-window-id",
            "x-codex-turn-state",
            "x-codex-turn-metadata",
            "x-codex-beta-features",
            "x-responsesapi-include-timing-metrics",
            "version",
            "x-codex-parent-thread-id",
        ],
    );
}

#[tokio::test]
async fn codex_backend_client_should_send_compact_headers_in_fingerprint_order() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_http_request(&mut stream).await;
        write_compact_json_response(&mut stream).await;
        request
    });
    let client = CodexBackendClient::new(
        build_reqwest_client(true).unwrap(),
        format!("http://{addr}"),
        Fingerprint::default_for_tests(),
    );

    client
        .create_compact_response(
            &CodexCompactRequest {
                model: "gpt-5.5".to_string(),
                input: Vec::new(),
                instructions: String::new(),
                tools: None,
                parallel_tool_calls: None,
                reasoning: None,
                text: None,
            },
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_compact",
                turn_state: None,
                turn_metadata: None,
                beta_features: None,
                include_timing_metrics: None,
                version: None,
                codex_window_id: None,
                parent_thread_id: None,
                cookie_header: Some("cf_clearance=old"),
                installation_id: Some("install-1"),
                session_id: None,
            },
        )
        .await
        .unwrap();

    let raw_request = server.await.unwrap();
    let header_names = header_names(&raw_request);
    assert_header_subsequence(
        &header_names,
        &[
            "authorization",
            "chatgpt-account-id",
            "originator",
            "user-agent",
            "sec-ch-ua",
            "sec-ch-ua-mobile",
            "sec-ch-ua-platform",
            "accept-encoding",
            "accept-language",
            "sec-fetch-site",
            "sec-fetch-mode",
            "sec-fetch-dest",
            "content-type",
            "cookie",
            "openai-beta",
            "x-openai-internal-codex-residency",
            "x-client-request-id",
            "x-codex-installation-id",
        ],
    );
}

#[tokio::test]
async fn build_reqwest_client_should_reuse_cached_connection_pool() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut first_stream, _) = listener.accept().await.unwrap();
        read_http_request(&mut first_stream).await;
        write_empty_http_response(&mut first_stream).await;

        tokio::select! {
            request = read_http_request(&mut first_stream) => {
                write_empty_http_response(&mut first_stream).await;
                !request.is_empty()
            }
            accepted = listener.accept() => {
                let (mut second_stream, _) = accepted.unwrap();
                read_http_request(&mut second_stream).await;
                write_empty_http_response(&mut second_stream).await;
                false
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => false,
        }
    });

    let url = format!("http://{addr}/reuse");
    let first_client = build_reqwest_client(false).unwrap();
    first_client
        .get(&url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let second_client = build_reqwest_client(false).unwrap();
    second_client
        .get(&url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(server.await.unwrap());
}

fn header_names(request: &str) -> Vec<String> {
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

async fn write_compact_json_response(stream: &mut TcpStream) {
    let body = r#"{"output":[]}"#;
    stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}
