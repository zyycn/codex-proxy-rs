use std::time::Duration;

use provider_openai::credential::token_client::{
    AuthorizationCodeExchangeError, AuthorizationCodeExchanger, AuthorizationCodeGrant,
    OpenAiTokenClient, RefreshFailure, TokenClientConfig, TokenRefresher,
};
use secrecy::{ExposeSecret, SecretString};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> OpenAiTokenClient {
    OpenAiTokenClient::new(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("test HTTP client"),
        TokenClientConfig {
            client_id: "test-public-client".to_owned(),
            token_endpoint: format!("{}/oauth/token", server.uri()),
        },
    )
}

#[tokio::test]
async fn oversized_chunked_oauth_response_should_fail_closed_and_redact_body() {
    let server = MockServer::start().await;
    let marker = "oauth-secret-response-marker";
    let body = format!(
        "{{\"error\":\"invalid_grant\",\"marker\":\"{marker}\",\"padding\":\"{}\"}}",
        "x".repeat(70 * 1024)
    );
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(
            ResponseTemplate::new(400)
                .insert_header("transfer-encoding", "chunked")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let failure = client(&server)
        .refresh("refresh-secret-request-marker")
        .await
        .expect_err("oversized response must fail closed before body classification");
    let diagnostic = format!("{failure:?} {failure}");

    assert_eq!(failure, RefreshFailure::Transport);
    assert!(!diagnostic.contains(marker));
    assert!(!diagnostic.contains("refresh-secret-request-marker"));
}

#[tokio::test]
async fn bounded_oauth_response_should_parse_lifetime_and_rotated_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "access-rotated",
            "refresh_token": "refresh-rotated",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    let tokens = client(&server)
        .refresh("refresh-initial")
        .await
        .expect("bounded response");

    assert_eq!(tokens.access_token, "access-rotated");
    assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-rotated"));
    assert_eq!(tokens.expires_in, Duration::from_secs(3600));
}

#[tokio::test]
async fn refresh_should_exchange_the_official_form_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    client(&server)
        .refresh("refresh secret")
        .await
        .expect("refresh succeeds");
    let requests = server.received_requests().await.expect("received request");
    let body = String::from_utf8(requests[0].body.clone()).expect("form body is UTF-8");

    assert!(body.contains("grant_type=refresh_token"));
    assert!(body.contains("client_id=test-public-client"));
    assert!(body.contains("refresh_token=refresh+secret"));
}

#[tokio::test]
async fn authorization_code_exchange_should_require_bounded_oidc_token_set_and_pkce_form() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "header.access.signature",
            "refresh_token": "refresh-token",
            "id_token": "header.id.signature",
            "token_type": "Bearer",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    let tokens = client(&server)
        .exchange_authorization_code(AuthorizationCodeGrant {
            code: SecretString::from("authorization code"),
            code_verifier: SecretString::from("pkce-verifier-secret"),
        })
        .await
        .expect("exchange bounded OIDC token set");
    let requests = server.received_requests().await.expect("received request");
    let body = String::from_utf8(requests[0].body.clone()).expect("form body");

    assert_eq!(
        tokens.secret.access_token.expose_secret(),
        "header.access.signature"
    );
    assert_eq!(tokens.id_token.expose_secret(), "header.id.signature");
    assert!(body.contains("grant_type=authorization_code"));
    assert!(body.contains("client_id=test-public-client"));
    assert!(body.contains("code=authorization+code"));
    assert!(body.contains("code_verifier=pkce-verifier-secret"));
    assert!(body.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
}

#[tokio::test]
async fn authorization_code_exchange_should_reject_missing_nonce_carrier_or_non_json_success() {
    let missing_id = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "header.access.signature",
            "refresh_token": "refresh-token",
            "token_type": "Bearer",
            "expires_in": 3600
        })))
        .mount(&missing_id)
        .await;
    let non_json = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
        .mount(&non_json)
        .await;

    for server in [&missing_id, &non_json] {
        let error = client(server)
            .exchange_authorization_code(AuthorizationCodeGrant {
                code: SecretString::from("code-secret"),
                code_verifier: SecretString::from("verifier-secret"),
            })
            .await
            .expect_err("invalid token response fails closed");
        assert_eq!(error, AuthorizationCodeExchangeError::Rejected);
    }
}

#[tokio::test]
async fn refresh_token_reuse_should_be_classified_as_invalid_grant() {
    let failure = refresh_failure(
        400,
        r#"{"error":"invalid_grant","error_description":"refresh_token_reused"}"#,
    )
    .await;

    assert_eq!(failure, RefreshFailure::InvalidGrant);
}

#[tokio::test]
async fn deactivated_account_should_be_classified_as_banned() {
    let failure = refresh_failure(
        403,
        r#"{"error":"access_denied","error_description":"account has been deactivated"}"#,
    )
    .await;

    assert_eq!(failure, RefreshFailure::Banned);
}

#[tokio::test]
async fn generic_banned_text_should_not_impersonate_the_deactivation_contract() {
    let failure = refresh_failure(403, "account is banned").await;

    assert_eq!(failure, RefreshFailure::Transport);
}

#[tokio::test]
async fn unregistered_disabled_account_text_should_remain_a_transport_failure() {
    let failure = refresh_failure(400, "account disabled").await;

    assert_eq!(failure, RefreshFailure::Transport);
}

#[tokio::test]
async fn quota_text_should_not_disable_the_oauth_credential() {
    let failure = refresh_failure(400, "quota exceeded").await;

    assert_eq!(failure, RefreshFailure::Transport);
}

#[tokio::test]
async fn token_revoked_text_without_invalid_grant_should_remain_temporary() {
    let failure = refresh_failure(400, "token_revoked").await;

    assert_eq!(failure, RefreshFailure::Transport);
}

async fn refresh_failure(status: u16, body: &str) -> RefreshFailure {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .expect(1)
        .mount(&server)
        .await;

    client(&server)
        .refresh("refresh-secret")
        .await
        .expect_err("refresh must fail")
}
