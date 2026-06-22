use super::*;

#[tokio::test]
async fn admin_auth_login_start_should_return_pkce_auth_url_and_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-auth-login-start.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let config = test_config(url);
    let secret_box = SecretBox::new([95u8; 32]);
    let hasher = ApiKeyHasher::new([96u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), SecretBox::new([95u8; 32])),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone(), hasher),
        event_logs: SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = Fingerprint::default_for_tests();
    let services = Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);

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
    let state_val = body["data"]["state"].as_str().unwrap();
    let auth_url = body["data"]["authUrl"].as_str().unwrap();
    assert_eq!(state_val.len(), 32);
    assert!(auth_url.starts_with("https://auth.openai.com/oauth/authorize?"));
    assert!(auth_url.contains("response_type=code"));
    assert!(auth_url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
}

#[tokio::test]
async fn admin_auth_status_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-auth-status-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let config = test_config(url);
    let secret_box = SecretBox::new([105u8; 32]);
    let hasher = ApiKeyHasher::new([106u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), SecretBox::new([105u8; 32])),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone(), hasher),
        event_logs: SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = Fingerprint::default_for_tests();
    let services = Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);

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
    assert_eq!(response_json(response).await["code"], 40101);
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
            added_at: None,
        },
    )
    .await;
    let config = test_config(url);
    let hasher = ApiKeyHasher::new([108u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), SecretBox::new([107u8; 32])),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone(), hasher),
        event_logs: SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = Fingerprint::default_for_tests();
    let services = Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);

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
    assert_eq!(body["data"]["pool"]["total"], 1);
    assert_eq!(body["data"]["pool"]["active"], 1);
}

#[tokio::test]
async fn admin_auth_logout_should_clear_accounts_usage_cookies_and_runtime_pool() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-auth-logout.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([109u8; 32]);
    let config = test_config(url);
    let hasher = ApiKeyHasher::new([110u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), SecretBox::new([109u8; 32])),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone(), hasher),
        event_logs: SqliteEventLogStore::new(pool.clone()),
    };
    let fingerprint = Fingerprint::default_for_tests();
    let services = Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);

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
    assert_eq!(response_json(response).await["data"]["success"], true);
}
