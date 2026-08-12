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
            "id_token": "header.e30.signature",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    let tokens = client(&server)
        .refresh("refresh-initial")
        .await
        .expect("bounded response");

    assert_eq!(tokens.access_token.as_deref(), Some("access-rotated"));
    assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-rotated"));
    assert_eq!(tokens.id_token.as_deref(), Some("header.e30.signature"));
    assert_eq!(tokens.expires_in, Some(Duration::from_secs(3600)));
}

#[tokio::test]
async fn refresh_response_should_reject_a_malformed_rotated_id_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "access-rotated",
            "id_token": "not-a-jwt"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let failure = client(&server)
        .refresh("refresh-initial")
        .await
        .expect_err("malformed ID token must not replace the stored token set");

    assert_eq!(failure, RefreshFailure::Transport);
}

#[tokio::test]
async fn refresh_response_should_allow_all_rotated_tokens_to_be_omitted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .expect(1)
        .mount(&server)
        .await;

    let tokens = client(&server)
        .refresh("refresh-initial")
        .await
        .expect("official refresh response fields are optional");

    assert!(tokens.access_token.is_none());
    assert!(tokens.refresh_token.is_none());
    assert!(tokens.id_token.is_none());
    assert!(tokens.expires_in.is_none());
}

#[tokio::test]
async fn refresh_response_keeps_the_upstream_rotated_token_verbatim() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "access-rotated",
            "refresh_token": " ",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    let tokens = client(&server)
        .refresh("refresh-initial")
        .await
        .expect("upstream response is accepted");

    assert_eq!(tokens.refresh_token.as_deref(), Some(" "));
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
async fn authorization_code_exchange_keeps_upstream_access_and_refresh_tokens_verbatim() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": " ",
            "refresh_token": "",
            "id_token": "header.payload.signature"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let tokens = client(&server)
        .exchange_authorization_code(AuthorizationCodeGrant {
            code: SecretString::from("authorization-code"),
            code_verifier: SecretString::from("pkce-verifier"),
        })
        .await
        .expect("token endpoint response is trusted structurally");

    assert_eq!(tokens.secret.access_token.expose_secret(), " ");
    assert_eq!(
        tokens
            .secret
            .refresh_token
            .as_ref()
            .expect("required response field")
            .expose_secret(),
        ""
    );
}

#[tokio::test]
async fn authorization_code_exchange_requires_id_token_and_json_response() {
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

#[tokio::test]
async fn server_error_or_rate_limit_must_stay_transient_even_with_oauth_error_body() {
    // 5xx/429（含 CDN/网关页恰好嵌入 invalid_grant 字样）按状态码判瞬态，
    // 不因正文子串把账号永久终态——正文只在 4xx 时才是权威 OAuth 错误。
    for status in [500, 502, 503, 429] {
        let failure = refresh_failure(
            status,
            r#"{"error":"invalid_grant","error_description":"refresh_token_expired"}"#,
        )
        .await;
        assert_eq!(
            failure,
            RefreshFailure::Transport,
            "status {status} must classify as transient"
        );
    }
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
