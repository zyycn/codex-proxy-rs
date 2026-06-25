use super::*;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn admin_accounts_export_should_return_native_account_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts-export.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([77u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_export".to_string(),
            email: Some("export@example.com".to_string()),
            account_id: Some("chatgpt_export".to_string()),
            user_id: Some("user_export".to_string()),
            label: Some("primary".to_string()),
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new("access-export".to_string().into()),
            refresh_token: Some(SecretString::new("refresh-export".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;
    let config = test_config(url);
    let hasher = ApiKeyHasher::new([78u8; 32]);
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
    let fingerprint = crate::support::fingerprint::test_fingerprint();
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
                .uri("/api/admin/accounts/export?ids=acct_export")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_export")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["sourceFormat"], "native");
    assert_eq!(body["data"]["accounts"][0]["id"], "acct_export");
    assert_eq!(body["data"]["accounts"][0]["token"], "access-export");
    assert_eq!(
        body["data"]["accounts"][0]["refreshToken"],
        "refresh-export"
    );
}

#[tokio::test]
async fn admin_accounts_import_should_store_native_account_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts-import.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([79u8; 32]);
    let access_token = test_jwt(
        "chatgpt_import",
        Some("user_import"),
        Some("import@example.com"),
        Some("team"),
    );
    let config = test_config(url);
    let hasher = ApiKeyHasher::new([80u8; 32]);
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
    let fingerprint = crate::support::fingerprint::test_fingerprint();
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
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_import", "email": "import@example.com",
                            "accountId": "chatgpt_import", "userId": "user_import",
                            "label": "secondary", "planType": "team",
                            "token": access_token.clone(), "refreshToken": "refresh-import",
                            "accessTokenExpiresAt": "2026-06-18T02:00:00Z", "status": "disabled"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let stored = SqliteAccountStore::new(pool, secret_box)
        .get("acct_import")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["skipped"], 0);
    assert_eq!(body["data"]["sourceFormat"], "native");
    assert_eq!(stored.access_token.expose_secret(), access_token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "refresh-import"
    );
    assert_eq!(stored.status, AccountStatus::Disabled);
}

#[tokio::test]
async fn admin_accounts_import_should_fetch_wham_usage_for_current_openai_token_identity() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user_id": "user-fkAgiY6kv2Xs5hloB1j27yGo",
            "account_id": "wham-account-current",
            "email": "setup-down-penpal@duck.com",
            "plan_type": "free",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 22,
                    "limit_window_seconds": 2_592_000,
                    "reset_after_seconds": 2_562_691,
                    "reset_at": 1_784_268_840
                },
                "secondary_window": null
            },
            "credits": {
                "has_credits": false,
                "unlimited": false,
                "overage_limit_reached": false,
                "balance": null,
                "approx_local_messages": null,
                "approx_cloud_messages": null
            },
            "spend_control": {
                "reached": false,
                "individual_limit": null
            },
            "rate_limit_reset_credits": {
                "available_count": 0
            }
        })))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir, secret_box) = admin_accounts_test_app_with_api_base_url(
        "admin-accounts-import-current-openai-token.sqlite",
        118,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    let header = json!({"alg": "none", "typ": "JWT"});
    let payload = json!({
        "exp": 4_102_444_800i64,
        "https://api.openai.com/auth": {
            "user_id": "user-fkAgiY6kv2Xs5hloB1j27yGo",
            "poid": "org-CwdMgN6joZmuKKiL91oumkwE",
        },
        "https://api.openai.com/profile": {
            "email": "setup-down-penpal@duck.com",
            "email_verified": true,
        },
    });
    let access_token = format!("{}.{}.", jwt_part(&header), jwt_part(&payload));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_current_token")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "a18bcfa9ae932857",
                            "email": "setup-down-penpal@duck.com",
                            "accountId": null,
                            "userId": null,
                            "token": access_token.clone(),
                            "refreshToken": "refresh-current",
                            "status": "active"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let stored = SqliteAccountStore::new(pool, secret_box)
        .get("a18bcfa9ae932857")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(stored.email.as_deref(), Some("setup-down-penpal@duck.com"));
    assert_eq!(stored.account_id.as_deref(), Some("wham-account-current"));
    assert_eq!(
        stored.user_id.as_deref(),
        Some("user-fkAgiY6kv2Xs5hloB1j27yGo")
    );
    assert_eq!(stored.plan_type.as_deref(), Some("free"));
    assert_eq!(stored.access_token.expose_secret(), access_token);
    assert_eq!(stored.status, AccountStatus::Active);
}

#[tokio::test]
async fn admin_accounts_import_should_fetch_usage_to_complete_missing_plan_and_quota() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "account_id": "usage-account",
            "user_id": "supplement-user",
            "email": "supplement@example.com",
            "plan_type": "free",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 12.4,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 18_000
                }
            }
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts-import-supplement.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([120u8; 32]);
    let mut config = test_config(url);
    config.api.base_url = format!("{}/backend-api", server.uri());
    let hasher = ApiKeyHasher::new([121u8; 32]);
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
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = Arc::new(Services::new(&config, stores, fingerprint));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);
    let access_token = test_jwt_with_exp(
        None,
        Some("supplement-user"),
        Some("supplement@example.com"),
        None,
        4_102_444_800,
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_supplement")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_import_supplement",
                            "email": "supplement@example.com",
                            "accountId": null,
                            "userId": null,
                            "token": access_token.clone(),
                            "refreshToken": "refresh-supplement",
                            "status": "active"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let stored = SqliteAccountStore::new(pool.clone(), secret_box)
        .get("acct_import_supplement")
        .await
        .unwrap()
        .unwrap();
    let quota_json: Option<String> =
        sqlx::query_scalar("select quota_json from accounts where id = ?")
            .bind("acct_import_supplement")
            .fetch_one(&pool)
            .await
            .unwrap();
    let quota: Value = serde_json::from_str(&quota_json.unwrap()).unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(stored.plan_type.as_deref(), Some("free"));
    assert_eq!(stored.account_id.as_deref(), Some("usage-account"));
    assert_eq!(stored.user_id.as_deref(), Some("supplement-user"));
    assert_eq!(quota["plan_type"], "free");
    assert_eq!(quota["rate_limit"]["limit_reached"], false);

    let second_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_supplement_update")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_import_supplement_duplicate",
                            "email": "supplement@example.com",
                            "accountId": null,
                            "userId": null,
                            "token": access_token,
                            "refreshToken": "refresh-supplement-2",
                            "status": "active"
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let second_body = response_json(second_response).await;
    let account_count: i64 = sqlx::query_scalar("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(second_body["data"]["imported"], 1);
    assert_eq!(account_count, 1);
}

#[tokio::test]
async fn admin_accounts_import_should_update_existing_sub2api_account_and_clear_stale_quota_lock() {
    let (app, _state, pool, _dir, secret_box) =
        admin_accounts_test_app("admin-accounts-import-update-existing.sqlite", 116).await;
    let new_access_token = test_jwt(
        "chatgpt-import-update",
        Some("user-import-update"),
        Some("new@example.com"),
        Some("plus"),
    );
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_import_update".to_string(),
            email: Some("old@example.com".to_string()),
            account_id: Some("chatgpt-import-update".to_string()),
            user_id: Some("user-import-update".to_string()),
            label: Some("old-label".to_string()),
            plan_type: Some("free".to_string()),
            access_token: SecretString::new("old-access".to_string().into()),
            refresh_token: Some(SecretString::new("old-refresh".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Expired,
            added_at: None,
        },
    )
    .await;
    sqlx::query("update accounts set quota_limit_reached = 1, quota_cooldown_until = ?, quota_verify_required = 1 where id = ?")
        .bind("2026-06-25T00:00:00Z").bind("acct_import_update").execute(&pool).await.unwrap();

    let response = app.clone().oneshot(
        Request::builder().method("POST").uri("/api/admin/accounts/import")
            .header("content-type", "application/json")
            .header("cookie", "cpr_admin_session=session_1")
            .header("x-request-id", "req_accounts_import_update")
            .body(Body::from(json!({
                "accounts": [{
                    "id": "acct_import_update", "email": "new@example.com",
                    "accountId": "chatgpt-import-update", "userId": "user-import-update",
                    "label": "new-label", "planType": "plus",
                    "token": new_access_token, "refreshToken": "new-refresh",
                    "status": "active",
                    "cachedQuota": { "rate_limit": { "allowed": true, "limit_reached": false, "used_percent": 5 } },
                    "quotaFetchedAt": "2026-06-19T14:00:00Z", "quotaVerifyRequired": false,
                    "proxyApiKey": "exported-key-prefix", "usage": {"requestCount": 9}
                }]
            }).to_string())).unwrap(),
    ).await.unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let stored = SqliteAccountStore::new(pool.clone(), secret_box)
        .get("acct_import_update")
        .await
        .unwrap()
        .unwrap();
    let quota_state: (i64, Option<String>, i64, Option<String>) = sqlx::query_as(
        "select quota_limit_reached, quota_cooldown_until, quota_verify_required, quota_json from accounts where id = ?",
    ).bind("acct_import_update").fetch_one(&pool).await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["sourceFormat"], "sub2api");
    assert_eq!(stored.email.as_deref(), Some("new@example.com"));
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(quota_state.0, 0);
    assert!(quota_state.1.is_none());
}

#[tokio::test]
async fn admin_accounts_import_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts-import-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let config = test_config(url);
    let secret_box = SecretBox::new([111u8; 32]);
    let hasher = ApiKeyHasher::new([112u8; 32]);
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
    let fingerprint = crate::support::fingerprint::test_fingerprint();
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
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("x-request-id", "req_accounts_import_auth")
                .body(Body::from(r#"{"accounts":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response_json(response).await["code"], 40101);
}

#[tokio::test]
async fn admin_accounts_import_should_store_tokens_encrypted_and_list_sanitized_accounts() {
    let (app, _state, pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-import-sanitized.sqlite", 113).await;
    let access_token = test_jwt(
        "chatgpt-account",
        Some("chatgpt-user"),
        Some("user@example.com"),
        Some("plus"),
    );

    let response = app.clone().oneshot(
        Request::builder().method("POST").uri("/api/admin/accounts/import")
            .header("content-type", "application/json")
            .header("cookie", "cpr_admin_session=session_1")
            .header("x-request-id", "req_accounts_import_sanitized")
            .body(Body::from(json!({
                "accounts": [{
                    "id": "acct_imported_sanitized", "email": "user@example.com",
                    "accountId": "chatgpt-account", "userId": "chatgpt-user",
                    "label": "primary", "planType": "plus",
                    "token": access_token.clone(), "refreshToken": "refresh-secret", "status": "active"
                }]
            }).to_string())).unwrap(),
    ).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["imported"], 1);

    let stored: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where id = ?",
    )
    .bind("acct_imported_sanitized")
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(stored.0.starts_with("v1:"));
    assert!(!stored.0.contains(&access_token));
    assert!(stored.1.starts_with("v1:"));
    assert!(!stored.1.contains("refresh-secret"));

    let list = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = response_json(list).await;
    assert_eq!(
        list_body["data"]["items"][0]["id"],
        "acct_imported_sanitized"
    );
    assert!(list_body["data"]["items"][0].get("token").is_none());
}

#[tokio::test]
async fn admin_accounts_export_should_return_native_accounts_with_tokens_and_filter_ids() {
    let (app, _state, pool, _dir, secret_box) =
        admin_accounts_test_app("admin-accounts-export-filter.sqlite", 117).await;
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_export_a".to_string(),
            email: Some("export-a@example.com".to_string()),
            account_id: Some("chatgpt-export-a".to_string()),
            user_id: Some("user-export-a".to_string()),
            label: None,
            plan_type: Some("plus".to_string()),
            access_token: SecretString::new("access-acct_export_a".to_string().into()),
            refresh_token: Some(SecretString::new(
                "refresh-acct_export_a".to_string().into(),
            )),
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/export?ids=acct_export_a")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["accounts"][0]["id"], "acct_export_a");
}
