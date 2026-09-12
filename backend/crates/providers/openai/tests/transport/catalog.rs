use chrono::{TimeZone, Utc};
use gateway_core::routing::ModelServiceTier;
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use provider_openai::transport::{
    CodexBackendClient, CodexCatalogCapabilityEvidence, CodexCatalogVisibility, CodexClientError,
    CodexModelCatalogError, CodexRequestContext, MAX_CODEX_MODEL_CATALOG_BYTES,
    build_reqwest_client, parse_codex_model_catalog,
};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OFFICIAL_FIXTURE: &[u8] = include_bytes!("fixtures/official_models_snapshot.json");

#[test]
fn native_document_keeps_unknown_nested_fields_nulls_and_model_instructions() {
    let original = json!({
        "slug": "gpt-native", "display_name": "Native",
        "base_instructions": "original model instructions\nwith line breaks",
        "model_messages": {"future_instructions": {"template": "do not replace"}},
        "future_field": [null, {"new": true}], "explicit_null": null,
        "service_tiers": [{"id":"priority", "name":"Fast", "description":"1.5x speed", "future":true}]
    });
    let body = serde_json::to_vec(&json!({"models":[original]})).expect("body");
    let snapshot = parse_codex_model_catalog(&body, None).expect("catalog");
    let model = &snapshot.models()[0];
    let preserved: Value = serde_json::from_slice(model.document().body()).expect("document");
    assert_eq!(preserved, original);
    assert!(!format!("{model:?}").contains("original model instructions"));
    assert!(!format!("{model:?}").contains("future_instructions"));
    let mut changed = original;
    changed["model_messages"]["future_instructions"]["template"] = json!("new content");
    let changed = parse_codex_model_catalog(
        &serde_json::to_vec(&json!({"models":[changed]})).expect("body"),
        None,
    )
    .expect("catalog");
    assert_ne!(snapshot.models(), changed.models());
}

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
fn raw_instructions_and_unknown_fields_should_not_appear_in_snapshot_debug() {
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
fn service_tiers_should_preserve_declared_ids_names_and_descriptions() {
    let body = json!({"models": [{
        "slug": "gpt-future",
        "display_name": "Future",
        "service_tiers": [
            {"id": "priority", "name": "Fast", "description": "Priority processing."},
            {"id": "deferred", "name": "Economy", "description": ""}
        ]
    }]});
    let snapshot = parse_codex_model_catalog(&serde_json::to_vec(&body).expect("JSON"), None)
        .expect("declared service tiers");

    assert_eq!(
        snapshot.models()[0].metadata().service_tiers(),
        [
            ModelServiceTier::new("priority", "Fast", "Priority processing."),
            ModelServiceTier::new("deferred", "Economy", ""),
        ]
    );
}

#[test]
fn missing_or_empty_service_tiers_should_not_infer_fast_from_legacy_metadata() {
    for service_tiers in [None, Some(json!([]))] {
        let mut model = json!({
            "slug": "gpt-5.4",
            "display_name": "GPT-5.4",
            "additional_speed_tiers": ["fast"]
        });
        if let Some(service_tiers) = service_tiers {
            model["service_tiers"] = service_tiers;
        }
        let body = json!({"models": [model]});
        let snapshot = parse_codex_model_catalog(&serde_json::to_vec(&body).expect("JSON"), None)
            .expect("optional service tiers");

        assert!(snapshot.models()[0].metadata().service_tiers().is_empty());
    }
}

#[test]
fn invalid_service_tier_metadata_should_fail_the_entire_snapshot() {
    for (field, value) in [
        ("id", String::new()),
        ("id", "p".repeat(65)),
        ("name", " ".to_owned()),
        ("name", "f".repeat(257)),
        ("name", "Fast\u{1b}".to_owned()),
        ("description", "d".repeat(4097)),
        ("description", "unsafe\u{0}".to_owned()),
    ] {
        let mut tier = json!({"id": "priority", "name": "Fast", "description": "Priority."});
        tier[field] = Value::String(value);
        let body = json!({"models": [{
            "slug": "gpt-5.4", "display_name": "GPT-5.4", "service_tiers": [tier]
        }]});

        assert_eq!(
            parse_codex_model_catalog(&serde_json::to_vec(&body).expect("JSON"), None),
            Err(CodexModelCatalogError::InvalidMetadata),
            "invalid {field}"
        );
    }
}

#[test]
fn malformed_service_tiers_should_fail_the_official_wire_contract() {
    for service_tiers in [
        Value::Null,
        json!("fast"),
        json!([{"id": "priority", "name": "Fast"}]),
        json!([{"id": 1, "name": "Fast", "description": "Priority."}]),
    ] {
        let body = json!({"models": [{
            "slug": "gpt-5.4", "display_name": "GPT-5.4", "service_tiers": service_tiers
        }]});

        assert_eq!(
            parse_codex_model_catalog(&serde_json::to_vec(&body).expect("JSON"), None),
            Err(CodexModelCatalogError::InvalidWire)
        );
    }
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
fn unknown_top_level_pagination_fields_should_not_reject_the_snapshot() {
    let snapshot = parse_codex_model_catalog(
        br#"{"models":[{"slug":"gpt-5.4","display_name":"GPT-5.4"}],"has_more":true,"cursor":"next"}"#,
        None,
    )
    .expect("unknown pagination fields are not part of the model contract");

    assert_eq!(snapshot.models()[0].request_model().as_str(), "gpt-5.4");
}

#[test]
fn unknown_input_modality_should_degrade_capability_evidence_to_unknown() {
    let snapshot = parse_codex_model_catalog(
        br#"{"models":[{"slug":"gpt-future","display_name":"Future","input_modalities":["audio"]}]}"#,
        None,
    )
    .expect("future modality should not reject the snapshot");
    let capabilities = snapshot.models()[0].capabilities();

    assert_eq!(
        (capabilities.text_input(), capabilities.image_input()),
        (
            CodexCatalogCapabilityEvidence::Unknown,
            CodexCatalogCapabilityEvidence::Unknown
        )
    );
}

#[test]
fn unknown_visibility_should_degrade_without_rejecting_the_snapshot() {
    let snapshot = parse_codex_model_catalog(
        br#"{"models":[{"slug":"gpt-future","display_name":"Future","visibility":"preview_only"}]}"#,
        None,
    )
    .expect("future visibility should not reject the snapshot");

    assert_eq!(
        snapshot.models()[0].metadata().visibility(),
        Some(CodexCatalogVisibility::Unknown)
    );
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
        .and(header(
            "user-agent",
            "codex_cli_rs/0.144.0 (linux 6.8; x86_64) xterm (codex_cli_rs; 1.0.0)",
        ))
        .and(header("accept", "*/*"))
        .and(header("version", "0.144.0"))
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
        .fetch_models_with_context(context(), None)
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

    let result = client.fetch_models_with_context(context(), None).await;

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
        residency: None,
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("valid fixture time"),
    })
}

fn context() -> CodexRequestContext<'static> {
    CodexRequestContext {
        trace: None,
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
        account_selection: Default::default(),
    }
}
