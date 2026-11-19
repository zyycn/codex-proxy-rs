//! 验证原始上游错误码独立于本地分类、诊断摘要和重试策略

use super::*;

#[tokio::test]
async fn network_failure_preserves_native_causes_without_the_request_url() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let provider = provider_with_base_url_and_retry_budget(
        &store,
        format!("http://{address}/PRIVATE_REQUEST_PATH"),
        0,
    );
    let mut stream = provider
        .execute(
            planned_request("openai", http_generate_operation()),
            context("req_native_cause", CancellationToken::new()),
        )
        .await
        .unwrap();
    let error = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("connection failure must surface"),
        }
    };
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    let details = error.error_details().unwrap();
    assert!(!details.contains("PRIVATE_REQUEST_PATH"));
    let details: Value = serde_json::from_str(&details).unwrap();
    assert_eq!(details["redacted"], true);
    let mut source = std::error::Error::source(&error);
    let mut saw_native = false;
    while let Some(cause) = source {
        if let Some(http) = cause.downcast_ref::<reqwest::Error>() {
            assert!(http.url().is_none());
            assert!(http.is_connect());
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            saw_native = io.kind() == std::io::ErrorKind::ConnectionRefused;
        }
        source = cause.source();
    }
    assert!(
        saw_native,
        "the original I/O cause must survive Provider mapping"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn upstream_codes_survive_http_and_websocket_opening_without_normalization() {
    let captured = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(captured.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    for use_websocket in [false, true] {
        for (body_code, header_code, expected_kind) in [
            (
                Some("Vendor.Future.v2"),
                None,
                ProviderErrorKind::Unavailable,
            ),
            (Some(" SERVER_ERROR "), None, ProviderErrorKind::Unavailable),
            (
                None,
                Some("Vendor.Identity.v2"),
                ProviderErrorKind::Unavailable,
            ),
            (
                Some("PrEvIoUs_ReSpOnSe_NoT_FoUnD"),
                None,
                ProviderErrorKind::ContinuationRecoveryRequired,
            ),
        ] {
            let store = Arc::new(MemoryAccountStore::default());
            create_account(&store, "acct_provider_contract").await;
            let server = MockServer::start().await;
            let body = json!({"error": {"code": body_code, "message": "PRIVATE_UPSTREAM_DETAIL"}});
            let mut response = ResponseTemplate::new(503).set_body_json(&body);
            if let Some(code) = header_code {
                response = response.insert_header(
                    "x-error-json",
                    STANDARD.encode(json!({"error": {"code": code}}).to_string()),
                );
            }
            Mock::given(method(if use_websocket { "GET" } else { "POST" }))
                .and(path("/codex/responses"))
                .respond_with(response)
                .mount(&server)
                .await;
            let operation = if use_websocket {
                capacity_websocket_operation()
            } else {
                http_generate_operation()
            };
            let mut stream = provider_with_base_url_and_retry_budget(&store, server.uri(), 0)
                .execute(
                    planned_request("openai", operation),
                    context("req_original_error_code", CancellationToken::new()),
                )
                .await
                .expect("prepare stream");
            let error = loop {
                match stream.next().await {
                    Some(Ok(_)) => {}
                    Some(Err(error)) => break error,
                    None => panic!("upstream rejection must surface"),
                }
            };
            let original_code = body_code.or(header_code).unwrap();
            assert_eq!(
                error.upstream_code().map(|code| code.as_str()),
                Some(original_code)
            );
            assert_eq!(error.kind(), expected_kind);
            assert_eq!(
                serde_json::from_str::<Value>(error.raw_upstream_error().unwrap().as_str())
                    .unwrap(),
                body,
            );
            let diagnostic = error.diagnostic().expect("safe diagnostic");
            if expected_kind == ProviderErrorKind::ContinuationRecoveryRequired {
                assert_eq!(diagnostic.as_str(), "上游找不到先前的响应");
            }
            assert!(!diagnostic.as_str().contains(original_code));
            assert!(!diagnostic.as_str().contains("PRIVATE_UPSTREAM_DETAIL"));
            assert!(!format!("{error:?} {error}").contains(original_code));
        }
    }
    let events = captured.json_events();
    assert!(events.iter().any(|event| {
        event["fields"]["message"] == "OpenAI upstream returned an error payload"
    }));
    let logs = serde_json::to_string(&events).unwrap();
    for private in [
        "Vendor.Future.v2",
        "Vendor.Identity.v2",
        "PRIVATE_UPSTREAM_DETAIL",
    ] {
        assert!(!logs.contains(private), "ordinary logs leaked {private}");
    }
}
