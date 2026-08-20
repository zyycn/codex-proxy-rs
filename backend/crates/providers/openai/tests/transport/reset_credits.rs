use chrono::{TimeZone as _, Utc};
use provider_openai::credential::CodexResetCreditsError;
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use provider_openai::transport::{
    CodexBackendClient, CodexClientError, CodexRequestContext, MAX_CODEX_RESET_CREDITS_BODY_BYTES,
};
use reqwest::{Method, StatusCode};
use serde_json::json;
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
            originator: "Codex Desktop".to_owned(),
            codex_version: "0.115.0-alpha.11".to_owned(),
            desktop_version: "26.818.21641".to_owned(),
            desktop_build: "6849".to_owned(),
            os_type: "Mac OS".to_owned(),
            os_version: "15.5.0".to_owned(),
            arch: "arm64".to_owned(),
            terminal: "xterm-256color".to_owned(),
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
async fn list_should_match_official_desktop_surface_profile() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/wham/rate-limit-reset-credits"))
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
    assert_eq!(header(request, "originator"), Some("Codex Desktop"));
    assert_eq!(header(request, "oai-language"), Some("en"));
    assert_eq!(
        header(request, "user-agent"),
        Some("Codex Desktop/26.818.21641 (Mac OS; arm64)")
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
        .and(path("/wham/rate-limit-reset-credits/consume"))
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
async fn consume_should_preserve_raw_upstream_error_and_never_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/wham/rate-limit-reset-credits/consume"))
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
        .and(path("/wham/rate-limit-reset-credits"))
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
