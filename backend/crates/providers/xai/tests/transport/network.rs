//! 外置的 reqwest 网络策略契约。

use std::cell::Cell;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use gateway_core::engine::UpstreamSendState;
use reqwest::Client;
use serde_json::json;
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use provider_xai::{
    FailClosedTokenVerifier, FormField, GrokBillingClient, GrokDnsResolutionPlan,
    GrokDnsResolutionPolicy, GrokEndpointPolicy, GrokInferenceClientCacheStatus,
    GrokInferenceRequest, GrokInferenceTransport, GrokInferenceTransportErrorKind,
    GrokModelCatalogSession, GrokOAuthClient, GrokOAuthConfig, GrokReqwestTransportBuildError,
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
async fn inference_transport_should_report_account_client_cache_miss_then_hit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("data: [DONE]\n\n", "text/event-stream"),
        )
        .expect(2)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).expect("wiremock origin");
    let transport = inference_transport(&origin);

    let first = transport
        .execute(inference_request(&origin))
        .await
        .expect("first response");
    let first_metrics = first.transport_metrics();
    assert_eq!(
        first_metrics.client_cache_status(),
        Some(GrokInferenceClientCacheStatus::Miss)
    );
    assert!(first_metrics.headers_ms().is_some());
    first.into_body().collect::<Vec<_>>().await;

    let second = transport
        .execute(inference_request(&origin))
        .await
        .expect("second response");
    assert_eq!(
        second.transport_metrics().client_cache_status(),
        Some(GrokInferenceClientCacheStatus::Hit)
    );
    second.into_body().collect::<Vec<_>>().await;
}

#[tokio::test]
async fn inference_transport_should_reuse_one_client_only_within_the_same_binding() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("data: [DONE]\n\n", "text/event-stream"),
        )
        .expect(3)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).expect("wiremock origin");
    let client_builds = Arc::new(AtomicUsize::new(0));
    let endpoint_policy: Arc<dyn GrokEndpointPolicy> = Arc::new(CountingEndpointPolicy {
        inner: loopback_endpoint_policy(&origin),
        inference_client_builds: Arc::clone(&client_builds),
        concurrency: None,
    });
    let transport = ReqwestGrokInferenceTransport::new(endpoint_policy).expect("transport");

    execute_inference(&transport, &origin, "account-a").await;
    execute_inference(&transport, &origin, "account-a").await;
    execute_inference(&transport, &origin, "account-b").await;

    assert_eq!(client_builds.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn inference_transport_should_evict_the_least_recent_binding_at_the_fixed_capacity() {
    let server = MockServer::start().await;
    let distinct_bindings = ReqwestGrokInferenceTransport::MAX_CACHED_ACCOUNT_CLIENTS + 1;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("data: [DONE]\n\n", "text/event-stream"),
        )
        .expect(distinct_bindings as u64 + 2)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).expect("wiremock origin");
    let client_builds = Arc::new(AtomicUsize::new(0));
    let endpoint_policy: Arc<dyn GrokEndpointPolicy> = Arc::new(CountingEndpointPolicy {
        inner: loopback_endpoint_policy(&origin),
        inference_client_builds: Arc::clone(&client_builds),
        concurrency: None,
    });
    let transport = ReqwestGrokInferenceTransport::new(endpoint_policy).expect("transport");

    for index in 0..distinct_bindings {
        execute_inference(&transport, &origin, &format!("account-{index}")).await;
    }
    execute_inference(
        &transport,
        &origin,
        &format!("account-{}", distinct_bindings - 1),
    )
    .await;
    let builds_after_cached_binding = client_builds.load(Ordering::SeqCst);
    execute_inference(&transport, &origin, "account-0").await;

    assert_eq!(
        (
            builds_after_cached_binding,
            client_builds.load(Ordering::SeqCst),
        ),
        (distinct_bindings, distinct_bindings + 1),
    );
}

#[tokio::test]
async fn inference_transport_should_build_distinct_cold_account_clients_concurrently() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw("data: [DONE]\n\n", "text/event-stream"),
        )
        .expect(3)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).expect("wiremock origin");
    let concurrency = Arc::new(BuildConcurrency::new(Duration::from_millis(100)));
    let endpoint_policy: Arc<dyn GrokEndpointPolicy> = Arc::new(CountingEndpointPolicy {
        inner: loopback_endpoint_policy(&origin),
        inference_client_builds: Arc::new(AtomicUsize::new(0)),
        concurrency: Some(Arc::clone(&concurrency)),
    });
    let transport = Arc::new(
        ReqwestGrokInferenceTransport::new(endpoint_policy).expect("concurrent transport"),
    );
    execute_inference(&transport, &origin, "account-primer").await;

    let start = Arc::new(std::sync::Barrier::new(2));
    let first = {
        let start = Arc::clone(&start);
        let transport = Arc::clone(&transport);
        let origin = origin.clone();
        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("first request runtime");
            start.wait();
            runtime.block_on(execute_inference(&transport, &origin, "account-cold-a"));
        })
    };
    let second = {
        let start = Arc::clone(&start);
        let transport = Arc::clone(&transport);
        let origin = origin.clone();
        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("second request runtime");
            start.wait();
            runtime.block_on(execute_inference(&transport, &origin, "account-cold-b"));
        })
    };
    first.await.expect("first task");
    second.await.expect("second task");

    assert!(concurrency.max_active.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn inference_transport_should_classify_http_failures_without_retaining_bodies() {
    let cases = [
        (400, GrokInferenceTransportErrorKind::InvalidRequest),
        (401, GrokInferenceTransportErrorKind::Unauthorized),
        (402, GrokInferenceTransportErrorKind::PaymentRequired),
        (403, GrokInferenceTransportErrorKind::PermissionDenied),
        (408, GrokInferenceTransportErrorKind::Timeout),
        (429, GrokInferenceTransportErrorKind::RateLimited),
        (500, GrokInferenceTransportErrorKind::Unavailable),
        (504, GrokInferenceTransportErrorKind::Unavailable),
        (529, GrokInferenceTransportErrorKind::Unavailable),
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
        assert_eq!(
            error.transport_metrics().client_cache_status(),
            Some(GrokInferenceClientCacheStatus::Miss)
        );
        assert!(error.transport_metrics().headers_ms().is_some());
        assert!(error.client_visible_upstream_error().is_none());
        assert!(!rendered.contains(&secret));
    }
}

#[tokio::test]
async fn inference_transport_should_expose_safe_flat_json_error_details() {
    let server = MockServer::start().await;
    let message = "You have run out of credits or need a Grok subscription";
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "code": "personal-team-blocked:spending-limit",
            "error": message,
        })))
        .expect(1)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).expect("wiremock origin");
    let error = inference_transport(&origin)
        .execute(inference_request(&origin))
        .await
        .expect_err("spending limit response must fail");
    let detail = error
        .client_visible_upstream_error()
        .expect("structured error detail");
    let rendered = format!("{error:?}\n{error}");

    assert_eq!(
        error.kind(),
        GrokInferenceTransportErrorKind::PaymentRequired
    );
    assert_eq!(
        (detail.message(), detail.code(), detail.error_type()),
        (message, Some("personal_team_blocked_spending_limit"), None,)
    );
    assert!(!rendered.contains(message));
}

#[tokio::test]
async fn inference_transport_should_scrub_nested_json_error_details() {
    let server = MockServer::start().await;
    let account_fingerprint = "123e4567-e89b-12d3-a456-426614174000";
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "code": "rate-limit:burst",
                "type": "rate_limit_error",
                "message": format!("team {account_fingerprint}\nrate limited"),
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).expect("wiremock origin");
    let error = inference_transport(&origin)
        .execute(inference_request(&origin))
        .await
        .expect_err("rate limit response must fail");
    let detail = error
        .client_visible_upstream_error()
        .expect("structured error detail");
    let rendered = format!("{error:?}\n{error}");

    assert_eq!(
        (detail.message(), detail.code(), detail.error_type()),
        (
            "team [redacted] rate limited",
            Some("rate_limit_burst"),
            Some("rate_limit_error"),
        )
    );
    assert!(!rendered.contains(account_fingerprint));
}

#[tokio::test]
async fn inference_transport_should_scope_forbidden_failures_from_safe_metadata() {
    let cases = [
        (
            json!({"error": {"code": "invalid_token", "message": "token expired"}}),
            GrokInferenceTransportErrorKind::Unauthorized,
            true,
            Some("invalid_token"),
        ),
        (
            json!({"error": {"code": "usage_exhausted", "message": "used all the included free usage for model"}}),
            GrokInferenceTransportErrorKind::ModelQuotaExhausted,
            false,
            Some("usage_exhausted"),
        ),
        (
            json!({"error": {"code": "permission_denied", "message": "access to the chat endpoint is denied"}}),
            GrokInferenceTransportErrorKind::PermissionDenied,
            false,
            Some("permission_denied"),
        ),
        (
            json!({"error": {"code": "permission_denied", "message": "request rejected", "details": {"check": "SAFETY_CHECK_TYPE_INPUT"}}}),
            GrokInferenceTransportErrorKind::PermissionDenied,
            false,
            Some("permission_denied"),
        ),
        (
            json!({"error": {"code": "invalid_token", "message": "request blocked by policy"}}),
            GrokInferenceTransportErrorKind::SafetyRejected,
            false,
            Some("invalid_token"),
        ),
        (
            json!({"error": {"code": "policy_denied", "message": "request rejected"}}),
            GrokInferenceTransportErrorKind::PermissionDenied,
            false,
            Some("policy_denied"),
        ),
    ];

    for (body, expected_kind, expected_recovery, expected_code) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(403).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let origin = Url::parse(&server.uri()).expect("wiremock origin");
        let error = inference_transport(&origin)
            .execute(inference_request(&origin))
            .await
            .expect_err("forbidden response must be classified");

        assert_eq!(error.kind(), expected_kind);
        assert_eq!(error.requires_credential_recovery(), expected_recovery);
        assert_eq!(
            error.upstream_code().map(|code| code.as_str()),
            expected_code
        );
    }
}

#[tokio::test]
async fn inference_transport_should_keep_unknown_402_out_of_account_quota_state() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(402).set_body_json(json!({
            "error": {
                "code": "payment_required",
                "type": "billing_error",
                "message": "Payment is required for this request"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).expect("wiremock origin");
    let error = inference_transport(&origin)
        .execute(inference_request(&origin))
        .await
        .expect_err("payment-required response must fail");

    assert_eq!(
        (error.kind(), error.requires_credential_recovery()),
        (GrokInferenceTransportErrorKind::PaymentRequired, false)
    );
}

#[tokio::test]
async fn inference_transport_should_classify_model_quota_for_403_and_429() {
    for status in [403, 429] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "error": {
                    "status": null,
                    "contentType": null,
                    "body": null,
                    "code": "subscription_free_usage_exhausted",
                    "type": null,
                    "message": "You've used all the included free usage for model grok-4.5-0722 for now. Usage resets over a rolling 24-hour window - tokens (actual/limit): 500505/500000. Upgrade to a Grok subscription for higher limits: https://grok.com/supergrok"
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let origin = Url::parse(&server.uri()).expect("wiremock origin");
        let error = inference_transport(&origin)
            .execute(inference_request(&origin))
            .await
            .expect_err("model quota response must fail");

        assert_eq!(
            error.kind(),
            GrokInferenceTransportErrorKind::ModelQuotaExhausted
        );
    }
}

#[tokio::test]
async fn inference_transport_should_prefer_structured_quota_fields_over_conflicting_message() {
    for (code, error_type) in [
        ("quota_exceeded", Some("subscription_free_usage_exhausted")),
        ("rate_limit_exceeded", Some("quota_exceeded")),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(429).set_body_json(json!({
                "error": {
                    "code": code,
                    "type": error_type,
                    "message": "You've used all the included free usage for model grok-4.5-0722 for now."
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let origin = Url::parse(&server.uri()).expect("wiremock origin");
        let error = inference_transport(&origin)
            .execute(inference_request(&origin))
            .await
            .expect_err("quota response must fail");

        assert_eq!(
            error.kind(),
            GrokInferenceTransportErrorKind::QuotaExhausted
        );
    }
}

#[tokio::test]
async fn inference_transport_should_classify_free_usage_429_as_account_free_quota_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "error": {
                "code": "subscription:free-usage-exhausted",
                "message": "You have used all your free usage"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).expect("wiremock origin");
    let error = inference_transport(&origin)
        .execute(inference_request(&origin))
        .await
        .expect_err("free usage exhaustion must fail");

    assert_eq!(
        error.kind(),
        GrokInferenceTransportErrorKind::FreeQuotaExhausted
    );
    assert_eq!(error.status(), Some(429));
    assert_eq!(
        error.upstream_code().map(|code| code.as_str()),
        Some("subscription_free_usage_exhausted")
    );
}

#[tokio::test]
async fn inference_transport_should_apply_sub2api_body_aware_400_failures() {
    let cases = [
        (
            json!({"error": {"code": "subscription:free-usage-exhausted", "message": "free usage exhausted"}}),
            GrokInferenceTransportErrorKind::FreeQuotaExhausted,
            Some("subscription_free_usage_exhausted"),
        ),
        (
            json!({"error": {"message": "selected model is at capacity"}}),
            GrokInferenceTransportErrorKind::RateLimited,
            Some("model_capacity"),
        ),
        (
            json!({"error": {"message": "spending limit reached"}}),
            GrokInferenceTransportErrorKind::PaymentRequired,
            Some("billing_quota"),
        ),
        (
            json!({"error": {"message": "empty model output: no content/tool_calls"}}),
            GrokInferenceTransportErrorKind::Unavailable,
            Some("empty_upstream"),
        ),
        (
            json!({"error": {"message": "rate limit exceeded"}}),
            GrokInferenceTransportErrorKind::InvalidRequest,
            Some("rate_limit"),
        ),
        (
            json!({"error": {"message": "invalid tool schema"}}),
            GrokInferenceTransportErrorKind::InvalidRequest,
            None,
        ),
    ];

    for (body, expected_kind, expected_code) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(400).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let origin = Url::parse(&server.uri()).expect("wiremock origin");
        let error = inference_transport(&origin)
            .execute(inference_request(&origin))
            .await
            .expect_err("400 response must be classified");

        assert_eq!(error.kind(), expected_kind);
        assert_eq!(
            error.upstream_code().map(|code| code.as_str()),
            expected_code
        );
    }
}

#[tokio::test]
async fn inference_transport_should_bound_retry_after_to_the_safe_window() {
    for (header, expected) in [
        ("120", Some(Duration::from_secs(120))),
        ("121", Some(Duration::from_secs(121))),
        ("86400", Some(Duration::from_secs(86_400))),
        ("86401", None),
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
async fn inference_transport_should_classify_reasoning_decode_rejections() {
    for message in [
        "could not decode the compaction blob",
        "could not decrypt the provided encrypted_content",
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": {"message": message}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let origin = Url::parse(&server.uri()).expect("wiremock origin");
        let error = inference_transport(&origin)
            .execute(inference_request(&origin))
            .await
            .expect_err("reasoning decode rejection must be classified");

        assert_eq!(
            error.kind(),
            GrokInferenceTransportErrorKind::InvalidRequest
        );
        assert_eq!(
            error.upstream_code().map(|code| code.as_str()),
            Some("reasoning_decode_failed")
        );
    }
}

#[tokio::test]
async fn inference_transport_should_not_reject_sse_bytes_only_because_content_type_is_wrong() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n",
            "application/json",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let origin = Url::parse(&server.uri()).expect("wiremock origin");
    let chunks = inference_transport(&origin)
        .execute(inference_request(&origin))
        .await
        .expect("body syntax, rather than response metadata, determines the stream")
        .into_body()
        .collect::<Vec<_>>()
        .await;

    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].is_ok());
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
        crate::support::xai_wire_profile(),
    )
    .expect("billing session");
    GrokBillingClient::new(transport)
        .fetch(&session)
        .await
        .expect("billing response");
}

#[test]
fn endpoint_policy_should_reject_private_and_documentation_addresses() {
    let policy = GrokDnsResolutionPolicy::official_oauth();
    for address in [
        "127.0.0.1",
        "10.0.0.1",
        "169.254.1.1",
        "192.0.2.1",
        "2001:db8::1",
        "::1",
    ] {
        let address = address.parse().expect("fixture address");
        assert_eq!(
            policy
                .plan_system_resolution("auth.x.ai", &[address])
                .expect("official host"),
            GrokDnsResolutionPlan::TrustedDoh,
            "{address} must require trusted fallback"
        );
        assert!(
            policy
                .validate_trusted_doh_resolution("auth.x.ai", &[address])
                .is_err(),
            "{address} must be rejected after trusted resolution"
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
        GrokOAuthConfig::official().expect("official config"),
        crate::support::xai_wire_profile(),
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

async fn execute_inference(transport: &ReqwestGrokInferenceTransport, origin: &Url, binding: &str) {
    let request = GrokInferenceRequest::new(
        origin.join("v1/responses").expect("responses URL"),
        Vec::new(),
        br#"{"model":"grok-code-test","stream":true}"#.to_vec(),
        GrokSessionBinding::new(binding).expect("binding"),
    );
    let chunks = transport
        .execute(request)
        .await
        .expect("SSE response")
        .into_body()
        .collect::<Vec<_>>()
        .await;
    assert!(chunks.iter().all(Result::is_ok), "SSE body must be valid");
}

#[derive(Debug)]
struct CountingEndpointPolicy {
    inner: Arc<dyn GrokEndpointPolicy>,
    inference_client_builds: Arc<AtomicUsize>,
    concurrency: Option<Arc<BuildConcurrency>>,
}

#[derive(Debug)]
struct BuildConcurrency {
    delay: Duration,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

impl BuildConcurrency {
    fn new(delay: Duration) -> Self {
        Self {
            delay,
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        }
    }
}

impl GrokEndpointPolicy for CountingEndpointPolicy {
    fn build_oauth_client(
        &self,
        timeout: Option<Duration>,
    ) -> Result<Client, GrokReqwestTransportBuildError> {
        self.inner.build_oauth_client(timeout)
    }

    fn build_inference_client(
        &self,
        timeout: Option<Duration>,
    ) -> Result<Client, GrokReqwestTransportBuildError> {
        self.inference_client_builds.fetch_add(1, Ordering::SeqCst);
        let Some(concurrency) = &self.concurrency else {
            return self.inner.build_inference_client(timeout);
        };
        let active = concurrency.active.fetch_add(1, Ordering::SeqCst) + 1;
        concurrency.max_active.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(concurrency.delay);
        let result = self.inner.build_inference_client(timeout);
        concurrency.active.fetch_sub(1, Ordering::SeqCst);
        result
    }

    fn validate_oauth(&self, url: &Url) -> bool {
        self.inner.validate_oauth(url)
    }

    fn validate_inference(&self, url: &Url) -> bool {
        self.inner.validate_inference(url)
    }

    fn validate_model_catalog(&self, url: &Url) -> bool {
        self.inner.validate_model_catalog(url)
    }

    fn route_billing(&self, url: &Url) -> Option<Url> {
        self.inner.route_billing(url)
    }

    fn validate_jwks(&self, url: &Url) -> bool {
        self.inner.validate_jwks(url)
    }

    fn validate_userinfo(&self, url: &Url) -> bool {
        self.inner.validate_userinfo(url)
    }
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
