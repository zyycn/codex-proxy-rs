use super::*;
use provider_openai::transport::build_reqwest_client;
use provider_openai::transport::profile::CodexResidency;
use uuid::Uuid;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

/// 官方本机样本：模型请求带 Core version，backend-client 账号请求不带。
/// 通过真实 HTTP 请求验证画像更新后每条路径取到同一制品中的正确字段。
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
fn residency_is_explicit_and_survives_artifact_updates() {
    let mut initial = test_wire_profile().snapshot();
    initial.residency = Some(CodexResidency::Us);
    let profile = CodexWireProfileState::new(initial);
    profile.update_bundled_release(&CodexBundledReleaseProfile {
        codex_version: "2.0.0".to_owned(),
        desktop_version: "26.999.10000".to_owned(),
        desktop_build: "9999".to_owned(),
        verified_at: Utc::now(),
    });
    let snapshot = profile.snapshot();
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
    assert_eq!(model["version"], "2.0.0");
    assert!(!account.contains_key("x-openai-internal-codex-residency"));
}
