use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::{
    upstream::accounts::token_refresh::TokenRefresher,
    upstream::token_client::{OpenAiTokenClient, TokenClientConfig},
};

#[tokio::test]
async fn openai_token_client_should_exchange_refresh_token_with_form_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "new-access",
            "refresh_token": "new-refresh"
        })))
        .mount(&server)
        .await;
    let client = test_token_client(&server);

    let tokens = client.refresh("refresh-secret").await.unwrap();

    assert_eq!(tokens.access_token, "new-access");
    assert_eq!(tokens.refresh_token.as_deref(), Some("new-refresh"));
    let requests = server.received_requests().await.unwrap();
    let body = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(body.contains("grant_type=refresh_token"));
    assert!(body.contains("client_id=codex-client"));
    assert!(body.contains("refresh_token=refresh-secret"));
}

fn test_token_client(server: &MockServer) -> OpenAiTokenClient {
    OpenAiTokenClient::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        TokenClientConfig {
            client_id: "codex-client".to_string(),
            token_endpoint: format!("{}/oauth/token", server.uri()),
        },
    )
}
