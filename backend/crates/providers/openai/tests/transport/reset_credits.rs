//! 验证额度重置卡查询和消费的请求合同与禁止重放边界

use chrono::{TimeZone as _, Utc};
use provider_openai::credential::CodexResetCreditsError;
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use provider_openai::transport::{
    CodexBackendClient, CodexClientError, CodexRequestContext, MAX_CODEX_RESET_CREDITS_BODY_BYTES,
};
use reqwest::{Method, StatusCode};
use serde_json::json;
use std::time::Duration;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(base_url: &str) -> CodexBackendClient {
    CodexBackendClient::new(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("reset-credit client"),
        base_url,
        CodexWireProfileState::new(CodexWireProfile {
            client_kind: provider_openai::transport::profile::selection::ClientKind::Desktop,
            originator: "Codex Desktop".to_owned(),
            codex_version: "0.115.0-alpha.11".to_owned(),
            desktop_version: "26.818.21641".to_owned(),
            desktop_build: "6849".to_owned(),
            os_type: "Mac OS".to_owned(),
            os_version: "15.5.0".to_owned(),
            arch: "arm64".to_owned(),
            terminal: "xterm-256color".to_owned(),
            exact_user_agent: None,
            residency: None,
            verified_at: Utc
                .with_ymd_and_hms(2026, 8, 20, 0, 0, 0)
                .single()
                .expect("profile timestamp"),
        }),
    )
}

fn context() -> CodexRequestContext<'static> {
    CodexRequestContext::auxiliary(
        "Bearer oauth-access",
        Some("acct_workspace"),
        "req_reset_credit",
        Some("must-not-be-attached"),
    )
}

#[tokio::test]
async fn list_should_match_official_core_account_profile() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "available_count": 1,
            "credits": [{
                "id": "credit_1",
                "status": "available",
                "title": "Reset credit",
                "expires_at": "2026-08-30T12:00:00Z",
                "future_field": true
            }],
            "future_top_level": "ignored"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let result = client(&server.uri())
        .list_rate_limit_reset_credits(context())
        .await
        .expect("official reset-credit list");

    assert_eq!(result.available_count, 1);
    assert_eq!(result.credits[0].id, "credit_1");
    let requests = server.received_requests().await.expect("received requests");
    let request = requests.first().expect("list request");
    assert_eq!(request.method, Method::GET);
    assert_eq!(
        header(request, "authorization"),
        Some("Bearer oauth-access")
    );
    assert_eq!(
        header(request, "chatgpt-account-id"),
        Some("acct_workspace")
    );
    assert_eq!(header(request, "originator"), None);
    assert_eq!(header(request, "oai-language"), None);
    assert_eq!(
        header(request, "user-agent"),
        Some(
            "Codex Desktop/0.115.0-alpha.11 (Mac OS 15.5.0; arm64) xterm-256color (Codex Desktop; 26.818.21641)"
        )
    );
    assert_eq!(header(request, "accept"), Some("*/*"));
    for name in [
        "content-type",
        "cookie",
        "x-openai-attach-auth",
        "x-openai-attach-desktop-surface",
        "x-openai-attach-devicecheck-token",
        "x-openai-attach-integrity-state",
        "x-openai-codex-client-version",
        "x-openai-internal-codex-residency",
        "x-codex-installation-id",
        "session-id",
        "x-codex-turn-state",
        "x-oai-is",
    ] {
        assert_eq!(header(request, name), None, "unexpected {name}");
    }
}

#[tokio::test]
async fn consume_should_send_exact_credit_and_redeem_request_id_without_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": "reset",
            "credit": {
                "id": "credit_1",
                "reset_type": "codex_rate_limits"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let redeem_request_id =
        Uuid::parse_str("8fbf302d-11df-4bd5-82e4-08e4b3df7874").expect("redeem request ID");

    let result = client(&server.uri())
        .consume_rate_limit_reset_credit(context(), Some("credit_1"), redeem_request_id)
        .await
        .expect("reset-credit consume");

    assert_eq!(result.code, "reset");
    let requests = server.received_requests().await.expect("received requests");
    let request = requests.first().expect("consume request");
    assert_eq!(request.method, Method::POST);
    assert_eq!(header(request, "content-type"), Some("application/json"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&request.body).expect("consume JSON"),
        json!({
            "credit_id": "credit_1",
            "redeem_request_id": "8fbf302d-11df-4bd5-82e4-08e4b3df7874"
        })
    );
}

#[tokio::test]
async fn count_only_credits_should_allow_upstream_selected_consumption() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "available_count": 1,
            "credits": []
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "code": "reset" })))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server.uri());
    let credits = client
        .list_rate_limit_reset_credits(context())
        .await
        .expect("count-only reset credits");
    assert_eq!(credits.available_count, 1);
    assert!(credits.credits.is_empty());

    let redeem_request_id =
        Uuid::parse_str("8fbf302d-11df-4bd5-82e4-08e4b3df7874").expect("redeem request ID");
    let result = client
        .consume_rate_limit_reset_credit(context(), None, redeem_request_id)
        .await
        .expect("upstream-selected reset-credit consume");
    assert_eq!(result.code, "reset");
    let requests = server.received_requests().await.expect("received requests");
    let request = requests
        .iter()
        .find(|request| request.method == Method::POST)
        .expect("consume request");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&request.body).expect("consume JSON"),
        json!({ "redeem_request_id": "8fbf302d-11df-4bd5-82e4-08e4b3df7874" })
    );
}

#[tokio::test]
async fn consume_should_preserve_raw_upstream_error_and_never_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "17")
                .set_body_raw(
                    r#"{"code":"no_credit","detail":"none left"}"#,
                    "application/json",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server.uri())
        .consume_rate_limit_reset_credit(
            context(),
            None,
            Uuid::parse_str("f37debe7-0150-4328-bca4-ddfd94fd942f").expect("UUID"),
        )
        .await
        .expect_err("upstream rejection");

    match error {
        CodexClientError::Upstream {
            status,
            body,
            retry_after_seconds,
            ..
        } => {
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(body, r#"{"code":"no_credit","detail":"none left"}"#);
            assert_eq!(retry_after_seconds, Some(17));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn list_should_reject_oversized_success_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "x".repeat(MAX_CODEX_RESET_CREDITS_BODY_BYTES + 1),
            "application/json",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let error = client(&server.uri())
        .list_rate_limit_reset_credits(context())
        .await
        .expect_err("oversized reset-credit response");

    assert!(matches!(
        error,
        CodexClientError::Upstream {
            status: StatusCode::BAD_GATEWAY,
            ..
        }
    ));
}

#[tokio::test]
async fn list_should_fallback_to_official_endpoint_when_custom_upstream_returns_404() {
    let custom_server = MockServer::start().await;
    let official_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(StatusCode::NOT_FOUND))
        .expect(1)
        .mount(&custom_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(StatusCode::OK).set_body_json(json!({
            "available_count": 2,
            "credits": [{
                "id": "credit_fallback",
                "status": "available",
                "title": "Fallback Reset Credit",
                "expires_at": "2026-09-30T12:00:00Z"
            }]
        })))
        .expect(1)
        .mount(&official_server)
        .await;

    let test_client = client(&custom_server.uri()).with_official_base_url(official_server.uri());
    let result = test_client
        .list_rate_limit_reset_credits(context())
        .await
        .expect("fallback reset-credit list");

    assert_eq!(result.available_count, 2);
    assert_eq!(result.credits[0].id, "credit_fallback");
}

#[tokio::test]
async fn consume_should_fallback_to_official_endpoint_when_custom_upstream_returns_404() {
    let custom_server = MockServer::start().await;
    let official_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(ResponseTemplate::new(StatusCode::NOT_FOUND))
        .expect(1)
        .mount(&custom_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(ResponseTemplate::new(StatusCode::OK).set_body_json(json!({
            "code": "reset"
        })))
        .expect(1)
        .mount(&official_server)
        .await;

    let test_client = client(&custom_server.uri()).with_official_base_url(official_server.uri());
    let result = test_client
        .consume_rate_limit_reset_credit(
            context(),
            Some("credit_fallback"),
            Uuid::parse_str("8fbf302d-11df-4bd5-82e4-08e4b3df7874").expect("UUID"),
        )
        .await
        .expect("fallback consume result");

    assert_eq!(result.code, "reset");
    for server in [&custom_server, &official_server] {
        let requests = server.received_requests().await.expect("consume requests");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&requests[0].body).unwrap(),
            json!({
                "credit_id": "credit_fallback",
                "redeem_request_id": "8fbf302d-11df-4bd5-82e4-08e4b3df7874"
            })
        );
        assert_eq!(
            header(&requests[0], "authorization"),
            Some("Bearer oauth-access")
        );
        assert_eq!(
            header(&requests[0], "chatgpt-account-id"),
            Some("acct_workspace")
        );
    }
}

#[tokio::test]
async fn list_fallback_should_preserve_credential_refresh_signal() {
    let custom_server = MockServer::start().await;
    let official_server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&custom_server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_string("expired"))
        .expect(1)
        .mount(&official_server)
        .await;

    let error = client(&custom_server.uri())
        .with_official_base_url(official_server.uri())
        .list_rate_limit_reset_credits(context())
        .await
        .expect_err("fallback requires credential refresh");
    let CodexClientError::Upstream {
        status,
        body,
        diagnostics,
        ..
    } = error
    else {
        panic!("expected the fallback HTTP rejection");
    };
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(diagnostics.status_code, Some(401));
    assert_eq!(body, "expired");
}

#[tokio::test]
async fn consume_fallback_should_preserve_rejection_and_retry_after() {
    let custom_server = MockServer::start().await;
    let official_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&custom_server)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "17")
                .set_body_raw(r#"{"code":"no_credit"}"#, "application/json"),
        )
        .expect(1)
        .mount(&official_server)
        .await;

    let error = client(&custom_server.uri())
        .with_official_base_url(official_server.uri())
        .consume_rate_limit_reset_credit(context(), None, Uuid::new_v4())
        .await
        .expect_err("fallback explicitly rejects consumption");
    let CodexClientError::Upstream {
        status,
        body,
        diagnostics,
        retry_after_seconds,
        ..
    } = error
    else {
        panic!("expected the fallback HTTP rejection");
    };
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(diagnostics.status_code, Some(429));
    assert_eq!(body, r#"{"code":"no_credit"}"#);
    assert_eq!(retry_after_seconds, Some(17));
}

#[tokio::test]
async fn consume_fallback_timeout_should_not_become_an_explicit_404_rejection() {
    let custom_server = MockServer::start().await;
    let official_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&custom_server)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code": "reset"}))
                .set_delay(Duration::from_secs(3)),
        )
        .expect(1)
        .mount(&official_server)
        .await;
    let test_client = CodexBackendClient::new(
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap(),
        custom_server.uri(),
        super::test_wire_profile(),
    )
    .with_official_base_url(official_server.uri());
    let redeem_request_id = Uuid::new_v4();

    let error = test_client
        .consume_rate_limit_reset_credit(context(), None, redeem_request_id)
        .await
        .expect_err("fallback consumption outcome is unknown");
    assert!(matches!(error, CodexClientError::HttpJson(error) if error.is_timeout()));
    for server in [&custom_server, &official_server] {
        let requests = server.received_requests().await.expect("consume requests");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&requests[0].body).unwrap(),
            json!({"redeem_request_id": redeem_request_id.to_string()})
        );
    }
}

fn header<'a>(request: &'a wiremock::Request, name: &str) -> Option<&'a str> {
    request
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
}

#[test]
fn reset_credit_error_debug_should_redact_raw_upstream_body() {
    let error = CodexResetCreditsError::Upstream {
        status: 429,
        body: "sensitive-upstream-body".to_owned(),
        retry_after_seconds: Some(17),
    };
    let debug = format!("{error:?}");

    assert!(debug.contains("429"));
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("sensitive-upstream-body"));

    let refresh_error = CodexResetCreditsError::CredentialRefreshRequired {
        upstream_body: Some("sensitive-oauth-body".to_owned()),
    };
    let debug = format!("{refresh_error:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("sensitive-oauth-body"));
}
