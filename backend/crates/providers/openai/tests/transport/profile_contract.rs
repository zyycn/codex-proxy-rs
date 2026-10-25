//! 验证各端点共用当前客户端画像并保留部署指定的驻留与路由请求头

use super::*;
use provider_openai::transport::build_reqwest_client;
use uuid::Uuid;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

/// 官方本机样本：模型请求带 Core version，backend-client 账号请求不带
/// 通过真实 HTTP 请求验证画像更新后每条路径取到同一制品中的正确字段
#[tokio::test]
async fn each_core_endpoint_uses_the_current_bundled_release() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error":"audit fixture"})))
        .expect(12)
        .mount(&server)
        .await;
    let profile = test_wire_profile();
    let client = CodexBackendClient::new(
        build_reqwest_client().expect("HTTP client"),
        format!("{}/backend-api", server.uri()),
        profile.clone(),
    );
    for (core, desktop, build) in [
        ("0.152.8", "26.800.10000", "8000"),
        ("0.153.4", "26.901.51231", "8109"),
    ] {
        profile.update_bundled_release(&CodexBundledReleaseProfile {
            codex_version: core.to_owned(),
            desktop_version: desktop.to_owned(),
            desktop_build: build.to_owned(),
            verified_at: Utc::now(),
        });
        let context = request_context("audit", Some("audit-account"));
        client
            .fetch_models_with_context(context, None)
            .await
            .expect_err("fixture rejection");
        client
            .fetch_usage(context)
            .await
            .expect_err("fixture rejection");
        client
            .fetch_profile_statistics(context)
            .await
            .expect_err("fixture rejection");
        client
            .list_rate_limit_reset_credits(context)
            .await
            .expect_err("fixture rejection");
        client
            .consume_rate_limit_reset_credit(context, None, Uuid::now_v7())
            .await
            .expect_err("fixture rejection");
        let mut body = codex_request_body("gpt-test", "", Vec::new());
        body.insert("service_tier".to_owned(), json!("priority"));
        let mut request = CodexResponsesRequest::from_body(body);
        request.force_http_sse = true;
        client
            .create_response(&request, context)
            .await
            .expect_err("fixture rejection");

        let captured = server.received_requests().await.expect("captured requests");
        for request in &captured[captured.len() - 6..] {
            let headers = &request.headers;
            assert_eq!(headers["user-agent"], profile.snapshot().user_agent());
            assert_eq!(headers["authorization"], "Bearer access-token");
            assert_eq!(headers["chatgpt-account-id"], "audit-account");
            for forbidden in [
                "x-codex-installation-id",
                "x-codex-turn-id",
                "x-openai-internal-codex-residency",
                "oai-language",
                "openai-beta",
            ] {
                assert!(
                    !headers.contains_key(forbidden),
                    "{}: {forbidden}",
                    request.url
                );
            }
            if request.url.path().contains("/codex/") {
                assert_eq!(headers["originator"], "codex_cli_rs");
                assert_eq!(headers["version"], core);
                assert_ne!(headers["version"], desktop);
            } else {
                assert!(!headers.contains_key("originator"));
                assert!(!headers.contains_key("version"));
            }
            if request.url.path().ends_with("/responses") {
                assert_eq!(headers["accept"], "text/event-stream");
                assert_eq!(
                    headers["x-codex-routing-hint"],
                    "model=gpt-test;tier=priority"
                );
            } else {
                assert_eq!(headers["accept"], "*/*");
            }
            if request.url.path().ends_with("/models") {
                assert_eq!(
                    request.url.query(),
                    Some(format!("client_version={core}").as_str())
                );
            }
        }
    }
}

#[test]
fn deployment_routing_survives_artifact_updates_and_profile_selection() {
    let mut initial = test_wire_profile().snapshot();
    initial.residency = Some("us".to_owned());
    initial.account_routing_override = Some("custom-route".to_owned());
    let profile = CodexWireProfileState::new(initial);
    profile.update_bundled_release(&CodexBundledReleaseProfile {
        codex_version: "2.0.0".to_owned(),
        desktop_version: "26.999.10000".to_owned(),
        desktop_build: "9999".to_owned(),
        verified_at: Utc::now(),
    });
    let snapshot =
        provider_openai::transport::profile::selection::ClientProfileSelection::default()
            .resolve(&profile)
            .expect("selected profile");
    let model = provider_openai::transport::headers::build_codex_model_headers(
        &snapshot,
        "Bearer audit",
        None,
    )
    .expect("model headers");
    let account = provider_openai::transport::headers::build_codex_account_headers(
        &snapshot,
        "Bearer audit",
        None,
    )
    .expect("account headers");
    assert_eq!(model["x-openai-internal-codex-residency"], "us");
    assert_eq!(model["x-openai-account-routing-override"], "custom-route");
    assert_eq!(model["version"], "2.0.0");
    assert!(!account.contains_key("x-openai-internal-codex-residency"));
    assert!(!account.contains_key("x-openai-account-routing-override"));
}

#[tokio::test]
async fn model_transports_apply_configured_routing_and_discard_downstream_values() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use gateway_core::operation::{GenerateRequest, ProtocolPayload};

    const HEADERS: [&str; 2] = [
        "x-openai-internal-codex-residency",
        "x-openai-account-routing-override",
    ];
    for (document, expected) in [
        (r#"{}"#, [None, None]),
        (
            r#"{"residency":null,"account_routing_override":null}"#,
            [None, None],
        ),
        (
            r#"{"residency":"us","account_routing_override":"us"}"#,
            [Some("us"), Some("us")],
        ),
        (
            r#"{"account_routing_override":"us_cr"}"#,
            [None, Some("us_cr")],
        ),
        (
            r#"{"residency":"custom-region"}"#,
            [Some("custom-region"), None],
        ),
        (
            r#"{"residency":"","account_routing_override":"custom-route"}"#,
            [Some(""), Some("custom-route")],
        ),
        (
            r#"{"account_routing_override":"NO_CONSTRAINT"}"#,
            [None, Some("NO_CONSTRAINT")],
        ),
    ] {
        for websocket in [false, true] {
            let (base_url, capture): (
                String,
                futures::future::BoxFuture<'static, reqwest::header::HeaderMap>,
            ) = if websocket {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let base_url = format!("http://{}", listener.local_addr().unwrap());
                let task = tokio::spawn(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut headers = None;
                    let mut socket = accept_codex_test_websocket_with(stream, |request, _| {
                        headers = Some(request.headers().clone());
                    })
                    .await;
                    socket.next().await.unwrap().unwrap();
                    socket
                        .send(Message::Text(
                            completed_websocket_response("resp_routing", 1, 1).into(),
                        ))
                        .await
                        .unwrap();
                    headers.unwrap()
                });
                (base_url, Box::pin(async { task.await.unwrap() }))
            } else {
                let server = MockServer::start().await;
                Mock::given(any())
                    .respond_with(
                        ResponseTemplate::new(200)
                            .insert_header("content-type", "text/event-stream")
                            .set_body_string(format!(
                                "event: response.completed\ndata: {}\n\n",
                                completed_websocket_response("resp_routing", 1, 1),
                            )),
                    )
                    .expect(1)
                    .mount(&server)
                    .await;
                (
                    server.uri(),
                    Box::pin(async move {
                        server.received_requests().await.unwrap()[0].headers.clone()
                    }),
                )
            };
            let config: provider_openai::config::OpenAiConfig =
                serde_json::from_str(document).unwrap();
            let profile = CodexWireProfileState::new(CodexWireProfile {
                residency: config.residency,
                account_routing_override: config.account_routing_override,
                ..test_wire_profile().snapshot()
            });
            let client = CodexBackendClient::new(
                reqwest::Client::builder().no_proxy().build().unwrap(),
                base_url,
                profile,
            );
            let downstream_headers = HEADERS
                .into_iter()
                .flat_map(|name| {
                    ["downstream-first", "downstream-second"]
                        .map(|value| json!([name, STANDARD.encode(value)]))
                })
                .collect::<Vec<_>>();
            let payload = ProtocolPayload::json_object(
                "openai",
                codex_request_body("gpt-test", "", Vec::new()),
            )
            .unwrap()
            .with_context(Map::from_iter([
                ("opaque_request_headers".into(), json!(downstream_headers)),
                ("use_websocket".into(), json!(websocket)),
            ]));
            let request = provider_openai::encode_generate_request(
                &GenerateRequest::from_protocol_payload(payload),
                "gpt-test",
                None,
            )
            .unwrap();
            client
                .create_response(&request, request_context("req_routing", Some("account")))
                .await
                .expect("model response");
            let headers = capture.await;
            for (name, value) in HEADERS.into_iter().zip(expected) {
                assert_eq!(
                    headers
                        .get_all(name)
                        .iter()
                        .map(|value| value.to_str().unwrap())
                        .collect::<Vec<_>>(),
                    value.into_iter().collect::<Vec<_>>(),
                    "{document}, websocket={websocket}, {name}",
                );
            }
        }
    }
}

#[tokio::test]
async fn profile_statistics_should_fallback_when_custom_upstream_returns_404() {
    let custom_server = MockServer::start().await;
    let official_server = MockServer::start().await;

    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/codex/profiles/me"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&custom_server)
        .await;

    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/codex/profiles/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "profile": {
                "display_name": "Fallback User"
            },
            "stats": {}
        })))
        .expect(1)
        .mount(&official_server)
        .await;

    let client = CodexBackendClient::new(
        build_reqwest_client().expect("HTTP client"),
        custom_server.uri(),
        test_wire_profile(),
    )
    .with_official_base_url(official_server.uri());

    let context = request_context("audit", Some("audit-account"));
    let statistics = client
        .fetch_profile_statistics(context)
        .await
        .expect("profile statistics after fallback");

    assert_eq!(statistics.display_name.as_deref(), Some("Fallback User"));
}
