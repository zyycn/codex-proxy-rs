use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use provider_xai::{
    GROK_BILLING_URL, GROK_MODEL_CATALOG_URL, GrokBillingClient, GrokBillingError,
    GrokBillingRequest, GrokBillingTransport, GrokBillingTransportError,
    GrokBillingTransportFuture, GrokBillingTransportResponse, GrokCatalogCapabilityEvidence,
    GrokHeaderValue, GrokModelCatalogClient, GrokModelCatalogError, GrokModelCatalogRequest,
    GrokModelCatalogSession, GrokModelCatalogTransport, GrokModelCatalogTransportError,
    GrokModelCatalogTransportFuture, GrokModelCatalogTransportResponse, MAX_GROK_BILLING_BYTES,
    MAX_GROK_MODEL_CATALOG_BYTES, SecretValue, parse_grok_billing, parse_grok_model_catalog,
};

const OFFICIAL_FIXTURE: &[u8] =
    include_bytes!("catalog/fixtures/official_grok_models_snapshot.json");

struct CapturingTransport {
    calls: AtomicUsize,
    request: Mutex<Option<GrokModelCatalogRequest>>,
    response:
        Mutex<Option<Result<GrokModelCatalogTransportResponse, GrokModelCatalogTransportError>>>,
}

impl CapturingTransport {
    fn success(body: impl Into<Vec<u8>>, etag: Option<&str>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            request: Mutex::new(None),
            response: Mutex::new(Some(Ok(GrokModelCatalogTransportResponse::new(
                body,
                etag.map(str::to_owned),
            )))),
        }
    }
}

impl GrokModelCatalogTransport for CapturingTransport {
    fn execute(&self, request: GrokModelCatalogRequest) -> GrokModelCatalogTransportFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.request.lock().expect("capture request") = Some(request);
        let response = self
            .response
            .lock()
            .expect("capture response")
            .take()
            .expect("one catalog response");
        Box::pin(async move { response })
    }
}

struct CapturingBillingTransport {
    request: Mutex<Option<GrokBillingRequest>>,
    response: Mutex<Option<Result<GrokBillingTransportResponse, GrokBillingTransportError>>>,
}

impl CapturingBillingTransport {
    fn success(body: impl Into<Vec<u8>>) -> Self {
        Self {
            request: Mutex::new(None),
            response: Mutex::new(Some(Ok(GrokBillingTransportResponse::new(body)))),
        }
    }
}

impl GrokBillingTransport for CapturingBillingTransport {
    fn execute(&self, request: GrokBillingRequest) -> GrokBillingTransportFuture<'_> {
        *self.request.lock().expect("capture billing request") = Some(request);
        let response = self
            .response
            .lock()
            .expect("capture billing response")
            .take()
            .expect("one billing response");
        Box::pin(async move { response })
    }
}

#[tokio::test]
async fn client_should_send_exact_oauth_headers_without_api_key() {
    let transport = Arc::new(CapturingTransport::success(
        OFFICIAL_FIXTURE,
        Some("\"grok-v1\""),
    ));
    let client = GrokModelCatalogClient::new(transport.clone());
    let snapshot = client
        .fetch(&session(Some("person@example.com")))
        .await
        .expect("fetch official fixture");
    let request = transport.request.lock().expect("captured request");
    let request = request.as_ref().expect("one request");
    let headers = request
        .headers()
        .iter()
        .map(|header| (header.name().to_ascii_lowercase(), header.value().expose()))
        .collect::<Vec<_>>();

    assert_eq!(
        (
            transport.calls.load(Ordering::SeqCst),
            request.endpoint().as_str(),
            header_value(&headers, "authorization"),
            header_value(&headers, "x-xai-token-auth"),
            header_value(&headers, "x-userid"),
            header_value(&headers, "x-email"),
            header_value(&headers, "x-grok-client-version"),
            header_value(&headers, "x-grok-client-mode"),
            header_value(&headers, "accept"),
            header_value(&headers, "x-api-key"),
            snapshot.etag(),
        ),
        (
            1,
            GROK_MODEL_CATALOG_URL,
            Some("Bearer oauth-access"),
            Some("xai-grok-cli"),
            Some("verified-user"),
            Some("person@example.com"),
            Some("0.2.106"),
            Some("headless"),
            Some("application/json"),
            None,
            Some("\"grok-v1\""),
        )
    );
}

#[tokio::test]
async fn client_should_omit_email_when_verified_profile_has_none() {
    let transport = Arc::new(CapturingTransport::success(OFFICIAL_FIXTURE, None));
    let client = GrokModelCatalogClient::new(transport.clone());
    client
        .fetch(&session(None))
        .await
        .expect("fetch without optional email");
    let request = transport.request.lock().expect("captured request");
    let headers = request
        .as_ref()
        .expect("one request")
        .headers()
        .iter()
        .map(|header| header.name().to_ascii_lowercase())
        .collect::<Vec<_>>();

    assert!(!headers.iter().any(|name| name == "x-email"));
}

#[tokio::test]
async fn billing_client_should_use_official_oauth_headers_and_credits_query() {
    let transport = Arc::new(CapturingBillingTransport::success(
        br#"{"config":{"creditUsagePercent":12.5}}"#,
    ));
    let client = GrokBillingClient::new(transport.clone());
    let snapshot = client
        .fetch(&session(Some("person@example.com")))
        .await
        .expect("fetch billing");
    let request = transport.request.lock().expect("captured billing request");
    let request = request.as_ref().expect("one request");
    let headers = request
        .headers()
        .iter()
        .map(|header| (header.name().to_ascii_lowercase(), header.value().expose()))
        .collect::<Vec<_>>();

    assert_eq!(request.endpoint().as_str(), GROK_BILLING_URL);
    assert_eq!(
        header_value(&headers, "authorization"),
        Some("Bearer oauth-access")
    );
    assert_eq!(
        header_value(&headers, "x-xai-token-auth"),
        Some("xai-grok-cli")
    );
    assert_eq!(header_value(&headers, "x-userid"), Some("verified-user"));
    assert_eq!(header_value(&headers, "x-api-key"), None);
    assert_eq!(
        snapshot
            .document()
            .get("config")
            .and_then(|value| value.get("creditUsagePercent"))
            .and_then(serde_json::Value::as_f64),
        Some(12.5),
    );
}

#[test]
fn billing_parser_should_preserve_unknown_provider_fields() {
    let snapshot = parse_grok_billing(
        br#"{"config":{"creditUsagePercent":1.5,"futureWindow":{"kind":"rolling"}},"futureTopLevel":{"enabled":true}}"#,
    )
    .expect("dynamic provider fields are preserved");

    assert!(snapshot.document()["config"].get("futureWindow").is_some());
    assert!(snapshot.document().get("futureTopLevel").is_some());
}

#[test]
fn billing_parser_should_accept_credits_and_legacy_shapes() {
    for body in [
        br#"{"config":{"creditUsagePercent":31.25,"currentPeriod":{"type":"USAGE_PERIOD_TYPE_WEEKLY","start":"2026-07-13T00:00:00Z","end":"2026-07-20T00:00:00Z"},"prepaidBalance":{"val":2500}}}"#.as_slice(),
        br#"{"config":{"monthlyLimit":{"val":2000},"used":{"val":500},"onDemandCap":{"val":1000},"onDemandUsed":{"val":100}}}"#.as_slice(),
        br#"{"config":null}"#.as_slice(),
    ] {
        parse_grok_billing(body).expect("supported official billing shape");
    }
}

#[test]
fn billing_parser_should_reject_invalid_known_fields() {
    for body in [
        br#"[]"#.as_slice(),
        br#"{"config":[]}"#.as_slice(),
        br#"{"config":{"creditUsagePercent":101}}"#.as_slice(),
        br#"{"config":{"used":{"val":-1}}}"#.as_slice(),
        br#"{"config":{"currentPeriod":"weekly"}}"#.as_slice(),
        br#"{"onDemandEnabled":"yes"}"#.as_slice(),
    ] {
        assert!(matches!(
            parse_grok_billing(body),
            Err(GrokBillingError::InvalidWire)
        ));
    }
}

#[test]
fn billing_body_over_hard_limit_should_fail_before_parsing() {
    let body = vec![b' '; MAX_GROK_BILLING_BYTES + 1];

    assert!(matches!(
        parse_grok_billing(&body),
        Err(GrokBillingError::ResponseTooLarge)
    ));
}

#[test]
fn billing_snapshot_debug_should_not_print_values() {
    let snapshot =
        parse_grok_billing(br#"{"config":{"subscriptionSecretMarker":"private-billing-marker"}}"#)
            .expect("dynamic billing document");

    assert!(!format!("{snapshot:?}").contains("private-billing-marker"));
}

#[test]
fn official_fixture_should_use_actual_model_and_whitelisted_metadata() {
    let snapshot = parse_grok_model_catalog(OFFICIAL_FIXTURE, Some("W/\"grok-v1\""))
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
            model.limits().max_output_tokens().map(|value| value.get()),
            (
                model.capabilities().responses_api(),
                model.capabilities().reasoning_effort(),
                model.capabilities().backend_search(),
                model.capabilities().streaming_tool_calls(),
            ),
            model.metadata().catalog_entry_id(),
            model.metadata().description(),
            model.metadata().hidden(),
        ),
        (
            1,
            Some("W/\"grok-v1\""),
            "grok-4.5",
            Some("Grok 4.5"),
            Some(1_000_000),
            Some(131_072),
            (
                GrokCatalogCapabilityEvidence::DeclaredNative,
                GrokCatalogCapabilityEvidence::DeclaredNative,
                GrokCatalogCapabilityEvidence::DeclaredNative,
                GrokCatalogCapabilityEvidence::DeclaredNative,
            ),
            Some("grok-4.5-catalog-entry"),
            Some("Official Grok Build coding model."),
            Some(false),
        )
    );
}

#[test]
fn non_whitelisted_wire_fields_should_not_survive_normalization() {
    let snapshot =
        parse_grok_model_catalog(OFFICIAL_FIXTURE, None).expect("official fixture should parse");
    let debug = format!("{snapshot:?}");

    assert!(
        !debug.contains("provider-only field")
            && !debug.contains("extraHeaders")
            && !debug.contains("baseUrl")
    );
}

#[test]
fn invalid_etag_should_fail_the_entire_snapshot() {
    let result = parse_grok_model_catalog(OFFICIAL_FIXTURE, Some("raw-unquoted-etag"));

    assert!(matches!(result, Err(GrokModelCatalogError::InvalidEtag)));
}

#[test]
fn missing_capability_fields_should_remain_unknown() {
    let snapshot =
        parse_grok_model_catalog(br#"{"object":"list","data":[{"id":"grok-unknown"}]}"#, None)
            .expect("identity-only official entry should parse");
    let model = &snapshot.models()[0];

    assert_eq!(
        (
            model.capabilities().responses_api(),
            model.capabilities().reasoning_effort(),
            model.capabilities().backend_search(),
            model.capabilities().streaming_tool_calls(),
            model.limits().context_window_tokens(),
            model.display_name(),
        ),
        (
            GrokCatalogCapabilityEvidence::Unknown,
            GrokCatalogCapabilityEvidence::Unknown,
            GrokCatalogCapabilityEvidence::Unknown,
            GrokCatalogCapabilityEvidence::Unknown,
            None,
            None,
        )
    );
}

#[test]
fn model_id_should_take_priority_over_catalog_id() {
    let snapshot = parse_grok_model_catalog(
        br#"{"object":"list","data":[{"id":"catalog-entry","modelId":"grok-4-fast"}]}"#,
        None,
    )
    .expect("modelId is official fallback");

    assert_eq!(snapshot.models()[0].request_model().as_str(), "grok-4-fast");
}

#[test]
fn official_responses_backend_should_be_native_without_redundant_supported_flag() {
    let snapshot = parse_grok_model_catalog(
        br#"{"object":"list","data":[{"id":"grok-responses","api_backend":"responses"}]}"#,
        None,
    )
    .expect("Responses backend is explicit capability evidence");

    assert_eq!(
        snapshot.models()[0].capabilities().responses_api(),
        GrokCatalogCapabilityEvidence::DeclaredNative
    );
}

#[test]
fn explicit_api_disable_and_non_responses_backend_should_remain_unsupported() {
    for body in [
        br#"{"object":"list","data":[{"id":"grok-disabled","api_backend":"responses","supported_in_api":false}]}"#.as_slice(),
        br#"{"object":"list","data":[{"id":"grok-chat","api_backend":"chat_completions","supported_in_api":true}]}"#.as_slice(),
    ] {
        let snapshot = parse_grok_model_catalog(body, None).expect("valid unsupported entry");
        assert_eq!(
            snapshot.models()[0].capabilities().responses_api(),
            GrokCatalogCapabilityEvidence::DeclaredUnsupported
        );
    }
}

#[test]
fn list_discriminator_should_be_required_and_exact() {
    for body in [
        br#"{"data":[{"id":"grok-4"}]}"#.as_slice(),
        br#"{"object":"collection","data":[{"id":"grok-4"}]}"#.as_slice(),
    ] {
        assert!(matches!(
            parse_grok_model_catalog(body, None),
            Err(GrokModelCatalogError::InvalidWire)
        ));
    }
}

#[test]
fn legacy_models_shape_should_fail_the_entire_snapshot() {
    let result = parse_grok_model_catalog(br#"{"object":"list","models":[{"id":"grok-4"}]}"#, None);

    assert!(matches!(result, Err(GrokModelCatalogError::InvalidWire)));
}

#[test]
fn empty_data_should_fail_the_entire_snapshot() {
    let result = parse_grok_model_catalog(br#"{"object":"list","data":[]}"#, None);

    assert!(matches!(result, Err(GrokModelCatalogError::EmptySnapshot)));
}

#[test]
fn duplicate_actual_models_should_fail_the_entire_snapshot() {
    let result = parse_grok_model_catalog(
        br#"{"object":"list","data":[{"id":"entry-a","model":"grok-4"},{"id":"entry-b","modelId":"grok-4"}]}"#,
        None,
    );

    assert!(matches!(
        result,
        Err(GrokModelCatalogError::DuplicateModelSlug)
    ));
}

#[test]
fn pagination_signal_should_fail_the_entire_snapshot() {
    let result = parse_grok_model_catalog(
        br#"{"object":"list","data":[{"id":"grok-4"}],"has_more":true,"cursor":"next"}"#,
        None,
    );

    assert!(matches!(result, Err(GrokModelCatalogError::InvalidWire)));
}

#[test]
fn invalid_preferred_model_should_fail_without_falling_back() {
    let result = parse_grok_model_catalog(
        br#"{"object":"list","data":[{"model":"https://evil.invalid/model","modelId":"grok-4","id":"entry"}]}"#,
        None,
    );

    assert!(matches!(
        result,
        Err(GrokModelCatalogError::InvalidModelSlug)
    ));
}

#[test]
fn body_over_hard_limit_should_fail_before_json_parsing() {
    let body = vec![b' '; MAX_GROK_MODEL_CATALOG_BYTES + 1];
    let result = parse_grok_model_catalog(&body, None);

    assert!(matches!(
        result,
        Err(GrokModelCatalogError::ResponseTooLarge)
    ));
}

#[tokio::test]
async fn client_should_enforce_hard_limit_for_injected_transport_too() {
    let transport = Arc::new(CapturingTransport::success(
        vec![b' '; MAX_GROK_MODEL_CATALOG_BYTES + 1],
        None,
    ));
    let client = GrokModelCatalogClient::new(transport);

    let result = client.fetch(&session(None)).await;

    assert!(matches!(
        result,
        Err(GrokModelCatalogError::ResponseTooLarge)
    ));
}

#[test]
fn session_debug_should_redact_oauth_and_identity_values() {
    let debug = format!("{:?}", session(Some("person@example.com")));

    assert!(
        !debug.contains("oauth-access")
            && !debug.contains("verified-user")
            && !debug.contains("person@example.com")
    );
}

fn session(email: Option<&str>) -> GrokModelCatalogSession {
    GrokModelCatalogSession::new(
        SecretValue::new("oauth-access".to_owned()),
        SecretValue::new("verified-user".to_owned()),
        email.map(|value| SecretValue::new(value.to_owned())),
        crate::support::xai_wire_profile(),
    )
    .expect("valid OAuth fixture")
}

fn header_value<'a>(headers: &'a [(String, &str)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| *value)
}

#[tokio::test]
async fn captured_sensitive_headers_should_remain_typed_as_sensitive() {
    let session = session(Some("person@example.com"));
    let transport = Arc::new(CapturingTransport::success(OFFICIAL_FIXTURE, None));
    let client = GrokModelCatalogClient::new(transport.clone());
    client.fetch(&session).await.expect("fetch fixture");
    let request = transport.request.lock().expect("captured request");

    assert!(
        request
            .as_ref()
            .expect("request")
            .headers()
            .iter()
            .all(|header| match header.name().to_ascii_lowercase().as_str() {
                "authorization" | "x-userid" | "x-email" => {
                    matches!(header.value(), GrokHeaderValue::Sensitive(_))
                }
                _ => matches!(header.value(), GrokHeaderValue::Public(_)),
            })
    );
}
