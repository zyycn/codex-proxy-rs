use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::{
    accounts::{
        oauth::{OAuthClient, OAuthConfig, OAuthError},
        token_refresh::TokenRefresher,
    },
    codex::oauth_client::OpenAiOAuthClient,
};

#[tokio::test]
async fn openai_oauth_client_should_exchange_refresh_token_with_form_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh"
        })))
        .mount(&server)
        .await;
    let client = test_oauth_client(&server);

    let tokens = client.refresh("refresh-secret").await.unwrap();

    assert_eq!(tokens.access_token, "new-access");
    assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
    let requests = server.received_requests().await.unwrap();
    let body = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(body.contains("grant_type=refresh_token"));
    assert!(body.contains("client_id=codex-client"));
    assert!(body.contains("refresh_token=refresh-secret"));
}

#[tokio::test]
async fn openai_oauth_client_should_request_device_code_with_form_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/device/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code": "device-secret",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://auth.openai.com/activate",
            "verification_uri_complete": "https://auth.openai.com/activate?user_code=ABCD-EFGH",
            "expires_in": 900,
            "interval": 5
        })))
        .mount(&server)
        .await;
    let client = test_oauth_client(&server);

    let device = client.request_device_code().await.unwrap();

    assert_eq!(device.device_code, "device-secret");
    assert_eq!(device.user_code, "ABCD-EFGH");
    assert_eq!(device.expires_in, 900);
    let requests = server.received_requests().await.unwrap();
    let body = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(body.contains("client_id=codex-client"));
    assert!(body.contains("scope=openid+profile+email+offline_access"));
}

#[tokio::test]
async fn openai_oauth_client_should_exchange_authorization_code_with_pkce_verifier() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "pkce-access",
            "refresh_token": "pkce-refresh"
        })))
        .mount(&server)
        .await;
    let client = test_oauth_client(&server);

    let tokens = client
        .exchange_code(
            "oauth-code",
            "pkce-verifier",
            "http://localhost:1455/auth/callback",
        )
        .await
        .unwrap();

    assert_eq!(tokens.access_token, "pkce-access");
    assert_eq!(tokens.refresh_token.as_deref(), Some("pkce-refresh"));
    let requests = server.received_requests().await.unwrap();
    let body = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(body.contains("grant_type=authorization_code"));
    assert!(body.contains("client_id=codex-client"));
    assert!(body.contains("code=oauth-code"));
    assert!(body.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
    assert!(body.contains("code_verifier=pkce-verifier"));
}

#[tokio::test]
async fn openai_oauth_client_should_map_device_poll_pending_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "authorization_pending",
            "error_description": "authorization is still pending"
        })))
        .mount(&server)
        .await;
    let client = test_oauth_client(&server);

    let error = client.poll_device_token("device-secret").await.unwrap_err();

    assert_eq!(error, OAuthError::AuthorizationPending);
    let requests = server.received_requests().await.unwrap();
    let body = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"));
    assert!(body.contains("device_code=device-secret"));
    assert!(body.contains("client_id=codex-client"));
}

fn test_oauth_client(server: &MockServer) -> OpenAiOAuthClient {
    OpenAiOAuthClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        OAuthConfig {
            client_id: "codex-client".to_string(),
            auth_endpoint: "https://auth.openai.com/oauth/authorize".to_string(),
            device_code_endpoint: format!("{}/oauth/device/code", server.uri()),
            token_endpoint: format!("{}/oauth/token", server.uri()),
        },
    )
}
