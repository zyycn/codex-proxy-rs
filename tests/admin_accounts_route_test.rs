use std::{collections::BTreeMap, fs, sync::Arc};

use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::Utc;
use secrecy::ExposeSecret;
use serde_json::{json, Value};
use sqlx::Row;
use tokio::sync::Mutex;
use tower::ServiceExt;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

use codex_proxy_rs::{
    accounts::repository::AccountRepository,
    app::build_router,
    auth::{
        api_key::ApiKeyHasher,
        oauth::{DeviceCode, OAuthClient, OAuthError},
        refresh::{RefreshFailure, TokenRefresher},
        token::TokenPair,
    },
    config::{
        AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, ModelConfig,
        QuotaConfig, QuotaWarningThresholds, SecurityConfig, ServerConfig, TlsConfig,
        UsageStatsConfig,
    },
    crypto::SecretBox,
    state::AppState,
    storage::db::connect_sqlite,
};

#[derive(Clone)]
struct StaticTokenRefresher {
    result: Result<TokenPair, RefreshFailure>,
    calls: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
struct FailingTokenRefresher {
    failure: RefreshFailure,
}

#[derive(Clone)]
struct StaticOAuthClient {
    device_response: Result<DeviceCode, OAuthError>,
    poll_response: Result<TokenPair, OAuthError>,
    exchange_response: Result<TokenPair, OAuthError>,
    poll_calls: Arc<Mutex<Vec<String>>>,
    exchange_calls: Arc<Mutex<Vec<ExchangeCall>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExchangeCall {
    code: String,
    code_verifier: String,
    redirect_uri: String,
}

#[async_trait]
impl TokenRefresher for StaticTokenRefresher {
    async fn refresh(&self, refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        self.calls.lock().await.push(refresh_token.to_string());
        self.result.clone()
    }
}

#[async_trait]
impl TokenRefresher for FailingTokenRefresher {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        Err(self.failure)
    }
}

#[async_trait]
impl TokenRefresher for StaticOAuthClient {
    async fn refresh(&self, _refresh_token: &str) -> Result<TokenPair, RefreshFailure> {
        Err(RefreshFailure::Transport)
    }
}

#[async_trait]
impl OAuthClient for StaticOAuthClient {
    async fn exchange_code(
        &self,
        code: &str,
        code_verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenPair, OAuthError> {
        self.exchange_calls.lock().await.push(ExchangeCall {
            code: code.to_string(),
            code_verifier: code_verifier.to_string(),
            redirect_uri: redirect_uri.to_string(),
        });
        self.exchange_response.clone()
    }

    async fn request_device_code(&self) -> Result<DeviceCode, OAuthError> {
        self.device_response.clone()
    }

    async fn poll_device_token(&self, device_code: &str) -> Result<TokenPair, OAuthError> {
        self.poll_calls.lock().await.push(device_code.to_string());
        self.poll_response.clone()
    }
}

fn test_config(database_url: String) -> AppConfig {
    AppConfig {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        api: ApiConfig {
            base_url: "https://chatgpt.com/backend-api".to_string(),
        },
        model: ModelConfig {
            default_model: "gpt-5.5".to_string(),
            default_reasoning_effort: None,
            service_tier: None,
            aliases: BTreeMap::new(),
        },
        auth: AuthConfig {
            refresh_margin_seconds: 300,
            refresh_enabled: true,
            refresh_concurrency: 2,
            max_concurrent_per_account: 3,
            request_interval_ms: 50,
            rotation_strategy: "least_used".to_string(),
            tier_priority: Vec::new(),
        },
        quota: QuotaConfig {
            refresh_interval_minutes: 5,
            warning_thresholds: QuotaWarningThresholds {
                primary: vec![80, 90],
                secondary: vec![80, 90],
            },
            skip_exhausted: true,
        },
        usage_stats: UsageStatsConfig {
            history_retention_days: None,
        },
        database: DatabaseConfig { url: database_url },
        security: SecurityConfig {
            master_key_file: "data/master.key".to_string(),
            api_key_pepper_file: "data/api-key-pepper.key".to_string(),
        },
        tls: TlsConfig {
            force_http11: false,
        },
        admin: AdminConfig {
            session_ttl_minutes: 1440,
        },
        logging: LoggingConfig {
            directory: "logs".to_string(),
            max_file_bytes: 10_485_760,
            retention_days: 14,
            enabled: false,
            capacity: 2_000,
            capture_body: false,
        },
    }
}

fn test_config_with_base_url(database_url: String, base_url: String) -> AppConfig {
    AppConfig {
        api: ApiConfig { base_url },
        ..test_config(database_url)
    }
}

#[tokio::test]
async fn admin_accounts_import_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool,
        SecretBox::new([11u8; 32]),
    ));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("x-request-id", "req_accounts")
                .body(Body::from(r#"{"accounts":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_accounts");
}

#[tokio::test]
async fn admin_auth_status_should_require_admin_session_cookie() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-auth-status-auth.sqlite", 33).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/auth/status")
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
                .uri("/admin/auth/status")
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
                .uri("/admin/auth/status")
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
                .uri("/admin/accounts/acct_auth_logout/cookies")
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
                .uri("/admin/auth/logout")
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
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
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
                .uri("/admin/auth/device-login")
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
                .uri("/admin/auth/device-login")
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
                .uri("/admin/auth/device-poll/device-secret")
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
                .uri("/admin/auth/device-poll/device-success")
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
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
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
                .uri("/admin/auth/login-start")
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
                .uri("/admin/auth/login-start")
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
    assert!(auth_url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
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
                .uri("/admin/auth/login-start")
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
        format!("http://localhost:1455/auth/callback?code=oauth-code&state={state_value}");

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/auth/code-relay")
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
    assert_eq!(calls[0].redirect_uri, "http://localhost:1455/auth/callback");
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
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
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
                .uri("/admin/auth/code-relay")
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
                .uri("/admin/auth/login-start")
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
    let stored = AccountRepository::new(pool.clone(), SecretBox::new([45; 32]))
        .find_by_chatgpt_identity("callback-account", Some("callback-user"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.email.as_deref(), Some("callback@example.com"));
}

#[tokio::test]
async fn admin_accounts_import_should_store_tokens_encrypted_and_list_sanitized_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool.clone(),
        SecretBox::new([12u8; 32]),
    ));
    let import_body = json!({
        "accounts": [{
            "id": "acct_imported",
            "email": "user@example.com",
            "accountId": "chatgpt-account",
            "userId": "chatgpt-user",
            "label": "primary",
            "planType": "plus",
            "token": "access-secret",
            "refreshToken": "refresh-secret",
            "status": "active"
        }]
    });

    let import_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts")
                .body(Body::from(import_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(import_response.status(), StatusCode::OK);
    let body = response_json(import_response).await;
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["skipped"], 0);
    let stored: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where id = ?",
    )
    .bind("acct_imported")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(stored.0.starts_with("v1:"));
    assert!(!stored.0.contains("access-secret"));
    assert!(stored.1.starts_with("v1:"));
    assert!(!stored.1.contains("refresh-secret"));

    let list_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(list_response.status(), StatusCode::OK);
    let body = response_json(list_response).await;
    assert_eq!(body["data"][0]["id"], "acct_imported");
    assert_eq!(body["data"][0]["email"], "user@example.com");
    assert!(body["data"][0].get("token").is_none());
    assert!(body["data"][0].get("refreshToken").is_none());
    assert_eq!(body["page"]["limit"], 10);
}

#[tokio::test]
async fn admin_accounts_import_should_accept_sub2api_oauth_export_and_mark_format() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-sub2api.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool.clone(),
        SecretBox::new([14u8; 32]),
    ));
    let import_body = json!({
        "type": "sub2api-data",
        "version": 1,
        "proxies": [],
        "accounts": [
            {
                "name": "Sub2API Team",
                "platform": "openai",
                "type": "oauth",
                "credentials": {
                    "access_token": "Bearer sub2api-access-secret",
                    "refresh_token": "rt_sub2api",
                    "email": "team@example.com",
                    "chatgpt_account_id": "chatgpt-account",
                    "chatgpt_user_id": "chatgpt-user",
                    "plan_type": "team"
                },
                "concurrency": 0,
                "priority": 0
            },
            {
                "name": "Other Provider",
                "platform": "anthropic",
                "type": "oauth",
                "credentials": {
                    "access_token": "ignored-secret"
                }
            }
        ]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_sub2api")
                .body(Body::from(import_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["sourceFormat"], "sub2api");
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["skipped"], 0);
    let stored: (String, String, String, String, String) =
        sqlx::query_as("select email, account_id, user_id, label, plan_type from accounts")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored.0, "team@example.com");
    assert_eq!(stored.1, "chatgpt-account");
    assert_eq!(stored.2, "chatgpt-user");
    assert_eq!(stored.3, "Sub2API Team");
    assert_eq!(stored.4, "team");
}

#[tokio::test]
async fn admin_accounts_import_should_accept_sub2api_native_account_export_without_proxy_data() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-sub2api-native.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool,
        SecretBox::new([15u8; 32]),
    ));
    let import_body = json!({
        "accounts": [{
            "id": "acct_sub2api_native",
            "token": "native-access-secret",
            "refreshToken": "native-refresh-secret",
            "email": "native@example.com",
            "accountId": "native-account",
            "userId": "native-user",
            "label": "Native Sub2API",
            "planType": "plus",
            "proxyApiKey": "ignored-proxy-secret",
            "status": "active",
            "usage": {
                "request_count": 1,
                "input_tokens": 0,
                "output_tokens": 0,
                "cached_tokens": 0
            },
            "cachedQuota": null,
            "quotaVerifyRequired": false
        }]
    });

    let import_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_sub2api_native")
                .body(Body::from(import_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(import_response.status(), StatusCode::OK);
    let body = response_json(import_response).await;
    assert_eq!(body["data"]["sourceFormat"], "sub2api");
    assert_eq!(body["data"]["imported"], 1);

    let list_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_sub2api_native_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = response_json(list_response).await;
    assert_eq!(list["data"][0]["id"], "acct_sub2api_native");
    assert_eq!(list["data"][0]["planType"], "plus");
    assert!(list["data"][0].get("proxyApiKey").is_none());
    assert!(list["data"][0].get("usage").is_none());
}

#[tokio::test]
async fn admin_accounts_list_should_not_decrypt_account_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "insert into accounts (id, email, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_corrupt")
    .bind("user@example.com")
    .bind("not-a-secret-box-cipher")
    .bind("active")
    .bind(&now)
    .bind(&now)
    .execute(&pool)
    .await
    .unwrap();
    let app = build_router(AppState::with_pool_and_secret_box(
        test_config(url),
        pool,
        SecretBox::new([13u8; 32]),
    ));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"][0]["id"], "acct_corrupt");
}

#[tokio::test]
async fn admin_account_label_should_update_and_clear_label() {
    let (app, state, pool, _dir) = admin_accounts_test_app("admin-account-label.sqlite", 16).await;
    import_test_account(&app, "session_1", "acct_label").await;

    let set_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/accounts/acct_label/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_label")
                .body(Body::from(r#"{"label":"Team Alpha"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(set_response.status(), StatusCode::OK);
    let body = response_json(set_response).await;
    assert_eq!(body["data"]["label"], "Team Alpha");

    let clear_response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/accounts/acct_label/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":null}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(clear_response.status(), StatusCode::OK);
    let body = response_json(clear_response).await;
    assert!(body["data"]["label"].is_null());

    let stored: (Option<String>,) = sqlx::query_as("select label from accounts where id = ?")
        .bind("acct_label")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored.0, None);
    assert!(state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .is_some());
}

#[tokio::test]
async fn admin_account_label_should_reject_too_long_or_missing_account() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-label-invalid.sqlite", 17).await;
    import_test_account(&app, "session_1", "acct_label_invalid").await;
    let long_label = "x".repeat(65);

    let too_long = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/accounts/acct_label_invalid/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({ "label": long_label }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(too_long.status(), StatusCode::BAD_REQUEST);

    let missing = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/accounts/missing/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":"Team Alpha"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_account_status_should_update_database_and_runtime_pool() {
    let (app, state, pool, _dir) = admin_accounts_test_app("admin-account-status.sqlite", 18).await;
    import_test_account(&app, "session_1", "acct_status").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/accounts/acct_status/status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_status")
                .body(Body::from(r#"{"status":"disabled"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["status"], "disabled");

    let stored: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_status")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored.0, "disabled");
    assert!(state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .is_none());
}

#[tokio::test]
async fn admin_account_delete_should_remove_database_row_and_runtime_pool_entry() {
    let (app, state, pool, _dir) = admin_accounts_test_app("admin-account-delete.sqlite", 19).await;
    import_test_account(&app, "session_1", "acct_delete").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/accounts/acct_delete")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_delete")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["deleted"], true);

    let row_count: (i64,) = sqlx::query_as("select count(*) from accounts where id = ?")
        .bind("acct_delete")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row_count.0, 0);
    assert!(state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .is_none());

    let missing = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/accounts/acct_delete")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_accounts_batch_delete_should_delete_found_accounts_and_report_missing_ids() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-accounts-batch-delete.sqlite", 20).await;
    import_test_account(&app, "session_1", "acct_batch_delete_a").await;
    import_test_account(&app, "session_1", "acct_batch_delete_b").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/batch-delete")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": ["acct_batch_delete_a", "ghost", "acct_batch_delete_b"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["deleted"], 2);
    assert_eq!(body["data"]["notFound"], json!(["ghost"]));

    let row_count: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row_count.0, 0);
    assert!(state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .is_none());

    let empty = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/batch-delete")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"ids":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_accounts_batch_status_should_update_found_accounts_and_reject_invalid_status() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-accounts-batch-status.sqlite", 21).await;
    import_test_account(&app, "session_1", "acct_batch_status_a").await;
    import_test_account(&app, "session_1", "acct_batch_status_b").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/batch-status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": ["acct_batch_status_a", "ghost"],
                        "status": "disabled"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["updated"], 1);
    assert_eq!(body["data"]["notFound"], json!(["ghost"]));

    let statuses =
        sqlx::query_as::<_, (String, String)>("select id, status from accounts order by id asc")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        statuses,
        vec![
            ("acct_batch_status_a".to_string(), "disabled".to_string()),
            ("acct_batch_status_b".to_string(), "active".to_string())
        ]
    );
    let acquired = state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .unwrap();
    assert_eq!(acquired.id, "acct_batch_status_b");

    let invalid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/batch-status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": ["acct_batch_status_a"],
                        "status": "expired"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let empty = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/batch-status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"ids":[],"status":"active"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_account_cookies_should_set_get_and_delete_encrypted_cookie_header() {
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-account-cookies.sqlite", 22).await;
    import_test_account(&app, "session_1", "acct_cookies").await;

    let set_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/acct_cookies/cookies")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    r#"{"cookies":"cf_clearance=clear-secret; __cf_bm=bm-secret"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(set_response.status(), StatusCode::OK);
    let body = response_json(set_response).await;
    assert_eq!(
        body["data"]["cookies"],
        "__cf_bm=bm-secret; cf_clearance=clear-secret"
    );

    let stored = sqlx::query_as::<_, (String, String)>(
        "select name, value_cipher from account_cookies where account_id = ? order by name asc",
    )
    .bind("acct_cookies")
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(stored.len(), 2);
    assert!(stored.iter().all(|(_, cipher)| cipher.starts_with("v1:")));
    assert!(stored
        .iter()
        .all(|(_, cipher)| !cipher.contains("clear-secret") && !cipher.contains("bm-secret")));

    let get_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/acct_cookies/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_response.status(), StatusCode::OK);
    let body = response_json(get_response).await;
    assert_eq!(
        body["data"]["cookies"],
        "__cf_bm=bm-secret; cf_clearance=clear-secret"
    );

    let delete_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/admin/accounts/acct_cookies/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_response.status(), StatusCode::OK);

    let get_empty = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/acct_cookies/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_empty.status(), StatusCode::OK);
    let body = response_json(get_empty).await;
    assert!(body["data"]["cookies"].is_null());
}

#[tokio::test]
async fn admin_account_cookies_should_require_existing_account_and_non_empty_cookies() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-cookies-invalid.sqlite", 23).await;

    let missing = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/missing/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    import_test_account(&app, "session_1", "acct_cookie_invalid").await;
    let invalid = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/acct_cookie_invalid/cookies")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"cookies":""}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_accounts_export_should_return_native_accounts_with_tokens_and_filter_ids() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-export-native.sqlite", 24).await;
    import_test_account(&app, "session_1", "acct_export_a").await;
    import_test_account(&app, "session_1", "acct_export_b").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/export?ids=acct_export_a")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["sourceFormat"], "native");
    assert_eq!(body["data"]["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["accounts"][0]["id"], "acct_export_a");
    assert_eq!(body["data"]["accounts"][0]["token"], "access-acct_export_a");
    assert_eq!(
        body["data"]["accounts"][0]["refreshToken"],
        "refresh-acct_export_a"
    );
    assert!(body["data"]["accounts"][0].get("proxyApiKey").is_none());

    let invalid = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/export?format=proxy")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_accounts_export_should_return_sub2api_openai_oauth_payload_without_proxy_fields() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-export-sub2api.sqlite", 25).await;
    import_test_account(&app, "session_1", "acct_export_sub2api").await;

    let label_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/admin/accounts/acct_export_sub2api/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":"Sub2API Export"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(label_response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/export?format=sub2api")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["type"], "sub2api-data");
    assert_eq!(body["data"]["version"], 1);
    assert_eq!(body["data"]["proxies"], json!([]));
    assert_eq!(body["data"]["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["accounts"][0]["name"], "Sub2API Export");
    assert_eq!(body["data"]["accounts"][0]["platform"], "openai");
    assert_eq!(body["data"]["accounts"][0]["type"], "oauth");
    assert_eq!(
        body["data"]["accounts"][0]["credentials"]["access_token"],
        "access-acct_export_sub2api"
    );
    assert_eq!(
        body["data"]["accounts"][0]["credentials"]["refresh_token"],
        "refresh-acct_export_sub2api"
    );
    assert_eq!(
        body["data"]["accounts"][0]["credentials"]["email"],
        "acct_export_sub2api@example.com"
    );
    assert_eq!(
        body["data"]["accounts"][0]["credentials"]["plan_type"],
        "plus"
    );
    assert!(body["data"]["accounts"][0].get("proxy").is_none());
    assert!(body["data"]["accounts"][0].get("proxyUrl").is_none());
}

#[tokio::test]
async fn manual_add_should_derive_claims_ignore_metadata_and_sync_pool() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-account-manual-add.sqlite", 26).await;
    let token = test_jwt(
        Some("jwt-account"),
        Some("jwt-user"),
        Some("jwt@example.com"),
        Some("team"),
        3600,
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "id": "caller-id",
                        "email": "caller@example.com",
                        "accountId": "caller-account",
                        "userId": "caller-user",
                        "label": "Caller Label",
                        "planType": "caller-plan",
                        "token": token,
                        "refreshToken": "manual-refresh-secret",
                        "status": "disabled"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let response_id = body["data"]["id"].as_str().unwrap();
    assert_ne!(response_id, "caller-id");
    assert_eq!(body["data"]["email"], "jwt@example.com");
    assert_eq!(body["data"]["accountId"], "jwt-account");
    assert_eq!(body["data"]["userId"], "jwt-user");
    assert_eq!(body["data"]["planType"], "team");
    assert!(body["data"]["label"].is_null());
    assert_eq!(body["data"]["status"], "active");
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());

    let stored = sqlx::query(
        "select id, email, account_id, user_id, label, plan_type, status, access_token_cipher, refresh_token_cipher from accounts",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored.get::<String, _>("id"), response_id);
    assert_eq!(
        stored.get::<Option<String>, _>("email").as_deref(),
        Some("jwt@example.com")
    );
    assert_eq!(
        stored.get::<Option<String>, _>("account_id").as_deref(),
        Some("jwt-account")
    );
    assert_eq!(
        stored.get::<Option<String>, _>("user_id").as_deref(),
        Some("jwt-user")
    );
    assert_eq!(stored.get::<Option<String>, _>("label"), None);
    assert_eq!(
        stored.get::<Option<String>, _>("plan_type").as_deref(),
        Some("team")
    );
    assert_eq!(stored.get::<String, _>("status"), "active");
    let access_token_cipher = stored.get::<String, _>("access_token_cipher");
    assert!(access_token_cipher.starts_with("v1:"));
    assert!(!access_token_cipher.contains(&token));
    let refresh_token_cipher = stored.get::<String, _>("refresh_token_cipher");
    assert!(refresh_token_cipher.starts_with("v1:"));
    assert!(!refresh_token_cipher.contains("manual-refresh-secret"));

    let acquired = state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .unwrap();
    assert_eq!(acquired.id, response_id);
    assert_eq!(acquired.email.as_deref(), Some("jwt@example.com"));
    assert_eq!(acquired.account_id.as_deref(), Some("jwt-account"));
    assert_eq!(acquired.user_id.as_deref(), Some("jwt-user"));
    assert_eq!(acquired.plan_type.as_deref(), Some("team"));
}

#[tokio::test]
async fn admin_account_manual_add_should_reject_missing_invalid_expired_or_unbound_tokens() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-manual-add-invalid.sqlite", 27).await;

    let missing_tokens = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_tokens.status(), StatusCode::BAD_REQUEST);

    let invalid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "token": "not-a-jwt"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let expired = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "token": test_jwt(
                            Some("expired-account"),
                            Some("expired-user"),
                            Some("expired@example.com"),
                            Some("plus"),
                            -3600,
                        )
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(expired.status(), StatusCode::BAD_REQUEST);

    let missing_account_claim = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "token": test_jwt(
                            None,
                            Some("claimless-user"),
                            Some("claimless@example.com"),
                            Some("free"),
                            3600,
                        )
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_account_claim.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_account_manual_add_should_exchange_refresh_token_only_and_rotate_refresh_token() {
    let token = test_jwt(
        Some("rt-account"),
        Some("rt-user"),
        Some("rt@example.com"),
        Some("plus"),
        3600,
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let refresher = StaticTokenRefresher {
        result: Ok(TokenPair {
            access_token: token.clone(),
            refresh_token: Some("rotated-refresh".to_string()),
        }),
        calls: calls.clone(),
    };
    let (app, state, pool, _dir) = admin_accounts_test_app_with_refresher(
        "admin-account-manual-refresh-only.sqlite",
        28,
        refresher,
    )
    .await;

    let response = post_admin_account(
        &app,
        json!({
            "refreshToken": "initial-refresh",
            "email": "caller@example.com",
            "planType": "caller-plan"
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["email"], "rt@example.com");
    assert_eq!(body["data"]["accountId"], "rt-account");
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());
    assert_eq!(*calls.lock().await, vec!["initial-refresh".to_string()]);

    let repo = AccountRepository::new(pool, SecretBox::new([28u8; 32]));
    let stored = repo
        .get(body["data"]["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose_secret(), &token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "rotated-refresh"
    );
    let acquired = state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .unwrap();
    assert_eq!(acquired.refresh_token.as_deref(), Some("rotated-refresh"));
}

#[tokio::test]
async fn admin_account_manual_add_should_preserve_input_refresh_token_when_exchange_omits_rotation()
{
    let token = test_jwt(
        Some("rt-preserve-account"),
        Some("rt-preserve-user"),
        Some("preserve@example.com"),
        Some("free"),
        3600,
    );
    let refresher = StaticTokenRefresher {
        result: Ok(TokenPair {
            access_token: token,
            refresh_token: None,
        }),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_refresher(
        "admin-account-manual-refresh-preserve.sqlite",
        29,
        refresher,
    )
    .await;

    let response = post_admin_account(&app, json!({ "refreshToken": "preserved-refresh" })).await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let repo = AccountRepository::new(pool, SecretBox::new([29u8; 32]));
    let stored = repo
        .get(body["data"]["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "preserved-refresh"
    );
}

#[tokio::test]
async fn admin_account_manual_add_should_update_existing_account_for_same_account_and_user() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-account-manual-update-existing.sqlite", 30).await;
    let first_token = test_jwt(
        Some("team-account"),
        Some("same-user"),
        Some("first@example.com"),
        Some("free"),
        3600,
    );
    let first_response = post_admin_account(
        &app,
        json!({
            "token": first_token,
            "refreshToken": "first-refresh"
        }),
    )
    .await;
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = response_json(first_response).await;
    let first_id = first_body["data"]["id"].as_str().unwrap().to_string();

    let second_token = test_jwt(
        Some("team-account"),
        Some("same-user"),
        Some("second@example.com"),
        Some("team"),
        7200,
    );
    let second_response = post_admin_account(&app, json!({ "token": second_token })).await;

    assert_eq!(second_response.status(), StatusCode::OK);
    let second_body = response_json(second_response).await;
    assert_eq!(second_body["data"]["id"], first_id);
    assert_eq!(second_body["data"]["email"], "second@example.com");
    assert_eq!(second_body["data"]["planType"], "team");

    let count: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
    let repo = AccountRepository::new(pool, SecretBox::new([30u8; 32]));
    let stored = repo.get(&first_id).await.unwrap().unwrap();
    assert_eq!(stored.access_token.expose_secret(), &second_token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "first-refresh"
    );
    assert_eq!(stored.email.as_deref(), Some("second@example.com"));
    assert_eq!(stored.plan_type.as_deref(), Some("team"));

    let acquired = state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .unwrap();
    assert_eq!(acquired.id, first_id);
    assert_eq!(acquired.access_token, second_token);
    assert_eq!(acquired.refresh_token.as_deref(), Some("first-refresh"));
}

#[tokio::test]
async fn manual_add_should_preserve_existing_refresh_when_refresh_only_omits_rotation() {
    let refreshed_token = test_jwt(
        Some("rt-existing-account"),
        Some("rt-existing-user"),
        Some("second@example.com"),
        Some("team"),
        7200,
    );
    let refresher = StaticTokenRefresher {
        result: Ok(TokenPair {
            access_token: refreshed_token.clone(),
            refresh_token: None,
        }),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_refresher(
        "admin-account-manual-refresh-existing-preserve.sqlite",
        31,
        refresher,
    )
    .await;
    let first_token = test_jwt(
        Some("rt-existing-account"),
        Some("rt-existing-user"),
        Some("first@example.com"),
        Some("free"),
        3600,
    );
    let first_response = post_admin_account(
        &app,
        json!({
            "token": first_token,
            "refreshToken": "old-refresh"
        }),
    )
    .await;
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_body = response_json(first_response).await;
    let account_id = first_body["data"]["id"].as_str().unwrap().to_string();

    let second_response =
        post_admin_account(&app, json!({ "refreshToken": "incoming-refresh" })).await;

    assert_eq!(second_response.status(), StatusCode::OK);
    let repo = AccountRepository::new(pool, SecretBox::new([31u8; 32]));
    let stored = repo.get(&account_id).await.unwrap().unwrap();
    assert_eq!(stored.access_token.expose_secret(), &refreshed_token);
    assert_eq!(stored.refresh_token.unwrap().expose_secret(), "old-refresh");
    assert_eq!(stored.email.as_deref(), Some("second@example.com"));
    assert_eq!(stored.plan_type.as_deref(), Some("team"));
}

#[tokio::test]
async fn admin_accounts_import_cli_should_read_codex_auth_file_store_encrypted_and_sync_pool() {
    let (app, state, pool, dir) =
        admin_accounts_test_app("admin-account-import-cli.sqlite", 28).await;
    let codex_home = dir.path().join("codex-home");
    fs::create_dir_all(&codex_home).unwrap();
    let token = test_jwt(
        Some("cli-account"),
        Some("cli-user"),
        Some("cli@example.com"),
        Some("plus"),
        3600,
    );
    fs::write(
        codex_home.join("auth.json"),
        json!({
            "access_token": token,
            "refresh_token": "cli-refresh-secret"
        })
        .to_string(),
    )
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import-cli")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_import_cli")
                .body(Body::from(
                    json!({
                        "codexHome": codex_home.display().to_string(),
                        "id": "caller-id",
                        "email": "caller@example.com",
                        "accountId": "caller-account",
                        "userId": "caller-user",
                        "label": "caller-label",
                        "planType": "caller-plan"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["sourceFormat"], "codex_cli");
    assert_eq!(body["data"]["imported"], 1);
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());

    let stored: (
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        String,
        String,
    ) = sqlx::query_as(
        "select id, email, account_id, user_id, label, plan_type, access_token_cipher, refresh_token_cipher from accounts limit 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_ne!(stored.0, "caller-id");
    assert_eq!(stored.1, "cli@example.com");
    assert_eq!(stored.2, "cli-account");
    assert_eq!(stored.3, "cli-user");
    assert_eq!(stored.4, None);
    assert_eq!(stored.5, "plus");
    assert!(stored.6.starts_with("v1:"));
    assert!(!stored.6.contains(&token));
    assert!(stored.7.starts_with("v1:"));
    assert!(!stored.7.contains("cli-refresh-secret"));

    let acquired = state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .unwrap();
    assert_eq!(acquired.id, stored.0);
}

#[tokio::test]
async fn admin_account_refresh_should_update_tokens_and_runtime_pool_without_returning_secrets() {
    let (app, state, pool, _dir) = admin_accounts_test_app_with_refresher(
        "admin-account-refresh.sqlite",
        28,
        StaticTokenRefresher {
            result: Ok(TokenPair {
                access_token: "new-admin-refresh-access".to_string(),
                refresh_token: Some("new-admin-refresh-rt".to_string()),
            }),
            calls: Arc::new(Mutex::new(Vec::new())),
        },
    )
    .await;
    import_test_account(&app, "session_1", "acct_refresh").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/acct_refresh/refresh")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_refresh_account")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["id"], "acct_refresh");
    assert_eq!(body["data"]["result"], "alive");
    assert_eq!(body["data"]["previousStatus"], "active");
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("new-admin-refresh-access"));
    assert!(!serialized.contains("new-admin-refresh-rt"));

    let stored: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where id = ?",
    )
    .bind("acct_refresh")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(stored.0.starts_with("v1:"));
    assert!(!stored.0.contains("new-admin-refresh-access"));
    assert!(stored.1.starts_with("v1:"));
    assert!(!stored.1.contains("new-admin-refresh-rt"));
    let acquired = state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .unwrap();
    assert_eq!(acquired.access_token, "new-admin-refresh-access");
}

#[tokio::test]
async fn admin_account_refresh_should_mark_invalid_refresh_token_as_expired() {
    let (app, state, pool, _dir) = admin_accounts_test_app_with_refresher(
        "admin-account-refresh-invalid.sqlite",
        29,
        FailingTokenRefresher {
            failure: RefreshFailure::InvalidGrant,
        },
    )
    .await;
    import_test_account(&app, "session_1", "acct_refresh_invalid").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/acct_refresh_invalid/refresh")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_refresh_invalid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["result"], "dead");
    assert_eq!(body["data"]["status"], "expired");
    let status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_refresh_invalid")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status.0, "expired");
    assert!(state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .is_none());
}

#[tokio::test]
async fn admin_account_reset_usage_should_clear_local_counters_and_pool_last_used() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-account-reset-usage.sqlite", 30).await;
    import_test_account(&app, "session_1", "acct_reset_usage").await;
    sqlx::query(
        "insert into account_usage (account_id, request_count, input_tokens, output_tokens, cached_tokens, last_used_at) values (?, 7, 11, 13, 17, ?)",
    )
    .bind("acct_reset_usage")
    .bind("2026-06-12T12:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    state.reload_account_pool_from_repository().await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/acct_reset_usage/reset-usage")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_reset_usage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["id"], "acct_reset_usage");
    assert_eq!(body["data"]["reset"], true);
    let usage: (i64, i64, i64, i64, Option<String>) = sqlx::query_as(
        "select request_count, input_tokens, output_tokens, cached_tokens, last_used_at from account_usage where account_id = ?",
    )
    .bind("acct_reset_usage")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(usage, (0, 0, 0, 0, None));
    let acquired = state
        .account_pool()
        .lock()
        .await
        .acquire("gpt-5.5")
        .unwrap();
    assert_eq!(acquired.last_used_at, None);
}

#[tokio::test]
async fn admin_account_quota_should_fetch_usage_store_quota_and_not_return_secrets() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-acct_quota"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 25,
                    "limit_window_seconds": 300,
                    "reset_after_seconds": 120,
                    "reset_at": 1770000000
                },
                "secondary_window": null
            },
            "code_review_rate_limit": null,
            "additional_rate_limits": null,
            "credits": {
                "has_credits": false,
                "unlimited": false,
                "overage_limit_reached": false,
                "balance": "0"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) =
        admin_accounts_test_app_with_base_url("admin-account-quota.sqlite", 31, server.uri()).await;
    import_test_account(&app, "session_1", "acct_quota").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/accounts/acct_quota/quota")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_quota")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["quota"]["plan_type"], "plus");
    assert_eq!(body["data"]["quota"]["rate_limit"]["remaining_percent"], 75);
    assert_eq!(
        body["data"]["raw"]["rate_limit"]["primary_window"]["used_percent"],
        25
    );
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("access-acct_quota"));
    assert!(!serialized.contains("refresh-acct_quota"));
    let stored: (String,) = sqlx::query_as("select quota_json from accounts where id = ?")
        .bind("acct_quota")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stored.0.contains("\"remaining_percent\":75"));
    assert!(!stored.0.contains("access-acct_quota"));
}

#[tokio::test]
async fn admin_accounts_health_check_should_probe_backend_and_mark_invalid_accounts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-acct_health_alive"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": null,
                "secondary_window": null
            },
            "code_review_rate_limit": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-acct_health_dead"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": "invalid token"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) =
        admin_accounts_test_app_with_base_url("admin-account-health.sqlite", 32, server.uri())
            .await;
    import_test_account(&app, "session_1", "acct_health_alive").await;
    import_test_account(&app, "session_1", "acct_health_dead").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/health-check")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_health")
                .body(Body::from(
                    json!({
                        "ids": ["acct_health_alive", "acct_health_dead"],
                        "concurrency": 2
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["summary"]["total"], 2);
    assert_eq!(body["data"]["summary"]["alive"], 1);
    assert_eq!(body["data"]["summary"]["dead"], 1);
    assert_eq!(body["data"]["summary"]["skipped"], 0);
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("access-acct_health_alive"));
    assert!(!serialized.contains("access-acct_health_dead"));
    let status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_health_dead")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status.0, "expired");
}

async fn admin_accounts_test_app(
    db_name: &str,
    key_byte: u8,
) -> (Router, AppState, sqlx::SqlitePool, tempfile::TempDir) {
    admin_accounts_test_app_with_base_url(
        db_name,
        key_byte,
        "https://chatgpt.com/backend-api".to_string(),
    )
    .await
}

async fn admin_accounts_test_app_with_base_url(
    db_name: &str,
    key_byte: u8,
    base_url: String,
) -> (Router, AppState, sqlx::SqlitePool, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_and_secret_box(
        test_config_with_base_url(url, base_url),
        pool.clone(),
        SecretBox::new([key_byte; 32]),
    );
    let app = build_router(state.clone());
    (app, state, pool, dir)
}

async fn admin_accounts_test_app_with_refresher<C>(
    db_name: &str,
    key_byte: u8,
    token_refresher: C,
) -> (Router, AppState, sqlx::SqlitePool, tempfile::TempDir)
where
    C: TokenRefresher,
{
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        test_config(url),
        pool.clone(),
        SecretBox::new([key_byte; 32]),
        ApiKeyHasher::new([key_byte; 32]),
        token_refresher,
    );
    let app = build_router(state.clone());
    (app, state, pool, dir)
}

async fn admin_accounts_test_app_with_oauth_client<C>(
    db_name: &str,
    key_byte: u8,
    oauth_client: C,
) -> (Router, AppState, sqlx::SqlitePool, tempfile::TempDir)
where
    C: OAuthClient + TokenRefresher,
{
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_secret_api_key_hasher_and_oauth_client(
        test_config(url),
        pool.clone(),
        SecretBox::new([key_byte; 32]),
        ApiKeyHasher::new([key_byte; 32]),
        oauth_client,
    );
    let app = build_router(state.clone());
    (app, state, pool, dir)
}

async fn post_admin_account(app: &Router, payload: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn import_test_account(app: &Router, session_id: &str, account_id: &str) {
    let import_body = json!({
        "accounts": [{
            "id": account_id,
            "email": format!("{account_id}@example.com"),
            "planType": "plus",
            "token": format!("access-{account_id}"),
            "refreshToken": format!("refresh-{account_id}"),
            "status": "active"
        }]
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", format!("cpr_admin_session={session_id}"))
                .body(Body::from(import_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn seed_admin_session(pool: &sqlx::SqlitePool, session_id: &str) {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "insert into admin_users (id, password_hash, created_at, updated_at) values (?, ?, ?, ?)",
    )
    .bind("admin_1")
    .bind("hash")
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into admin_sessions (id, user_id, expires_at, created_at) values (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind("admin_1")
    .bind("2999-01-01T00:00:00Z")
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn test_jwt(
    account_id: Option<&str>,
    user_id: Option<&str>,
    email: Option<&str>,
    plan_type: Option<&str>,
    exp_offset_seconds: i64,
) -> String {
    let mut auth = serde_json::Map::new();
    if let Some(account_id) = account_id {
        auth.insert(
            "chatgpt_account_id".to_string(),
            Value::String(account_id.to_string()),
        );
    }
    if let Some(user_id) = user_id {
        auth.insert(
            "chatgpt_user_id".to_string(),
            Value::String(user_id.to_string()),
        );
    }
    if let Some(plan_type) = plan_type {
        auth.insert(
            "chatgpt_plan_type".to_string(),
            Value::String(plan_type.to_string()),
        );
    }
    let mut profile = serde_json::Map::new();
    if let Some(email) = email {
        profile.insert("email".to_string(), Value::String(email.to_string()));
    }
    let payload = json!({
        "exp": Utc::now().timestamp() + exp_offset_seconds,
        "https://api.openai.com/auth": Value::Object(auth),
        "https://api.openai.com/profile": Value::Object(profile),
    });
    let header = json!({ "alg": "none", "typ": "JWT" });
    format!("{}.{}.", jwt_part(&header), jwt_part(&payload),)
}

fn jwt_part(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
}
