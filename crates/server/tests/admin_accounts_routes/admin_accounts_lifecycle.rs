use super::*;

#[tokio::test]
async fn admin_accounts_lifecycle_should_update_and_delete_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts-lifecycle.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    sqlx::query(
        "insert into accounts (id, email, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_lifecycle")
    .bind("life@example.com")
    .bind("cipher")
    .bind("active")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([75u8; 32]),
        ApiKeyHasher::new([76u8; 32]),
    );
    let app = router::router().with_state(state);

    let label = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/accounts/acct_lifecycle/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({"label": "primary"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let label_status = label.status();
    let label_body = response_json(label).await;

    assert_eq!(label_status, StatusCode::OK);
    assert_eq!(label_body["data"]["label"], "primary");

    let status = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/accounts/acct_lifecycle/status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({"status": "disabled"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status_code = status.status();
    let status_body = response_json(status).await;

    assert_eq!(status_code, StatusCode::OK);
    assert_eq!(status_body["data"]["status"], "disabled");

    let deleted = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/accounts/acct_lifecycle")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let delete_status = deleted.status();
    let delete_body = response_json(deleted).await;

    assert_eq!(delete_status, StatusCode::OK);
    assert_eq!(delete_body["data"]["deleted"], true);
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
        },
    )
    .await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box,
        ApiKeyHasher::new([86u8; 32]),
    );
    let app = router::router().with_state(state);

    let set = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/acct_cookies/cookies")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"cookies":"cf_clearance=clear-secret; __cf_bm=bm-secret"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let set_status = set.status();
    let set_body = response_json(set).await;

    assert_eq!(set_status, StatusCode::OK);
    assert_eq!(
        set_body["data"]["cookies"],
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

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/acct_cookies/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let get_status = get.status();
    let get_body = response_json(get).await;

    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(
        get_body["data"]["cookies"],
        "__cf_bm=bm-secret; cf_clearance=clear-secret"
    );

    let delete = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/accounts/acct_cookies/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let delete_status = delete.status();
    let delete_body = response_json(delete).await;

    assert_eq!(delete_status, StatusCode::OK);
    assert_eq!(delete_body["data"]["deleted"], true);
}

#[tokio::test]
async fn admin_account_cookies_should_reject_missing_account_and_empty_payload() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-cookies-invalid.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([87u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_cookie_invalid".to_string(),
            email: Some("cookie-invalid@example.com".to_string()),
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-cookie-invalid".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        secret_box,
        ApiKeyHasher::new([88u8; 32]),
    );
    let app = router::router().with_state(state);

    let missing = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/missing/cookies")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let empty = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/acct_cookie_invalid/cookies")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({"cookies": ""}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_account_health_check_should_skip_account_without_refresh_token() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 1
                }
            }
        })))
        .expect(0)
        .mount(&upstream)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-health-no-refresh.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([93u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_health".to_string(),
            email: Some("health@example.com".to_string()),
            account_id: Some("chatgpt-health".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-health".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let mut config = test_config(url);
    config.api.base_url = upstream.uri();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        config,
        pool,
        secret_box,
        ApiKeyHasher::new([94u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/health-check")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": ["acct_health"],
                        "concurrency": 1,
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
    assert_eq!(body["data"]["summary"]["total"], 1);
    assert_eq!(body["data"]["summary"]["alive"], 0);
    assert_eq!(body["data"]["summary"]["dead"], 0);
    assert_eq!(body["data"]["summary"]["skipped"], 1);
    assert_eq!(body["data"]["results"][0]["id"], "acct_health");
    assert_eq!(body["data"]["results"][0]["result"], "skipped");
    assert_eq!(body["data"]["results"][0]["status"], "active");
    assert_eq!(body["data"]["results"][0]["error"], "no refresh token");
}

#[tokio::test]
async fn admin_accounts_health_check_should_reject_unsupported_stagger_ms_field() {
    let (app, _state, _pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-account-health-unsupported-field.sqlite", 125).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/health-check")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"stagger_ms":1000}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response_json(response).await;
    assert_eq!(body["code"], 40001);
}

#[tokio::test]
async fn admin_accounts_batch_status_should_update_found_accounts_and_report_invalid_requests() {
    let (app, state, pool, _dir, secret_box) =
        admin_accounts_test_app("admin-accounts-batch-status-route.sqlite", 120).await;
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_batch_status_a".to_string(),
            email: Some("batch-a@example.com".to_string()),
            account_id: Some("batch-a".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-batch-a".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    seed_encrypted_account(
        &pool,
        secret_box,
        NewAccount {
            id: "acct_batch_status_b".to_string(),
            email: Some("batch-b@example.com".to_string()),
            account_id: Some("batch-b".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-batch-b".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let restored = state.restore_account_pool_from_repository().await.unwrap();
    assert_eq!(restored, 2);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/batch-status")
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
    let invalid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/batch-status")
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
                .uri("/api/admin/accounts/batch-status")
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
async fn admin_account_refresh_should_update_tokens_and_runtime_pool_without_returning_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-refresh-route.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([121u8; 32]);
    let refreshed_access_token = test_jwt(
        "refresh-account",
        Some("refresh-user"),
        Some("refresh@example.com"),
        Some("plus"),
    );
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_refresh_route".to_string(),
            email: Some("old-refresh@example.com".to_string()),
            account_id: Some("old-refresh-account".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("old-access-token".to_string().into()),
            refresh_token: Some(SecretString::new("old-refresh-token".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([122u8; 32]),
        StaticTokenRefresher {
            result: Ok(TokenPair {
                access_token: refreshed_access_token.clone(),
                refresh_token: Some("new-admin-refresh-rt".to_string()),
            }),
            calls: Arc::new(Mutex::new(Vec::new())),
        },
    );
    let app = router::router().with_state(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/acct_refresh_route/refresh")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_refresh_account")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["id"], "acct_refresh_route");
    assert_eq!(body["data"]["result"], "alive");
    assert_eq!(body["data"]["previousStatus"], "active");
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains(&refreshed_access_token));
    assert!(!serialized.contains("new-admin-refresh-rt"));

    let stored = SqliteAccountStore::new(pool.clone(), secret_box)
        .get("acct_refresh_route")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose_secret(), refreshed_access_token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "new-admin-refresh-rt"
    );
    let raw: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where id = ?",
    )
    .bind("acct_refresh_route")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(raw.0.starts_with("v1:"));
    assert!(!raw.0.contains("old-access-token"));
    assert!(!raw.0.contains(&refreshed_access_token));
    assert!(raw.1.starts_with("v1:"));
    assert!(!raw.1.contains("new-admin-refresh-rt"));
    let acquired = state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .unwrap()
        .account;
    assert_eq!(acquired.access_token, refreshed_access_token);
}

#[tokio::test]
async fn admin_account_refresh_should_disable_invalid_refresh_token() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir
        .path()
        .join("admin-account-refresh-invalid-route.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([123u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box,
        NewAccount {
            id: "acct_refresh_invalid_route".to_string(),
            email: Some("refresh-invalid@example.com".to_string()),
            account_id: Some("refresh-invalid-account".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("invalid-old-access".to_string().into()),
            refresh_token: Some(SecretString::new(
                "invalid-refresh-token".to_string().into(),
            )),
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        test_config(url),
        pool.clone(),
        SecretBox::new([123u8; 32]),
        ApiKeyHasher::new([124u8; 32]),
        StaticTokenRefresher {
            result: Err(RefreshFailure::InvalidGrant),
            calls: Arc::new(Mutex::new(Vec::new())),
        },
    );
    let restored = state.restore_account_pool_from_repository().await.unwrap();
    assert_eq!(restored, 1);
    let app = router::router().with_state(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/acct_refresh_invalid_route/refresh")
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
    assert_eq!(body["data"]["status"], "disabled");
    let stored_status: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_refresh_invalid_route")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored_status.0, "disabled");
    assert!(state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .is_none());
}

#[tokio::test]
async fn admin_account_reset_usage_should_clear_local_counters_and_pool_last_used() {
    let (app, state, pool, _dir, secret_box) =
        admin_accounts_test_app("admin-account-reset-usage-route.sqlite", 125).await;
    let window_reset_at = Utc::now() + Duration::minutes(5);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_reset_usage_route".to_string(),
            email: Some("reset-usage@example.com".to_string()),
            account_id: Some("reset-usage-account".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-reset-usage".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    sqlx::query(
        "insert into account_usage (account_id, request_count, input_tokens, output_tokens, cached_tokens, window_request_count, window_input_tokens, window_output_tokens, window_cached_tokens, window_started_at, window_reset_at, limit_window_seconds, last_used_at) values (?, 7, 11, 13, 17, 5, 19, 23, 29, ?, ?, 300, ?)",
    )
    .bind("acct_reset_usage_route")
    .bind("2026-06-12T12:30:00Z")
    .bind(window_reset_at.to_rfc3339())
    .bind("2026-06-12T12:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    state.restore_account_pool_from_repository().await.unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/acct_reset_usage_route/reset-usage")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_reset_usage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["id"], "acct_reset_usage_route");
    assert_eq!(body["data"]["reset"], true);
    type ResetUsageRow = (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<String>,
        Option<String>,
    );
    let usage: ResetUsageRow = sqlx::query_as(
        "select request_count, input_tokens, output_tokens, cached_tokens, window_request_count, window_input_tokens, window_output_tokens, window_cached_tokens, window_reset_at, last_used_at from account_usage where account_id = ?",
    )
    .bind("acct_reset_usage_route")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        usage,
        (
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            Some(window_reset_at.to_rfc3339()),
            None
        )
    );
    let pool_account = SqliteAccountStore::new(pool, secret_box)
        .get_pool_account("acct_reset_usage_route")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pool_account.request_count, 0);
    assert_eq!(pool_account.window_request_count, 0);
    assert_eq!(pool_account.window_reset_at, Some(window_reset_at));
    assert!(pool_account.last_used_at.is_none());
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
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([92u8; 32]),
    );
    let app = router::router().with_state(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts")
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
                        "token": format!("Bearer {token}"),
                        "refreshToken": "manual-refresh-secret",
                        "status": "disabled"
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
    let created_id = body["data"]["id"].as_str().unwrap();
    assert_ne!(created_id, "caller-id");
    assert_eq!(body["data"]["email"], "jwt@example.com");
    assert_eq!(body["data"]["accountId"], "jwt-account");
    assert_eq!(body["data"]["userId"], "jwt-user");
    assert_eq!(body["data"]["planType"], "team");
    assert!(body["data"]["label"].is_null());
    assert_eq!(body["data"]["status"], "active");
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());

    let stored = SqliteAccountStore::new(pool.clone(), secret_box)
        .get(created_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose_secret(), &token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "manual-refresh-secret"
    );

    let raw_cipher: (String,) =
        sqlx::query_as("select access_token_cipher from accounts where id = ?")
            .bind(created_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!raw_cipher.0.contains(&token));
    let raw_refresh_cipher: (String,) =
        sqlx::query_as("select refresh_token_cipher from accounts where id = ?")
            .bind(created_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(raw_cipher.0.starts_with("v1:"));
    assert!(raw_refresh_cipher.0.starts_with("v1:"));
    assert!(!raw_refresh_cipher.0.contains("manual-refresh-secret"));

    let acquired = state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .unwrap()
        .account;
    assert_eq!(acquired.id, created_id);
    assert_eq!(acquired.email.as_deref(), Some("jwt@example.com"));
    assert_eq!(acquired.account_id.as_deref(), Some("jwt-account"));
    assert_eq!(acquired.user_id.as_deref(), Some("jwt-user"));
    assert_eq!(acquired.plan_type.as_deref(), Some("team"));
}

#[tokio::test]
async fn admin_account_manual_create_should_reject_missing_invalid_expired_or_unbound_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-create-invalid.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([93u8; 32]),
        ApiKeyHasher::new([94u8; 32]),
    );
    let app = router::router().with_state(state);

    let cases = [
        ("missing tokens", json!({})),
        ("invalid jwt", json!({"token": "not-a-jwt"})),
        (
            "expired jwt",
            json!({"token": test_jwt_with_exp(
                Some("expired-account"),
                Some("expired-user"),
                Some("expired@example.com"),
                Some("plus"),
                1_600_000_000,
            )}),
        ),
        (
            "missing account claim",
            json!({"token": test_jwt_with_exp(
                None,
                Some("claimless-user"),
                Some("claimless@example.com"),
                Some("free"),
                4_102_444_800,
            )}),
        ),
    ];

    for (name, payload) in cases {
        let response = post_admin_account(&app, payload).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
    }
}

#[tokio::test]
async fn admin_account_manual_create_refresh_only_should_exchange_rotate_and_sync_pool() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-refresh-only.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([95u8; 32]);
    let token = test_jwt(
        "rt-account",
        Some("rt-user"),
        Some("rt@example.com"),
        Some("plus"),
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([96u8; 32]),
        StaticTokenRefresher {
            result: Ok(TokenPair {
                access_token: token.clone(),
                refresh_token: Some("rotated-refresh".to_string()),
            }),
            calls: calls.clone(),
        },
    );
    let app = router::router().with_state(state.clone());

    let response = post_admin_account(
        &app,
        json!({
            "refreshToken": "initial-refresh",
            "email": "caller@example.com",
            "planType": "caller-plan"
        }),
    )
    .await;
    let status = response.status();
    let body = response_json(response).await;
    let created_id = body["data"]["id"].as_str().unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["email"], "rt@example.com");
    assert_eq!(body["data"]["accountId"], "rt-account");
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());
    assert_eq!(*calls.lock().await, vec!["initial-refresh".to_string()]);

    let stored = SqliteAccountStore::new(pool, secret_box)
        .get(created_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose_secret(), &token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "rotated-refresh"
    );

    let acquired = state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .unwrap()
        .account;
    assert_eq!(acquired.id, created_id);
    assert_eq!(acquired.refresh_token.as_deref(), Some("rotated-refresh"));
}

#[tokio::test]
async fn admin_account_manual_create_refresh_only_should_preserve_input_refresh_without_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir
        .path()
        .join("admin-account-refresh-preserve-input.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([97u8; 32]);
    let token = test_jwt(
        "rt-preserve-account",
        Some("rt-preserve-user"),
        Some("preserve@example.com"),
        Some("free"),
    );
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([98u8; 32]),
        StaticTokenRefresher {
            result: Ok(TokenPair {
                access_token: token,
                refresh_token: None,
            }),
            calls: Arc::new(Mutex::new(Vec::new())),
        },
    );
    let app = router::router().with_state(state);

    let response = post_admin_account(&app, json!({"refreshToken": "preserved-refresh"})).await;
    let body = response_json(response).await;
    let created_id = body["data"]["id"].as_str().unwrap();

    let stored = SqliteAccountStore::new(pool, secret_box)
        .get(created_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "preserved-refresh"
    );
}

#[tokio::test]
async fn admin_account_manual_create_should_update_existing_and_preserve_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-update-existing.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([99u8; 32]);
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([100u8; 32]),
    );
    let app = router::router().with_state(state.clone());

    let first_token = test_jwt(
        "team-account",
        Some("same-user"),
        Some("first@example.com"),
        Some("free"),
    );
    let first_response = post_admin_account(
        &app,
        json!({
            "token": first_token,
            "refreshToken": "first-refresh"
        }),
    )
    .await;
    let first_body = response_json(first_response).await;
    let first_id = first_body["data"]["id"].as_str().unwrap().to_string();

    let second_token = test_jwt(
        "team-account",
        Some("same-user"),
        Some("second@example.com"),
        Some("team"),
    );
    let second_response = post_admin_account(&app, json!({"token": second_token})).await;
    let status = second_response.status();
    let second_body = response_json(second_response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(second_body["data"]["id"], first_id);
    assert_eq!(second_body["data"]["email"], "second@example.com");
    assert_eq!(second_body["data"]["planType"], "team");

    let count: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
    let stored = SqliteAccountStore::new(pool, secret_box)
        .get(&first_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose_secret(), &second_token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "first-refresh"
    );

    let acquired = state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .unwrap()
        .account;
    assert_eq!(acquired.id, first_id);
    assert_eq!(acquired.access_token, second_token);
    assert_eq!(acquired.refresh_token.as_deref(), Some("first-refresh"));
}

#[tokio::test]
async fn admin_account_manual_create_refresh_only_should_preserve_existing_refresh_without_rotation(
) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir
        .path()
        .join("admin-account-refresh-preserve-existing.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([101u8; 32]);
    let refreshed_token = test_jwt(
        "rt-existing-account",
        Some("rt-existing-user"),
        Some("second@example.com"),
        Some("team"),
    );
    let state = AppState::with_pool_secret_api_key_hasher_and_token_refresher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([102u8; 32]),
        StaticTokenRefresher {
            result: Ok(TokenPair {
                access_token: refreshed_token.clone(),
                refresh_token: None,
            }),
            calls: Arc::new(Mutex::new(Vec::new())),
        },
    );
    let app = router::router().with_state(state);
    let first_token = test_jwt(
        "rt-existing-account",
        Some("rt-existing-user"),
        Some("first@example.com"),
        Some("free"),
    );
    let first_response = post_admin_account(
        &app,
        json!({
            "token": first_token,
            "refreshToken": "old-refresh"
        }),
    )
    .await;
    let first_body = response_json(first_response).await;
    let account_id = first_body["data"]["id"].as_str().unwrap().to_string();

    let second_response =
        post_admin_account(&app, json!({"refreshToken": "incoming-refresh"})).await;
    assert_eq!(second_response.status(), StatusCode::OK);

    let stored = SqliteAccountStore::new(pool, secret_box)
        .get(&account_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token.expose_secret(), &refreshed_token);
    assert_eq!(stored.refresh_token.unwrap().expose_secret(), "old-refresh");
    assert_eq!(stored.email.as_deref(), Some("second@example.com"));
    assert_eq!(stored.plan_type.as_deref(), Some("team"));
}
