use super::*;
use wiremock::{
    matchers::{body_string_contains, method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn admin_account_oauth_authorize_should_return_pkce_auth_url() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-oauth-authorize", 94).await;

    let response = post_oauth_authorize(&app, "req_oauth_authorize").await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let auth_url = body["data"]["authUrl"].as_str().unwrap();
    let url = reqwest::Url::parse(auth_url).unwrap();
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(
        url.as_str().split('?').next().unwrap(),
        "https://auth.openai.com/oauth/authorize"
    );
    assert_eq!(
        params.get("response_type").map(String::as_str),
        Some("code")
    );
    assert_eq!(
        params.get("client_id").map(String::as_str),
        Some("app_EMoamEEZ73f0CkXaXp7hrann")
    );
    assert_eq!(
        params.get("redirect_uri").map(String::as_str),
        Some("http://localhost:1455/auth/callback")
    );
    assert_eq!(
        params.get("scope").map(String::as_str),
        Some("openid profile email offline_access")
    );
    assert_eq!(
        params.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert!(params.get("state").is_some_and(|value| !value.is_empty()));
    assert!(params
        .get("code_challenge")
        .is_some_and(|value| !value.is_empty()));
}

#[tokio::test]
async fn admin_account_oauth_exchange_should_reject_callback_without_state() {
    let server = MockServer::start().await;
    let (app, _state, _pool, _dir) = admin_accounts_test_app_with_oauth_token_endpoint(
        "admin-account-oauth-missing-state",
        95,
        format!("{}/oauth/token", server.uri()),
    )
    .await;
    let authorize =
        response_json(post_oauth_authorize(&app, "req_oauth_missing_state").await).await;
    let session_id = authorize["data"]["sessionId"].as_str().unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/oauth/exchange")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_oauth_missing_state_exchange")
                .body(Body::from(
                    json!({
                        "sessionId": session_id,
                        "callbackUrl": "http://localhost:1455/auth/callback?code=oauth-code"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn admin_account_oauth_exchange_should_import_account_tokens() {
    let server = MockServer::start().await;
    let access_token = test_jwt(
        "chatgpt-oauth",
        Some("user-oauth"),
        Some("oauth@example.com"),
        Some("plus"),
    );
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains(
            "client_id=app_EMoamEEZ73f0CkXaXp7hrann",
        ))
        .and(body_string_contains("code=oauth-code"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
        ))
        .and(body_string_contains("code_verifier="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": access_token,
            "refresh_token": "refresh-oauth"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "account_id": "chatgpt-oauth",
            "user_id": "user-oauth",
            "email": "oauth@example.com",
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "primary_window": {
                    "used_percent": 1,
                    "limit_window_seconds": 18000
                }
            }
        })))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) =
        admin_accounts_test_app_with_api_base_url_and_oauth_token_endpoint(
            "admin-account-oauth-exchange",
            96,
            server.uri(),
            format!("{}/oauth/token", server.uri()),
        )
        .await;
    let authorize = response_json(post_oauth_authorize(&app, "req_oauth_exchange").await).await;
    let session_id = authorize["data"]["sessionId"].as_str().unwrap();
    let auth_url = reqwest::Url::parse(authorize["data"]["authUrl"].as_str().unwrap()).unwrap();
    let state = auth_url
        .query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/oauth/exchange")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_oauth_exchange_done")
                .body(Body::from(
                    json!({
                        "sessionId": session_id,
                        "callbackUrl": format!(
                            "http://localhost:1455/auth/callback?code=oauth-code&state={state}"
                        )
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["imported"], 1);
    let stored: (String, String, String, String) = sqlx::query_as(
        "select email, chatgpt_account_id, access_token, refresh_token from accounts where chatgpt_account_id = $1",
    )
        .bind("chatgpt-oauth")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored.0, "oauth@example.com");
    assert_eq!(stored.1, "chatgpt-oauth");
    assert_eq!(stored.2, access_token);
    assert_eq!(stored.3, "refresh-oauth");
}

async fn post_oauth_authorize(app: &axum::Router, request_id: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/oauth/authorize")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", request_id)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}
