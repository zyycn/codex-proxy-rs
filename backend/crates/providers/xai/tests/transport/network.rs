//! External reqwest network policy contracts.

use std::cell::Cell;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use gateway_core::engine::UpstreamSendState;
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use provider_xai::{
    FailClosedTokenVerifier, FormField, GrokBillingClient, GrokDnsResolutionPlan,
    GrokDnsResolutionPolicy, GrokInferenceRequest, GrokInferenceTransport,
    GrokInferenceTransportErrorKind, GrokModelCatalogSession, GrokOAuthClient, GrokOAuthConfig,
    GrokSessionBinding, HttpMethod, OAuthHttpRequest, OAuthHttpTransport,
    OfficialGrokEndpointPolicy, ReqwestGrokInferenceTransport, ReqwestGrokModelCatalogTransport,
    ReqwestOAuthTransport, SecretValue,
};

use crate::support::loopback_endpoint_policy;

#[tokio::test]
async fn oauth_transport_should_post_form_once_without_redirect() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/redirected", server.uri())),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/redirected"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).expect("wiremock origin");
    let transport = oauth_transport(&origin);
    let request = OAuthHttpRequest::post(
        origin.join("oauth2/token").expect("token URL"),
        Vec::new(),
        vec![FormField::secret(
            "refresh_token",
            SecretValue::new("fixture-refresh".to_owned()),
        )],
    );

    let response = transport.execute(request).await.expect("HTTP response");

    assert_eq!(response.status(), 302);
}

#[tokio::test]
async fn inference_transport_should_stream_one_official_shape_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("data: [DONE]\n\n", "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).expect("wiremock origin");
    let transport = inference_transport(&origin);
    let request = GrokInferenceRequest::new(
        origin.join("v1/responses").expect("responses URL"),
        Vec::new(),
        br#"{"model":"grok-code-test","stream":true}"#.to_vec(),
        GrokSessionBinding::new("wiremock-binding").expect("binding"),
    );

    let response = transport.execute(request).await.expect("SSE response");
    assert_eq!(
        response.http_version(),
        gateway_core::event::UpstreamHttpVersion::Http11
    );
    assert_eq!(response.status_code(), 200);
    let chunks = response.into_body().collect::<Vec<_>>().await;

    assert_eq!(chunks.len(), 1);
}

#[tokio::test]
async fn inference_transport_should_classify_http_failures_without_retaining_bodies() {
    let cases = [
        (400, GrokInferenceTransportErrorKind::InvalidRequest),
        (401, GrokInferenceTransportErrorKind::Unauthorized),
        (402, GrokInferenceTransportErrorKind::QuotaExhausted),
        (403, GrokInferenceTransportErrorKind::PermissionDenied),
        (408, GrokInferenceTransportErrorKind::Timeout),
        (429, GrokInferenceTransportErrorKind::RateLimited),
        (500, GrokInferenceTransportErrorKind::Unavailable),
        (418, GrokInferenceTransportErrorKind::Protocol),
    ];

    for (status, expected_kind) in cases {
        let server = MockServer::start().await;
        let secret = format!("private-upstream-body-{status}");
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(status).set_body_string(secret.clone()))
            .expect(1)
            .mount(&server)
            .await;
        let origin = Url::parse(&server.uri()).expect("wiremock origin");
        let error = inference_transport(&origin)
            .execute(inference_request(&origin))
            .await
            .expect_err("non-success response must be classified");
        let rendered = format!("{error:?}\n{error}");

        assert_eq!(
            (
                error.kind(),
                error.status(),
                error.send_state(),
                error.sensitive_context_was_redacted(),
                error.http_version(),
            ),
            (
                expected_kind,
                Some(status),
                UpstreamSendState::Sent,
                true,
                Some(gateway_core::event::UpstreamHttpVersion::Http11),
            )
        );
        assert!(!rendered.contains(&secret));
    }
}

#[tokio::test]
async fn inference_transport_should_bound_retry_after_to_the_safe_window() {
    for (header, expected) in [
        ("120", Some(Duration::from_secs(120))),
        ("121", None),
        ("0", None),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", header)
                    .set_body_string("private rate limit detail"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let origin = Url::parse(&server.uri()).expect("wiremock origin");
        let error = inference_transport(&origin)
            .execute(inference_request(&origin))
            .await
            .expect_err("rate limit response must fail");

        assert_eq!(error.retry_after(), expected);
    }
}

#[tokio::test]
async fn inference_transport_should_reject_success_with_non_sse_content_type() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{}", "application/json"))
        .expect(1)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).expect("wiremock origin");
    let error = inference_transport(&origin)
        .execute(inference_request(&origin))
        .await
        .expect_err("successful non-SSE response must fail closed");

    assert_eq!(
        (error.kind(), error.send_state()),
        (
            GrokInferenceTransportErrorKind::Protocol,
            UpstreamSendState::Sent,
        )
    );
}

#[tokio::test]
async fn billing_transport_should_get_exact_credits_resource_without_redirect() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/billing"))
        .and(query_param("format", "credits"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"config":{"creditUsagePercent":25}}"#,
            "application/json",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).expect("wiremock origin");
    let transport = Arc::new(
        ReqwestGrokModelCatalogTransport::new(loopback_endpoint_policy(&origin))
            .expect("billing transport"),
    );
    let session = GrokModelCatalogSession::new(
        SecretValue::new("access-token".to_owned()),
        SecretValue::new("user-id".to_owned()),
        None,
        "0.2.101",
    )
    .expect("billing session");
    GrokBillingClient::new(transport)
        .fetch(&session)
        .await
        .expect("billing response");
}

#[test]
fn endpoint_policy_should_reject_private_and_documentation_addresses() {
    for address in [
        "127.0.0.1",
        "10.0.0.1",
        "169.254.1.1",
        "192.0.2.1",
        "2001:db8::1",
        "::1",
    ] {
        assert!(
            !OfficialGrokEndpointPolicy::accepts_resolved_address(
                address.parse().expect("fixture address")
            ),
            "{address} must be rejected"
        );
    }
}

#[test]
fn fake_ip_system_result_should_use_public_trusted_fallback() {
    let calls = Cell::new(0_u8);
    let result = resolve_with_policy(
        "auth.x.ai",
        vec!["198.18.0.100".parse().expect("fake IP")],
        || {
            calls.set(calls.get() + 1);
            vec!["104.18.18.80".parse().expect("public IP")]
        },
    )
    .expect("public trusted fallback should pass");

    assert_eq!(calls.get(), 1);
    assert_eq!(
        result,
        vec!["104.18.18.80".parse::<IpAddr>().expect("public IP")]
    );
}

#[test]
fn trusted_fallback_should_reject_the_whole_set_when_any_address_is_private() {
    let result = resolve_with_policy(
        "auth.x.ai",
        vec!["198.18.0.100".parse().expect("fake IP")],
        || {
            vec![
                "104.18.18.80".parse().expect("public IP"),
                "10.0.0.8".parse().expect("private IP"),
            ]
        },
    );

    assert!(result.is_err());
}

#[test]
fn non_allowlisted_host_should_not_invoke_trusted_fallback() {
    let calls = Cell::new(0_u8);
    let result = resolve_with_policy("metadata.invalid", Vec::new(), || {
        calls.set(calls.get() + 1);
        vec!["104.18.18.80".parse().expect("public IP")]
    });

    assert!(result.is_err());
    assert_eq!(calls.get(), 0);
}

#[test]
fn public_system_result_should_not_invoke_trusted_fallback() {
    let calls = Cell::new(0_u8);
    let system = vec!["104.18.18.80".parse().expect("public IP")];
    let result = resolve_with_policy("auth.x.ai", system.clone(), || {
        calls.set(calls.get() + 1);
        Vec::new()
    })
    .expect("public system result should pass");

    assert_eq!(calls.get(), 0);
    assert_eq!(result, system);
}

#[tokio::test]
async fn official_oauth_transport_should_resolve_through_the_production_policy_when_enabled() {
    if std::env::var("CPR_TEST_XAI_OFFICIAL_NETWORK").as_deref() != Ok("1") {
        return;
    }
    let client = GrokOAuthClient::new(
        GrokOAuthConfig::official("0.2.101").expect("official config"),
        Arc::new(
            ReqwestOAuthTransport::new(Arc::new(OfficialGrokEndpointPolicy))
                .expect("production OAuth transport"),
        ),
        Arc::new(FailClosedTokenVerifier),
    );

    let discovery = client
        .discover()
        .await
        .expect("official discovery should resolve and validate");

    assert_eq!(discovery.issuer().as_str(), "https://auth.x.ai/");
}

#[test]
fn oauth_request_method_should_remain_typed() {
    let request = OAuthHttpRequest::get(
        url::Url::parse("https://auth.x.ai/.well-known/openid-configuration")
            .expect("official URL"),
    );

    assert_eq!(request.method(), HttpMethod::Get);
}

fn oauth_transport(origin: &Url) -> ReqwestOAuthTransport {
    ReqwestOAuthTransport::new(loopback_endpoint_policy(origin)).expect("loopback transport")
}

fn inference_transport(origin: &Url) -> ReqwestGrokInferenceTransport {
    ReqwestGrokInferenceTransport::new(loopback_endpoint_policy(origin))
        .expect("loopback transport")
}

fn inference_request(origin: &Url) -> GrokInferenceRequest {
    GrokInferenceRequest::new(
        origin.join("v1/responses").expect("responses URL"),
        Vec::new(),
        br#"{"model":"grok-code-test","stream":true}"#.to_vec(),
        GrokSessionBinding::new("wiremock-binding").expect("binding"),
    )
}

fn resolve_with_policy(
    requested_host: &str,
    system: Vec<IpAddr>,
    trusted_fallback: impl FnOnce() -> Vec<IpAddr>,
) -> Result<Vec<IpAddr>, provider_xai::GrokDnsResolutionError> {
    let policy = GrokDnsResolutionPolicy::official_oauth();
    match policy.plan_system_resolution(requested_host, &system)? {
        GrokDnsResolutionPlan::System => Ok(system),
        GrokDnsResolutionPlan::TrustedDoh => {
            let addresses = trusted_fallback();
            policy.validate_trusted_doh_resolution(requested_host, &addresses)?;
            Ok(addresses)
        }
    }
}
