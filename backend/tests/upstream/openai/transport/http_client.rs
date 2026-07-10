use super::*;

#[test]
fn endpoints_should_join_backend_paths() {
    assert_eq!(
        codex_proxy_rs::upstream::openai::transport::endpoint_url(
            "https://api.example.com/",
            "/codex/responses"
        ),
        "https://api.example.com/codex/responses"
    );
    assert_eq!(
        codex_proxy_rs::upstream::openai::transport::endpoint_request_path(
            "https://api.example.com/backend-api",
            "/codex/usage"
        ),
        "/backend-api/codex/usage"
    );
}

#[test]
fn custom_ca_should_report_environment_cache_key_consistently() {
    const CASE_ENV: &str = "CODEX_PROXY_TEST_CUSTOM_CA_CACHE_KEY_CASE";
    const SSL_CERT_PATH: &str = "/tmp/codex-proxy-ssl-cert-file.pem";
    const CODEX_CA_PATH: &str = "/tmp/codex-proxy-codex-ca-certificate.pem";

    if let Ok(case) = std::env::var(CASE_ENV) {
        let expected = match case.as_str() {
            "unset" => None,
            "ssl_cert_file" => Some(format!(
                "{}={SSL_CERT_PATH}",
                codex_proxy_rs::upstream::openai::transport::tls::SSL_CERT_FILE_ENV
            )),
            "codex_ca_priority" => Some(format!(
                "{}={CODEX_CA_PATH}",
                codex_proxy_rs::upstream::openai::transport::tls::CODEX_CA_CERT_ENV
            )),
            _ => panic!("unknown custom CA cache key test case: {case}"),
        };

        assert_eq!(
            codex_proxy_rs::upstream::openai::transport::tls::custom_ca_env_cache_key(),
            expected
        );
        return;
    }

    let current_exe = std::env::current_exe().expect("current test binary path");
    let cases = [
        ("unset", None, None),
        (
            "ssl_cert_file",
            None,
            Some((
                codex_proxy_rs::upstream::openai::transport::tls::SSL_CERT_FILE_ENV,
                SSL_CERT_PATH,
            )),
        ),
        (
            "codex_ca_priority",
            Some((
                codex_proxy_rs::upstream::openai::transport::tls::CODEX_CA_CERT_ENV,
                CODEX_CA_PATH,
            )),
            Some((
                codex_proxy_rs::upstream::openai::transport::tls::SSL_CERT_FILE_ENV,
                SSL_CERT_PATH,
            )),
        ),
    ];

    for (case, codex_ca, ssl_cert_file) in cases {
        let mut command = Command::new(&current_exe);
        command
            .arg("--exact")
            .arg("custom_ca_should_report_environment_cache_key_consistently")
            .arg("--nocapture")
            .env(CASE_ENV, case)
            .env_remove(codex_proxy_rs::upstream::openai::transport::tls::CODEX_CA_CERT_ENV)
            .env_remove(codex_proxy_rs::upstream::openai::transport::tls::SSL_CERT_FILE_ENV);
        if let Some((key, value)) = codex_ca {
            command.env(key, value);
        }
        if let Some((key, value)) = ssl_cert_file {
            command.env(key, value);
        }

        let output = command.output().expect("run isolated custom CA case");
        assert!(
            output.status.success(),
            "isolated custom CA case {case} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[tokio::test]
async fn codex_backend_client_should_cap_non_success_error_body_at_one_mib() {
    let server = wiremock::MockServer::start().await;
    let large_error_body = "x".repeat(1024 * 1024 + 17);
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/codex/responses"))
        .respond_with(wiremock::ResponseTemplate::new(500).set_body_string(large_error_body))
        .mount(&server)
        .await;
    let client = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        server.uri(),
        crate::support::fingerprint::runtime_test_fingerprint(),
    );
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "",
            Vec::new(),
        );
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
async fn codex_backend_client_should_parse_retry_after_from_rate_limit_error_body() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/codex/responses"))
        .respond_with(wiremock::ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "code": "rate_limit_exceeded",
                "message": "Rate limit exceeded, try again in 12s"
            }
        })))
        .mount(&server)
        .await;
    let client = CodexBackendClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        server.uri(),
        crate::support::fingerprint::runtime_test_fingerprint(),
    );
    let mut request =
        codex_proxy_rs::upstream::openai::protocol::responses::CodexResponsesRequest::new_http_sse(
            "gpt-5.5",
            "",
            Vec::new(),
        );
    request.force_http_sse = true;

    let result = client
        .create_response(
            &request,
            CodexRequestContext {
                access_token: "access-token",
                account_id: Some("chatgpt-account"),
                request_id: "req_http_retry_after_body",
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

    let Err(CodexClientError::Upstream {
        status,
        retry_after_seconds,
        ..
    }) = result
    else {
        panic!("expected upstream error");
    };
    assert_eq!(status, reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry_after_seconds, Some(12));
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
            () = tokio::time::sleep(Duration::from_millis(500)) => false,
        }
    });

    let url = format!("http://{addr}/reuse");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    client.get(&url).send().await.unwrap().text().await.unwrap();
    client.get(&url).send().await.unwrap().text().await.unwrap();

    assert!(server.await.unwrap());
}
