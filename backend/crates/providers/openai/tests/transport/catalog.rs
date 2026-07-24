use chrono::{TimeZone, Utc};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use provider_openai::transport::{
    CodexBackendClient, CodexCatalogCapabilityEvidence, CodexClientError, CodexModelCatalogError,
    CodexRequestContext, MAX_CODEX_MODEL_CATALOG_BYTES, build_reqwest_client,
    parse_codex_model_catalog,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OFFICIAL_FIXTURE: &[u8] = include_bytes!("fixtures/official_models_snapshot.json");

#[test]
fn official_fixture_should_produce_safe_full_snapshot() {
    let snapshot = parse_codex_model_catalog(OFFICIAL_FIXTURE, Some("W/\"codex-v1\""))
        .expect("official fixture should parse");
    let model = &snapshot.models()[0];

    assert_eq!(
        (
            snapshot.models().len(),
            snapshot.etag(),
            model.request_model().as_str(),
            model.display_name(),
            model
                .limits()
                .context_window_tokens()
                .map(|value| value.get()),
            model
                .limits()
                .max_context_window_tokens()
                .map(|value| value.get()),
            (
                model.capabilities().responses_api(),
                model.capabilities().reasoning(),
                model.capabilities().parallel_tool_calls(),
                model.capabilities().text_input(),
                model.capabilities().image_input(),
                model.capabilities().web_search(),
                model.capabilities().reasoning_efforts(),
            ),
            model.metadata().description(),
            model.metadata().priority(),
        ),
        (
            1,
            Some("W/\"codex-v1\""),
            "gpt-5.4",
            "GPT-5.4",
            Some(272_000),
            Some(272_000),
            (
                CodexCatalogCapabilityEvidence::DeclaredNative,
                CodexCatalogCapabilityEvidence::DeclaredNative,
                CodexCatalogCapabilityEvidence::DeclaredNative,
                CodexCatalogCapabilityEvidence::DeclaredNative,
                CodexCatalogCapabilityEvidence::DeclaredNative,
                CodexCatalogCapabilityEvidence::DeclaredNative,
                &["low".to_owned(), "high".to_owned()][..],
            ),
            Some("Frontier agentic coding model."),
            Some(1),
        )
    );
}

#[test]
fn non_whitelisted_wire_fields_should_not_survive_normalization() {
    let snapshot =
        parse_codex_model_catalog(OFFICIAL_FIXTURE, None).expect("official fixture should parse");
    let debug = format!("{snapshot:?}");

    assert!(
        !debug.contains("provider-only instructions")
            && !debug.contains("provider-only template")
            && !debug.contains("available_in_plans")
    );
}

#[test]
fn invalid_etag_should_fail_the_entire_snapshot() {
    let result = parse_codex_model_catalog(OFFICIAL_FIXTURE, Some("raw-unquoted-etag"));

    assert!(matches!(result, Err(CodexModelCatalogError::InvalidEtag)));
}

#[test]
fn missing_capability_fields_should_remain_unknown() {
    let snapshot = parse_codex_model_catalog(
        br#"{"models":[{"slug":"gpt-unknown","display_name":"Unknown"}]}"#,
        None,
    )
    .expect("identity-only official entry should parse");
    let model = &snapshot.models()[0];

    assert_eq!(
        (
            model.capabilities().responses_api(),
            model.capabilities().reasoning(),
            model.capabilities().parallel_tool_calls(),
            model.capabilities().text_input(),
            model.capabilities().image_input(),
            model.limits().context_window_tokens(),
        ),
        (
            CodexCatalogCapabilityEvidence::Unknown,
            CodexCatalogCapabilityEvidence::Unknown,
            CodexCatalogCapabilityEvidence::Unknown,
            CodexCatalogCapabilityEvidence::Unknown,
            CodexCatalogCapabilityEvidence::Unknown,
            None,
        )
    );
}

#[test]
fn legacy_data_shape_should_fail_the_entire_snapshot() {
    let result = parse_codex_model_catalog(
        br#"{"data":[{"slug":"gpt-5.4","display_name":"GPT-5.4"}]}"#,
        None,
    );

    assert!(matches!(result, Err(CodexModelCatalogError::InvalidWire)));
}

#[test]
fn empty_models_should_fail_the_entire_snapshot() {
    let result = parse_codex_model_catalog(br#"{"models":[]}"#, None);

    assert!(matches!(result, Err(CodexModelCatalogError::EmptySnapshot)));
}

#[test]
fn duplicate_request_slugs_should_fail_the_entire_snapshot() {
    let result = parse_codex_model_catalog(
        br#"{"models":[{"slug":"gpt-5.4","display_name":"A"},{"slug":"gpt-5.4","display_name":"B"}]}"#,
        None,
    );

    assert!(matches!(
        result,
        Err(CodexModelCatalogError::DuplicateModelSlug)
    ));
}

#[test]
fn pagination_signal_should_fail_the_entire_snapshot() {
    let result = parse_codex_model_catalog(
        br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4"}],"has_more":true,"cursor":"next"}"#,
        None,
    );

    assert!(matches!(result, Err(CodexModelCatalogError::InvalidWire)));
}

#[test]
fn invalid_request_slug_should_fail_the_entire_snapshot() {
    let result = parse_codex_model_catalog(
        br#"{"models":[{"slug":"gpt-5.4","display_name":"Good"},{"slug":"https://evil.invalid/model","display_name":"Bad"}]}"#,
        None,
    );

    assert!(matches!(
        result,
        Err(CodexModelCatalogError::InvalidModelSlug)
    ));
}

#[test]
fn body_over_hard_limit_should_fail_before_json_parsing() {
    let body = vec![b' '; MAX_CODEX_MODEL_CATALOG_BYTES + 1];
    let result = parse_codex_model_catalog(&body, None);

    assert!(matches!(
        result,
        Err(CodexModelCatalogError::ResponseTooLarge)
    ));
}

#[tokio::test]
async fn fetch_should_send_official_catalog_headers_and_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .and(query_param("client_version", "0.144.0"))
        .and(header("authorization", "Bearer oauth-access"))
        .and(header("chatgpt-account-id", "acct_123"))
        .and(header("originator", "codex_cli_rs"))
        .and(header("accept", "application/json"))
        .and(header("x-codex-installation-id", "installation-123"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .insert_header("etag", "\"catalog-v1\"")
                .set_body_raw(OFFICIAL_FIXTURE, "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = CodexBackendClient::new(
        build_reqwest_client().expect("build Codex client"),
        server.uri(),
        profile(),
    );
    let snapshot = client
        .fetch_models_with_context(context())
        .await
        .expect("fetch strict snapshot");

    assert_eq!(snapshot.models()[0].request_model().as_str(), "gpt-5.4");
    let requests = server
        .received_requests()
        .await
        .expect("received model catalog request");
    let headers = &requests[0].headers;
    for forbidden in [
        "content-type",
        "openai-beta",
        "x-openai-internal-codex-residency",
        "x-client-request-id",
        "session_id",
        "session-id",
        "thread-id",
    ] {
        assert!(
            headers.get(forbidden).is_none(),
            "unexpected {forbidden} header"
        );
    }
}

#[tokio::test]
async fn fetch_should_reject_streamed_body_over_hard_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
            b' ';
            MAX_CODEX_MODEL_CATALOG_BYTES
                + 1
        ]))
        .expect(1)
        .mount(&server)
        .await;
    let client = CodexBackendClient::new(
        build_reqwest_client().expect("build Codex client"),
        server.uri(),
        profile(),
    );

    let result = client.fetch_models_with_context(context()).await;

    assert!(matches!(
        result,
        Err(CodexClientError::ModelCatalog(
            CodexModelCatalogError::ResponseTooLarge
        ))
    ));
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
        authorization: "Bearer oauth-access",
        account_id: Some("acct_123"),
        request_id: "req_catalog",
        turn_state: None,
        turn_metadata: None,
        beta_features: None,
        include_timing_metrics: None,
        version: None,
        codex_window_id: None,
        parent_thread_id: None,
        cookie_header: None,
        installation_id: Some("installation-123"),
        session_id: None,
        thread_id: None,
        client_request_id: None,
        turn_id: None,
    }
}
