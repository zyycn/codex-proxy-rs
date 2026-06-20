use super::*;

#[tokio::test]
async fn admin_auth_login_start_should_return_pkce_auth_url_and_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-auth-login-start.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([95u8; 32]),
        ApiKeyHasher::new([96u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
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
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let state = body["data"]["state"].as_str().unwrap();
    let auth_url = body["data"]["authUrl"].as_str().unwrap();
    assert_eq!(state.len(), 32);
    assert!(auth_url.starts_with("https://auth.openai.com/oauth/authorize?"));
    assert!(auth_url.contains("response_type=code"));
    assert!(auth_url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
    assert!(auth_url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
    assert!(auth_url.contains("scope=openid%20profile%20email%20offline_access"));
    assert!(auth_url.contains("code_challenge_method=S256"));
    assert!(auth_url.contains("originator=codex_cli_rs"));
    assert!(auth_url.contains(&format!("state={state}")));
}

#[tokio::test]
async fn admin_auth_status_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-auth-status-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([105u8; 32]),
        ApiKeyHasher::new([106u8; 32]),
    );
    let app = router::router().with_state(state);

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
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_auth_status_no_session");
}

#[tokio::test]
async fn admin_auth_status_should_report_user_and_pool_summary_without_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-auth-status-summary.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([107u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_auth_active".to_string(),
            email: Some("active-auth@example.com".to_string()),
            account_id: Some("auth-active-account".to_string()),
            user_id: Some("auth-active-user".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new("access-auth-active".to_string().into()),
            refresh_token: Some(SecretString::new("refresh-auth-active".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_auth_disabled".to_string(),
            email: Some("disabled-auth@example.com".to_string()),
            account_id: Some("auth-disabled-account".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-auth-disabled".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Disabled,
        },
    )
    .await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        secret_box,
        ApiKeyHasher::new([108u8; 32]),
    );
    let app = router::router().with_state(state);

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
    assert_eq!(body["data"]["user"]["email"], "active-auth@example.com");
    assert!(body["data"]["user"].get("accessToken").is_none());
    assert!(body["data"]["user"].get("token").is_none());
    assert!(body["data"]["user"].get("refreshToken").is_none());
}

#[tokio::test]
async fn admin_auth_logout_should_clear_accounts_usage_cookies_and_runtime_pool() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-auth-logout.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([109u8; 32]);
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([110u8; 32]),
    );
    let app = router::router().with_state(state.clone());
    let imported = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_auth_logout",
                            "email": "logout@example.com",
                            "planType": "plus",
                            "token": "access-auth-logout",
                            "refreshToken": "refresh-auth-logout",
                            "status": "active"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(imported.status(), StatusCode::OK);
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
    let usage_count: (i64,) = sqlx::query_as("select count(*) from account_usage")
        .fetch_one(&pool)
        .await
        .unwrap();
    let cookie_count: (i64,) = sqlx::query_as("select count(*) from account_cookies")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(account_count.0, 0);
    assert_eq!(usage_count.0, 0);
    assert_eq!(cookie_count.0, 0);
    assert!(state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .is_none());
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
        admin_accounts_test_app_with_oauth_client("admin-auth-device-login.sqlite", 111, client)
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
async fn admin_auth_login_start_should_use_configured_oauth_authorize_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir
        .path()
        .join("admin-auth-login-start-configured-oauth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let mut config = test_config(url);
    config.auth.oauth_client_id = "app_configured_client".to_string();
    config.auth.oauth_auth_endpoint = "https://auth.example.test/oauth/authorize".to_string();
    config.auth.oauth_token_endpoint = "https://auth.example.test/oauth/token".to_string();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        config,
        pool,
        SecretBox::new([114u8; 32]),
        ApiKeyHasher::new([115u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/login-start")
                .header("cookie", "cpr_admin_session=session_1")
                .header("host", "127.0.0.1:8080")
                .header("x-request-id", "req_login_start_configured_oauth")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let auth_url = body["data"]["authUrl"].as_str().unwrap();

    assert!(auth_url.starts_with("https://auth.example.test/oauth/authorize?"));
    assert!(auth_url.contains("client_id=app_configured_client"));
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
        117,
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
                .body(Body::from(json!({"callbackUrl": "not a url"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_auth_callback_should_exchange_code_and_redirect_to_return_host() {
    let token = test_jwt(
        "callback-account",
        Some("callback-user"),
        Some("callback@example.com"),
        Some("plus"),
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
        admin_accounts_test_app_with_oauth_client("admin-auth-callback.sqlite", 118, client).await;
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
    let callback_path = format!("/auth/callback?code=callback-code&state={state_value}");

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
    let count: (i64,) = sqlx::query_as("select count(*) from accounts where account_id = ?")
        .bind("callback-account")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
}

#[tokio::test]
async fn admin_accounts_health_check_should_refresh_oauth_without_touching_codex_backend() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&upstream)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-health-refresh.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([124u8; 32]);
    for (id, refresh_token) in [
        ("acct_health_alive", "refresh-acct_health_alive"),
        ("acct_health_dead", "refresh-acct_health_dead"),
    ] {
        seed_encrypted_account(
            &pool,
            secret_box.clone(),
            NewAccount {
                id: id.to_string(),
                email: Some(format!("{id}@example.com")),
                account_id: Some(format!("old-{id}")),
                user_id: None,
                label: None,
                plan_type: None,
                access_token: SecretString::new(format!("old-access-{id}").into()),
                refresh_token: Some(SecretString::new(refresh_token.to_string().into())),
                access_token_expires_at: None,
                status: AccountStatus::Active,
            },
        )
        .await;
    }
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(url);
    config.api.base_url = upstream.uri();
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        config,
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([124u8; 32]),
        HealthCheckTokenRefresher {
            calls: calls.clone(),
        },
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/health-check")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_health_refresh")
                .body(Body::from(
                    json!({
                        "ids": ["acct_health_alive", "acct_health_dead"],
                        "concurrency": 2,
                        "staggerMs": 500
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["summary"]["total"], 2);
    assert_eq!(body["data"]["summary"]["alive"], 1);
    assert_eq!(body["data"]["summary"]["dead"], 1);
    assert_eq!(body["data"]["summary"]["skipped"], 0);
    let mut refresh_calls = calls.lock().await.clone();
    refresh_calls.sort();
    assert_eq!(
        refresh_calls,
        vec![
            "refresh-acct_health_alive".to_string(),
            "refresh-acct_health_dead".to_string(),
        ]
    );
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("new-health-access"));
    assert!(!serialized.contains("new-health-refresh"));
    let dead_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_health_dead")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(dead_status.0, "disabled");
}
