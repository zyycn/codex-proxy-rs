use codex_proxy_rs::{
    codex::gateway::fingerprint::{model::Fingerprint, updater::parse_update_manifest},
    codex::gateway::transport::{
        client::{build_reqwest_client, CodexBackendClient, CodexRequestContext},
        headers::build_codex_headers,
        types::CodexResponsesRequest,
    },
};

use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

#[test]
fn codex_headers_include_desktop_identity_and_turn_state() {
    let fp = Fingerprint::default_for_tests();
    let headers = build_codex_headers(
        &fp,
        "access-token",
        Some("acct_123"),
        Some("turn-state"),
        "rid_1",
    );

    assert_eq!(headers.get("originator").unwrap(), "Codex Desktop");
    assert!(headers.get("user-agent").unwrap().contains("Codex"));
    assert_eq!(headers.get("authorization").unwrap(), "Bearer access-token");
    assert_eq!(headers.get("chatgpt-account-id").unwrap(), "acct_123");
    assert_eq!(headers.get("x-codex-turn-state").unwrap(), "turn-state");
    assert_eq!(headers.get("x-client-request-id").unwrap(), "rid_1");
}

#[test]
fn update_manifest_updates_app_version_and_build_number() {
    let manifest = r#"{"version":"26.600.12345","build_number":"4001"}"#;
    let update = parse_update_manifest(manifest).unwrap();
    assert_eq!(update.app_version, "26.600.12345");
    assert_eq!(update.build_number, "4001");
}

/// 验证使用数据库指纹构造的请求头是否正确
#[tokio::test]
async fn database_fingerprint_headers_should_match_expected_format() {
    let server = MockServer::start().await;

    // 模拟数据库中的最新指纹（auto_update）
    let db_fingerprint = Fingerprint {
        originator: "Codex Desktop".to_string(),
        app_version: "26.609.41114".to_string(),
        build_number: "3888".to_string(),
        platform: "darwin".to_string(),
        arch: "arm64".to_string(),
        chromium_version: "146".to_string(),
        user_agent_template: "Codex Desktop/{app_version} ({platform}; {arch})".to_string(),
        default_headers: Fingerprint::default_headers(),
        header_order: Fingerprint::default_header_order(),
    };

    let expected_user_agent = "Codex Desktop/26.609.41114 (darwin; arm64)";

    // 验证关键请求头
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("user-agent", expected_user_agent))
        .and(header("originator", "Codex Desktop"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "event: response.completed\ndata: {\"response\":{\"id\":\"resp_1\"}}\n\n",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = CodexBackendClient::new(
        build_reqwest_client(false).unwrap(),
        server.uri(),
        db_fingerprint,
    );

    let response = client
        .create_response(
            &CodexResponsesRequest::new_http_sse("gpt-4", "", Vec::new()),
            CodexRequestContext {
                access_token: "test-token",
                account_id: None,
                request_id: "req_test",
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

    assert!(
        response.is_ok(),
        "Request should succeed with correct headers"
    );
}

/// 验证新旧版本指纹的差异
#[test]
fn old_vs_new_fingerprint_user_agent_comparison() {
    let old_fingerprint = Fingerprint::default_codex_desktop();
    let new_fingerprint = Fingerprint {
        originator: "Codex Desktop".to_string(),
        app_version: "26.609.41114".to_string(),
        build_number: "3888".to_string(),
        platform: "darwin".to_string(),
        arch: "arm64".to_string(),
        chromium_version: "146".to_string(),
        user_agent_template: "Codex Desktop/{app_version} ({platform}; {arch})".to_string(),
        default_headers: Fingerprint::default_headers(),
        header_order: Fingerprint::default_header_order(),
    };

    let old_ua = old_fingerprint.user_agent();
    let new_ua = new_fingerprint.user_agent();

    // 验证格式一致性
    assert!(old_ua.starts_with("Codex Desktop/"));
    assert!(new_ua.starts_with("Codex Desktop/"));
    assert!(old_ua.contains("darwin"));
    assert!(new_ua.contains("darwin"));

    // 验证版本号不同
    assert_ne!(
        old_ua, new_ua,
        "New and old fingerprints should have different User-Agent"
    );

    // 验证新版本包含更新的版本号
    assert!(new_ua.contains("26.609.41114"));

    // 验证 sec-ch-ua 格式
    assert_eq!(
        new_fingerprint.sec_ch_ua(),
        "\"Chromium\";v=\"146\", \"Not:A-Brand\";v=\"24\""
    );
}
