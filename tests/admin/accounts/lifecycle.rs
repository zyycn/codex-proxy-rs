use super::*;

#[tokio::test]
async fn admin_accounts_lifecycle_should_update_and_delete_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts-lifecycle.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    sqlx::query("insert into accounts (id, email, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)")
        .bind("acct_lifecycle").bind("life@example.com").bind("cipher").bind("active")
        .bind("2026-06-18T00:00:00Z").bind("2026-06-18T00:00:00Z").execute(&pool).await.unwrap();
    let config = test_config(url);
    let secret_box = SecretBox::new([75u8; 32]);
    let hasher = ApiKeyHasher::new([76u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), SecretBox::new([75u8; 32])),
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

    let label = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/update")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"id": "acct_lifecycle", "label": "primary"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(label.status(), StatusCode::OK);
    assert_eq!(response_json(label).await["data"]["label"], "primary");

    let status = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/update")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"id": "acct_lifecycle", "status": "disabled"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(response_json(status).await["data"]["status"], "disabled");

    let deleted = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/delete")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({"ids": ["acct_lifecycle"]}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(response_json(deleted).await["data"]["deleted"], 1);
}

#[tokio::test]
async fn admin_account_status_update_should_update_proxy_account_pool() {
    let (app, state, pool, _dir, secret_box) =
        admin_accounts_test_app("admin-account-status-runtime-pool.sqlite", 121).await;
    seed_encrypted_account(
        &pool,
        secret_box,
        NewAccount {
            id: "acct_runtime_status".to_string(),
            email: Some("runtime-status@example.com".to_string()),
            account_id: Some("chatgpt-runtime-status".to_string()),
            user_id: Some("user-runtime-status".to_string()),
            label: None,
            plan_type: None,
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt-runtime-status",
                    Some("user-runtime-status"),
                    Some("runtime-status@example.com"),
                    Some("free"),
                )
                .into(),
            ),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;
    state
        .services
        .account_pool
        .restore_from_repository()
        .await
        .unwrap();
    assert_eq!(
        state
            .services
            .account_pool
            .capacity_summary_now()
            .await
            .available_slots,
        3
    );

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/update")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"id": "acct_runtime_status", "status": "banned"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        state
            .services
            .account_pool
            .capacity_summary_now()
            .await
            .available_slots,
        0
    );
}

#[tokio::test]
async fn admin_account_refresh_should_not_mark_valid_account_banned_when_refresh_token_reused() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/oauth/token"))
        .respond_with(wiremock::ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "refresh_token_reused"
        })))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir, secret_box) = admin_accounts_test_app_with_oauth_token_endpoint(
        "admin-account-refresh-rt-reused.sqlite",
        124,
        format!("{}/oauth/token", server.uri()),
    )
    .await;
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_refresh_rt_reused".to_string(),
            email: Some("rt-reused@example.com".to_string()),
            account_id: Some("chatgpt-rt-reused".to_string()),
            user_id: Some("user-rt-reused".to_string()),
            label: None,
            plan_type: None,
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt-rt-reused",
                    Some("user-rt-reused"),
                    Some("rt-reused@example.com"),
                    Some("plus"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new("refresh-reused".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/refresh")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"id": "acct_refresh_rt_reused"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let stored = SqliteAccountStore::new(pool, secret_box)
        .get("acct_refresh_rt_reused")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(stored.status, AccountStatus::Active);
}

#[tokio::test]
async fn admin_account_update_should_accept_batch_status_payload() {
    let (app, _state, pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-account-batch-status-update.sqlite", 122).await;
    sqlx::query("insert into accounts (id, email, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)")
        .bind("acct_batch_status_1").bind("batch-1@example.com").bind("cipher").bind("active")
        .bind("2026-06-18T00:00:00Z").bind("2026-06-18T00:00:00Z").execute(&pool).await.unwrap();
    sqlx::query("insert into accounts (id, email, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)")
        .bind("acct_batch_status_2").bind("batch-2@example.com").bind("cipher").bind("active")
        .bind("2026-06-18T00:00:00Z").bind("2026-06-18T00:00:00Z").execute(&pool).await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/update")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": ["acct_batch_status_1", "acct_batch_status_2", "acct_missing"],
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
    assert_eq!(body["data"]["updated"], 2);
    assert_eq!(body["data"]["notFound"], json!(["acct_missing"]));
}

#[tokio::test]
async fn admin_account_batch_status_route_should_not_exist() {
    let (_app, state, _pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-account-batch-status-route.sqlite", 123).await;
    let admin_router = codex_proxy_rs::admin::router::router().with_state(state);

    let response = admin_router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/batch-status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"ids": ["acct_batch_status_1"], "status": "disabled"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_account_cookies_should_store_encrypted_cookie_header() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-cookies.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([85u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_cookies".to_string(),
            email: Some("cookies@example.com".to_string()),
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-cookies".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;
    let config = test_config(url);
    let hasher = ApiKeyHasher::new([86u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), SecretBox::new([85u8; 32])),
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

    let set = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/cookies")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "id": "acct_cookies",
                        "cookies":"cf_clearance=clear-secret; __cf_bm=bm-secret"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(set.status(), StatusCode::OK);

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/cookies?id=acct_cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    let get_body = response_json(get).await;
    let cookies = get_body["data"]["cookies"].as_str().unwrap();
    assert!(cookies.contains("cf_clearance=clear-secret"));
    assert!(cookies.contains("__cf_bm=bm-secret"));

    let stored_values: Vec<String> =
        sqlx::query_scalar("select value_cipher from account_cookies where account_id = ?1")
            .bind("acct_cookies")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(!stored_values.is_empty());
    assert!(!stored_values.iter().any(|value| value == "clear-secret"));
    assert!(!stored_values.iter().any(|value| value == "bm-secret"));
}

#[tokio::test]
async fn admin_account_create_should_derive_claims_and_store_encrypted_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-create.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([91u8; 32]);
    let token = test_jwt(
        "jwt-account",
        Some("jwt-user"),
        Some("jwt@example.com"),
        Some("team"),
    );
    let config = test_config(url);
    let hasher = ApiKeyHasher::new([92u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), secret_box.clone()),
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
    let app = codex_proxy_rs::http::router::router().with_state(state.clone());

    let response = app.oneshot(
        Request::builder().method("POST").uri("/api/admin/accounts")
            .header("content-type", "application/json")
            .header("cookie", "cpr_admin_session=session_1")
            .body(Body::from(json!({
                "id": "caller-id", "email": "caller@example.com",
                "accountId": "caller-account", "userId": "caller-user",
                "label": "Caller Label", "planType": "caller-plan",
                "token": format!("Bearer {}", token), "refreshToken": "manual-refresh-secret",
                "status": "disabled"
            }).to_string())).unwrap(),
    ).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert!(body["data"]["id"].as_str().is_some());
    assert!(body["data"].get("token").is_none());
}

#[tokio::test]
async fn admin_account_manual_create_should_reject_missing_invalid_expired_or_unbound_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-create-invalid.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let config = test_config(url);
    let secret_box = SecretBox::new([93u8; 32]);
    let hasher = ApiKeyHasher::new([94u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), SecretBox::new([93u8; 32])),
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

    let cases = [
        ("missing tokens", json!({})),
        ("invalid jwt", json!({"token": "not-a-jwt"})),
        (
            "expired jwt",
            json!({"token": test_jwt_with_exp(Some("expired-account"), Some("expired-user"), Some("expired@example.com"), Some("plus"), 1_600_000_000)}),
        ),
    ];
    for (name, payload) in cases {
        let response = post_admin_account(&app, payload).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
    }
}

#[tokio::test]
async fn admin_account_manual_create_should_accept_current_openai_token_without_chatgpt_account_id()
{
    let (app, _state, pool, _dir, secret_box) =
        admin_accounts_test_app("admin-account-create-current-openai-token.sqlite", 119).await;
    let token = test_jwt_with_exp(
        None,
        Some("current-user"),
        Some("current@example.com"),
        Some("free"),
        4_102_444_800,
    );

    let response = post_admin_account(
        &app,
        json!({"token": token.clone(), "refreshToken": "refresh-current"}),
    )
    .await;
    let status = response.status();
    let body = response_json(response).await;
    let stored = SqliteAccountStore::new(pool, secret_box)
        .get(body["data"]["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(stored.email.as_deref(), Some("current@example.com"));
    assert_eq!(stored.account_id, None);
    assert_eq!(stored.user_id.as_deref(), Some("current-user"));
    assert_eq!(stored.access_token.expose_secret(), token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "refresh-current"
    );
}
