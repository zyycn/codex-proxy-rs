use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use secrecy::ExposeSecret;
use serde_json::json;
use tokio::sync::Mutex;
use tower::ServiceExt;

use codex_proxy_rs::{
    codex::accounts::repository::AccountRepository,
    codex::gateway::oauth::{DeviceCode, OAuthError, TokenPair},
    platform::crypto::SecretBox,
};

use crate::support::{
    admin_accounts::{
        admin_accounts_test_app, admin_accounts_test_app_with_oauth_client, import_test_account,
        test_jwt, StaticOAuthClient,
    },
    response_json,
};

#[tokio::test]
async fn admin_auth_status_should_require_admin_session_cookie() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-auth-status-auth.sqlite", 33).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/auth/status")
                .header("x-request-id", "req_auth_status_no_session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_auth_status_no_session");
}

#[tokio::test]
async fn admin_auth_status_should_report_empty_account_pool() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-auth-status-empty.sqlite", 34).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/auth/status")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_auth_status_empty")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["authenticated"], false);
    assert!(body["data"]["user"].is_null());
    assert_eq!(body["data"]["pool"]["total"], 0);
    assert_eq!(body["data"]["pool"]["active"], 0);
}

#[tokio::test]
async fn admin_auth_status_should_report_user_and_pool_summary_without_secrets() {
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-auth-status-summary.sqlite", 35).await;
    import_test_account(&app, "session_1", "acct_auth_status").await;
    import_test_account(&app, "session_1", "acct_auth_disabled").await;
    sqlx::query("update accounts set status = 'disabled' where id = ?")
        .bind("acct_auth_disabled")
        .execute(&pool)
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/auth/status")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_auth_status_summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["authenticated"], true);
    assert_eq!(body["data"]["pool"]["total"], 2);
    assert_eq!(body["data"]["pool"]["active"], 1);
    assert_eq!(body["data"]["pool"]["disabled"], 1);
    assert!(body["data"]["user"].get("accessToken").is_none());
    assert!(body["data"]["user"].get("token").is_none());
    assert!(body["data"]["user"].get("refreshToken").is_none());
}

#[tokio::test]
async fn admin_auth_logout_should_clear_accounts_and_runtime_pool() {
    let (app, state, pool, _dir) = admin_accounts_test_app("admin-auth-logout.sqlite", 36).await;
    import_test_account(&app, "session_1", "acct_auth_logout").await;
    sqlx::query(
        "insert into account_usage (account_id, request_count, input_tokens, output_tokens, cached_tokens) values (?, 2, 3, 4, 1)",
    )
    .bind("acct_auth_logout")
    .execute(&pool)
    .await
    .unwrap();
    let set_cookies = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/acct_auth_logout/cookies")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"cookies":"cf_clearance=secret"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(set_cookies.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/logout")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_auth_logout")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["success"], true);
    assert_eq!(body["data"]["deleted"], 1);
    let account_count: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(account_count.0, 0);
    let usage_count: (i64,) = sqlx::query_as("select count(*) from account_usage")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(usage_count.0, 0);
    let cookie_count: (i64,) = sqlx::query_as("select count(*) from account_cookies")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cookie_count.0, 0);
    assert!(state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .is_none());
}

#[tokio::test]
async fn admin_auth_device_login_should_require_admin_session_cookie() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-auth-device-login-auth.sqlite", 37).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/device-login")
                .header("x-request-id", "req_device_login_no_session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_device_login_no_session");
}

#[tokio::test]
async fn admin_auth_device_login_should_return_openai_device_code() {
    let client = StaticOAuthClient {
        device_response: Ok(DeviceCode {
            device_code: "device-secret".to_string(),
            user_code: "ABCD-EFGH".to_string(),
            verification_uri: "https://auth.openai.com/activate".to_string(),
            verification_uri_complete: "https://auth.openai.com/activate?user_code=ABCD-EFGH"
                .to_string(),
            expires_in: 900,
            interval: 5,
        }),
        poll_response: Err(OAuthError::AuthorizationPending),
        exchange_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_calls: Arc::new(Mutex::new(Vec::new())),
        exchange_calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app_with_oauth_client("admin-auth-device-login.sqlite", 38, client)
            .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/device-login")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_device_login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["userCode"], "ABCD-EFGH");
    assert_eq!(
        body["data"]["verificationUri"],
        "https://auth.openai.com/activate"
    );
    assert_eq!(body["data"]["deviceCode"], "device-secret");
    assert_eq!(body["data"]["expiresIn"], 900);
    assert_eq!(body["data"]["interval"], 5);
}

#[tokio::test]
async fn admin_auth_device_poll_should_return_pending_without_importing_account() {
    let poll_calls = Arc::new(Mutex::new(Vec::new()));
    let client = StaticOAuthClient {
        device_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_response: Err(OAuthError::SlowDown),
        exchange_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_calls: poll_calls.clone(),
        exchange_calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, _state, pool, _dir) =
        admin_accounts_test_app_with_oauth_client("admin-auth-device-pending.sqlite", 39, client)
            .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/auth/device-poll/device-secret")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_device_pending")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["pending"], true);
    assert_eq!(body["data"]["success"], false);
    assert_eq!(body["data"]["code"], "slow_down");
    assert_eq!(*poll_calls.lock().await, vec!["device-secret".to_string()]);
    let account_count: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(account_count.0, 0);
}

#[tokio::test]
async fn admin_auth_device_poll_should_import_tokens_without_returning_secrets() {
    let token = test_jwt(
        Some("device-account"),
        Some("device-user"),
        Some("device@example.com"),
        Some("plus"),
        3600,
    );
    let poll_calls = Arc::new(Mutex::new(Vec::new()));
    let client = StaticOAuthClient {
        device_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_response: Ok(TokenPair {
            access_token: token.clone(),
            refresh_token: Some("device-refresh-secret".to_string()),
        }),
        exchange_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_calls: poll_calls.clone(),
        exchange_calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, state, pool, _dir) =
        admin_accounts_test_app_with_oauth_client("admin-auth-device-success.sqlite", 40, client)
            .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/auth/device-poll/device-success")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_device_success")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["success"], true);
    assert_eq!(body["data"]["pending"], false);
    assert!(body["data"].get("accessToken").is_none());
    assert!(body["data"].get("refreshToken").is_none());
    assert_eq!(*poll_calls.lock().await, vec!["device-success".to_string()]);

    let stored = AccountRepository::new(pool.clone(), SecretBox::new([40; 32]))
        .find_by_chatgpt_identity("device-account", Some("device-user"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.email.as_deref(), Some("device@example.com"));
    assert_eq!(stored.account_id.as_deref(), Some("device-account"));
    assert!(!stored.access_token.expose_secret().is_empty());
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "device-refresh-secret"
    );
    let raw: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where account_id = ?",
    )
    .bind("device-account")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!raw.0.contains(&token));
    assert!(!raw.1.contains("device-refresh-secret"));
    let acquired = state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .unwrap();
    assert_eq!(acquired.email.as_deref(), Some("device@example.com"));
}

#[tokio::test]
async fn admin_auth_login_start_should_require_admin_session_cookie() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-auth-login-start-auth.sqlite", 41).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/login-start")
                .header("x-request-id", "req_login_start_no_session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_login_start_no_session");
}

#[tokio::test]
async fn admin_auth_login_start_should_return_pkce_auth_url_and_state() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-auth-login-start.sqlite", 42).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/login-start")
                .header("cookie", "cpr_admin_session=session_1")
                .header("host", "127.0.0.1:8080")
                .header("x-request-id", "req_login_start")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let state = body["data"]["state"].as_str().unwrap();
    let auth_url = body["data"]["authUrl"].as_str().unwrap();
    assert_eq!(state.len(), 32);
    assert!(auth_url.starts_with("https://auth.openai.com/oauth/authorize?"));
    assert!(auth_url.contains("response_type=code"));
    assert!(auth_url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
    assert!(
        auth_url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fopenai%2Fcallback")
    );
    assert!(auth_url.contains("scope=openid%20profile%20email%20offline_access"));
    assert!(auth_url.contains("code_challenge_method=S256"));
    assert!(auth_url.contains("originator=codex_cli_rs"));
    assert!(auth_url.contains(&format!("state={state}")));
}

#[tokio::test]
async fn admin_auth_code_relay_should_exchange_code_and_import_account() {
    let token = test_jwt(
        Some("pkce-account"),
        Some("pkce-user"),
        Some("pkce@example.com"),
        Some("plus"),
        3600,
    );
    let exchange_calls = Arc::new(Mutex::new(Vec::new()));
    let client = StaticOAuthClient {
        device_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_response: Err(OAuthError::AuthorizationPending),
        exchange_response: Ok(TokenPair {
            access_token: token.clone(),
            refresh_token: Some("pkce-refresh-secret".to_string()),
        }),
        poll_calls: Arc::new(Mutex::new(Vec::new())),
        exchange_calls: exchange_calls.clone(),
    };
    let (app, state, pool, _dir) =
        admin_accounts_test_app_with_oauth_client("admin-auth-code-relay.sqlite", 43, client).await;
    let start = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/login-start")
                .header("cookie", "cpr_admin_session=session_1")
                .header("host", "127.0.0.1:8080")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let start_body = response_json(start).await;
    let state_value = start_body["data"]["state"].as_str().unwrap();
    let callback_url =
        format!("http://localhost:1455/auth/openai/callback?code=oauth-code&state={state_value}");

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/code-relay")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_code_relay")
                .body(Body::from(
                    json!({ "callbackUrl": callback_url }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["success"], true);
    assert!(body["data"].get("accessToken").is_none());
    assert!(body["data"].get("refreshToken").is_none());
    let calls = exchange_calls.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].code, "oauth-code");
    assert!(!calls[0].code_verifier.is_empty());
    assert_eq!(
        calls[0].redirect_uri,
        "http://localhost:1455/auth/openai/callback"
    );
    drop(calls);
    let stored = AccountRepository::new(pool.clone(), SecretBox::new([43; 32]))
        .find_by_chatgpt_identity("pkce-account", Some("pkce-user"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.email.as_deref(), Some("pkce@example.com"));
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "pkce-refresh-secret"
    );
    let acquired = state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .unwrap();
    assert_eq!(acquired.account_id.as_deref(), Some("pkce-account"));
}

#[tokio::test]
async fn admin_auth_code_relay_should_reject_invalid_callback_url() {
    let client = StaticOAuthClient {
        device_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_response: Err(OAuthError::AuthorizationPending),
        exchange_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_calls: Arc::new(Mutex::new(Vec::new())),
        exchange_calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, _state, _pool, _dir) = admin_accounts_test_app_with_oauth_client(
        "admin-auth-code-relay-invalid.sqlite",
        44,
        client,
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/code-relay")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_code_relay_invalid")
                .body(Body::from(
                    json!({ "callbackUrl": "not a url" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40001);
}

#[tokio::test]
async fn admin_auth_callback_should_exchange_code_and_redirect_to_return_host() {
    let token = test_jwt(
        Some("callback-account"),
        Some("callback-user"),
        Some("callback@example.com"),
        Some("plus"),
        3600,
    );
    let exchange_calls = Arc::new(Mutex::new(Vec::new()));
    let client = StaticOAuthClient {
        device_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_response: Err(OAuthError::AuthorizationPending),
        exchange_response: Ok(TokenPair {
            access_token: token,
            refresh_token: Some("callback-refresh-secret".to_string()),
        }),
        poll_calls: Arc::new(Mutex::new(Vec::new())),
        exchange_calls: exchange_calls.clone(),
    };
    let (app, _state, pool, _dir) =
        admin_accounts_test_app_with_oauth_client("admin-auth-callback.sqlite", 45, client).await;
    let start = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/login-start")
                .header("cookie", "cpr_admin_session=session_1")
                .header("host", "codex.local:1455")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let start_body = response_json(start).await;
    let state_value = start_body["data"]["state"].as_str().unwrap();
    let callback_path = format!("/auth/openai/callback?code=callback-code&state={state_value}");

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(callback_path)
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers().get("location").unwrap(),
        "http://codex.local:1455/"
    );
    assert_eq!(exchange_calls.lock().await[0].code, "callback-code");
    let stored = AccountRepository::new(pool.clone(), SecretBox::new([45; 32]))
        .find_by_chatgpt_identity("callback-account", Some("callback-user"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.email.as_deref(), Some("callback@example.com"));
}
