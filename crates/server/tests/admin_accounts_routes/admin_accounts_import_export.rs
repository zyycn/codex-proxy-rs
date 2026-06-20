use super::*;

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
        },
    )
    .await;
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        secret_box,
        ApiKeyHasher::new([78u8; 32]),
    );
    let app = router::router().with_state(state);

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
    assert_eq!(body["requestId"], "req_accounts_export");
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
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([80u8; 32]),
    );
    let app = router::router().with_state(state);

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
                            "id": "acct_import",
                            "email": "import@example.com",
                            "accountId": "chatgpt_import",
                            "userId": "user_import",
                            "label": "secondary",
                            "planType": "team",
                            "token": "access-import",
                            "refreshToken": "refresh-import",
                            "accessTokenExpiresAt": "2026-06-18T02:00:00Z",
                            "status": "disabled"
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
    assert_eq!(body["requestId"], "req_accounts_import");
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["skipped"], 0);
    assert_eq!(body["data"]["sourceFormat"], "native");
    assert_eq!(stored.access_token.expose_secret(), "access-import");
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "refresh-import"
    );
    assert_eq!(stored.status, AccountStatus::Disabled);
}

#[tokio::test]
async fn admin_accounts_import_should_update_existing_sub2api_account_and_clear_stale_quota_lock() {
    let (app, state, pool, _dir, secret_box) =
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
        },
    )
    .await;
    sqlx::query(
        "update accounts set quota_limit_reached = 1, quota_cooldown_until = ?, quota_verify_required = 1 where id = ?",
    )
    .bind("2026-06-25T00:00:00Z")
    .bind("acct_import_update")
    .execute(&pool)
    .await
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_update")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_import_update",
                            "email": "new@example.com",
                            "accountId": "chatgpt-import-update",
                            "userId": "user-import-update",
                            "label": "new-label",
                            "planType": "plus",
                            "token": new_access_token,
                            "refreshToken": "new-refresh",
                            "status": "active",
                            "cachedQuota": {
                                "rate_limit": {
                                    "allowed": true,
                                    "limit_reached": false,
                                    "used_percent": 5
                                }
                            },
                            "quotaFetchedAt": "2026-06-19T14:00:00Z",
                            "quotaVerifyRequired": false,
                            "proxyApiKey": "exported-key-prefix",
                            "usage": {"requestCount": 9}
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
        .get("acct_import_update")
        .await
        .unwrap()
        .unwrap();
    let quota_state: (i64, Option<String>, i64, Option<String>) = sqlx::query_as(
        "select quota_limit_reached, quota_cooldown_until, quota_verify_required, quota_json from accounts where id = ?",
    )
    .bind("acct_import_update")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["requestId"], "req_accounts_import_update");
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["skipped"], 0);
    assert_eq!(body["data"]["sourceFormat"], "sub2api");
    assert_eq!(stored.email.as_deref(), Some("new@example.com"));
    assert_eq!(stored.label.as_deref(), Some("new-label"));
    assert_eq!(stored.plan_type.as_deref(), Some("plus"));
    assert_eq!(stored.access_token.expose_secret(), new_access_token);
    assert_eq!(stored.refresh_token.unwrap().expose_secret(), "new-refresh");
    assert_eq!(stored.status, AccountStatus::Active);
    assert_eq!(quota_state.0, 0);
    assert!(quota_state.1.is_none());
    assert_eq!(quota_state.2, 0);
    assert!(quota_state.3.is_some());
    let acquired = state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .unwrap()
        .account;
    assert_eq!(acquired.id, "acct_import_update");
    assert_eq!(acquired.access_token, new_access_token);
}

#[tokio::test]
async fn admin_accounts_import_should_update_existing_identity_when_import_id_changed() {
    let (app, _state, pool, _dir, secret_box) =
        admin_accounts_test_app("admin-accounts-import-update-existing-identity.sqlite", 126).await;
    let new_access_token = test_jwt(
        "chatgpt-import-identity",
        Some("user-import-identity"),
        Some("identity-new@example.com"),
        Some("plus"),
    );
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_existing_identity".to_string(),
            email: Some("identity-old@example.com".to_string()),
            account_id: Some("chatgpt-import-identity".to_string()),
            user_id: Some("user-import-identity".to_string()),
            label: Some("old-label".to_string()),
            plan_type: Some("free".to_string()),
            access_token: SecretString::new("old-access".to_string().into()),
            refresh_token: Some(SecretString::new("old-refresh".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Disabled,
        },
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_update_identity")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_changed_export_id",
                            "email": "identity-new@example.com",
                            "accountId": "chatgpt-import-identity",
                            "userId": "user-import-identity",
                            "label": "new-label",
                            "planType": "plus",
                            "token": new_access_token,
                            "refreshToken": "new-refresh",
                            "status": "active",
                            "cachedQuota": {
                                "rate_limit": {
                                    "allowed": true,
                                    "limit_reached": false,
                                    "used_percent": 6
                                }
                            }
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
    let store = SqliteAccountStore::new(pool.clone(), secret_box);
    let stored = store.get("acct_existing_identity").await.unwrap().unwrap();
    let changed_id = store.get("acct_changed_export_id").await.unwrap();
    let account_count: i64 = sqlx::query_scalar("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["requestId"], "req_accounts_import_update_identity");
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["skipped"], 0);
    assert_eq!(body["data"]["sourceFormat"], "sub2api");
    assert_eq!(account_count, 1);
    assert!(changed_id.is_none());
    assert_eq!(stored.email.as_deref(), Some("identity-new@example.com"));
    assert_eq!(stored.label.as_deref(), Some("new-label"));
    assert_eq!(stored.plan_type.as_deref(), Some("plus"));
    assert_eq!(stored.access_token.expose_secret(), new_access_token);
    assert_eq!(stored.refresh_token.unwrap().expose_secret(), "new-refresh");
    assert_eq!(stored.status, AccountStatus::Active);
}

#[tokio::test]
async fn admin_accounts_import_should_expire_sub2api_account_when_token_is_expired() {
    let (app, state, pool, _dir, secret_box) =
        admin_accounts_test_app("admin-accounts-import-expired-sub2api.sqlite", 117).await;
    let expired_access_token = test_jwt_with_exp(
        Some("chatgpt-expired-import"),
        Some("user-expired-import"),
        Some("expired@example.com"),
        Some("free"),
        1_600_000_000,
    );

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_expired_sub2api")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_expired_import",
                            "email": "expired@example.com",
                            "accountId": "chatgpt-expired-import",
                            "userId": "user-expired-import",
                            "label": "expired-label",
                            "planType": "free",
                            "token": expired_access_token,
                            "refreshToken": "expired-refresh",
                            "status": "active",
                            "cachedQuota": {
                                "rate_limit": {
                                    "allowed": true,
                                    "limit_reached": false,
                                    "used_percent": 5
                                }
                            }
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
        .get("acct_expired_import")
        .await
        .unwrap()
        .unwrap();
    let auth_status = state.services.admin_accounts.auth_status().await.unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["sourceFormat"], "sub2api");
    assert_eq!(stored.status, AccountStatus::Expired);
    assert_eq!(auth_status.pool.active, 0);
    assert_eq!(auth_status.pool.expired, 1);
    assert!(auth_status.user.is_none());
    assert!(state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .is_none());
}

#[tokio::test]
async fn admin_accounts_import_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts-import-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([111u8; 32]),
        ApiKeyHasher::new([112u8; 32]),
    );
    let app = router::router().with_state(state);

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
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_accounts_import_auth");
}

#[tokio::test]
async fn admin_accounts_import_should_store_tokens_encrypted_and_list_sanitized_accounts() {
    let (app, _state, pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-import-sanitized.sqlite", 113).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_sanitized")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_imported_sanitized",
                            "email": "user@example.com",
                            "accountId": "chatgpt-account",
                            "userId": "chatgpt-user",
                            "label": "primary",
                            "planType": "plus",
                            "token": "access-secret",
                            "refreshToken": "refresh-secret",
                            "status": "active"
                        }]
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
    assert_eq!(body["data"]["skipped"], 0);

    let stored: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where id = ?",
    )
    .bind("acct_imported_sanitized")
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
                .uri("/api/admin/accounts?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_status = list_response.status();
    let list_body = response_json(list_response).await;

    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list_body["data"][0]["id"], "acct_imported_sanitized");
    assert_eq!(list_body["data"][0]["email"], "user@example.com");
    assert!(list_body["data"][0].get("token").is_none());
    assert!(list_body["data"][0].get("refreshToken").is_none());
    assert_eq!(list_body["page"]["limit"], 10);
}

#[tokio::test]
async fn admin_accounts_import_should_reject_non_native_export_shape() {
    let (app, _state, pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-import-external-shape.sqlite", 114).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_external_import")
                .body(Body::from(
                    json!({
                        "type": "external-data",
                        "version": 1,
                        "legacy": [],
                        "accounts": [{
                            "platform": "openai",
                            "type": "oauth",
                            "credentials": {
                                "access_token": "Bearer external-access-secret",
                                "refresh_token": "rt_external",
                                "email": "team@example.com",
                                "chatgpt_account_id": "chatgpt-account",
                                "chatgpt_user_id": "chatgpt-user",
                                "plan_type": "team"
                            }
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
    let stored: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "No importable accounts found");
    assert_eq!(stored.0, 0);
}

#[tokio::test]
async fn admin_accounts_import_should_reject_native_payload_with_unknown_account_fields() {
    let (app, _state, _pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-import-unknown-account.sqlite", 115).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_native_unknown_account")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_native_unknown",
                            "token": "native-access-secret",
                            "refreshToken": "native-refresh-secret",
                            "email": "native@example.com",
                            "accountId": "native-account",
                            "userId": "native-user",
                            "label": "Native Unknown",
                            "planType": "plus",
                            "legacyField": "ignored-secret",
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

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "No importable accounts found");
}

#[tokio::test]
async fn admin_accounts_import_should_reject_native_payload_with_unknown_container_fields() {
    let (app, _state, _pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-import-unknown-container.sqlite", 116).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_native_unknown_container")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_native_extra_container",
                            "token": "native-access-secret",
                            "refreshToken": "native-refresh-secret",
                            "email": "native@example.com",
                            "accountId": "native-account",
                            "userId": "native-user",
                            "label": "Native",
                            "planType": "plus",
                            "status": "active"
                        }],
                        "legacyContainer": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "No importable accounts found");
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
        },
    )
    .await;
    seed_encrypted_account(
        &pool,
        secret_box,
        NewAccount {
            id: "acct_export_b".to_string(),
            email: Some("export-b@example.com".to_string()),
            account_id: Some("chatgpt-export-b".to_string()),
            user_id: Some("user-export-b".to_string()),
            label: None,
            plan_type: Some("team".to_string()),
            access_token: SecretString::new("access-acct_export_b".to_string().into()),
            refresh_token: Some(SecretString::new(
                "refresh-acct_export_b".to_string().into(),
            )),
            access_token_expires_at: None,
            status: AccountStatus::Active,
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
    assert_eq!(body["data"]["sourceFormat"], "native");
    assert_eq!(body["data"]["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["accounts"][0]["id"], "acct_export_a");
    assert_eq!(body["data"]["accounts"][0]["token"], "access-acct_export_a");
    assert_eq!(
        body["data"]["accounts"][0]["refreshToken"],
        "refresh-acct_export_a"
    );
    assert!(body["data"]["accounts"][0].get("legacyField").is_none());
}

#[tokio::test]
async fn admin_accounts_export_should_reject_unsupported_external_format() {
    let (app, _state, _pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-export-external.sqlite", 118).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/export?format=external")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_accounts_export_should_reject_unsupported_full_format() {
    let (app, _state, _pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-export-full.sqlite", 119).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/export?format=full")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
        admin_accounts_test_app_with_oauth_client("admin-auth-device-pending.sqlite", 112, client)
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
        "device-account",
        Some("device-user"),
        Some("device@example.com"),
        Some("plus"),
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
        admin_accounts_test_app_with_oauth_client("admin-auth-device-success.sqlite", 113, client)
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
    let stored = SqliteAccountStore::new(pool.clone(), SecretBox::new([113u8; 32]))
        .get(
            state
                .services
                .account_pool
                .acquire("gpt-5.5", Utc::now())
                .await
                .unwrap()
                .account
                .id
                .as_str(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.email.as_deref(), Some("device@example.com"));
    assert_eq!(stored.account_id.as_deref(), Some("device-account"));
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "device-refresh-secret"
    );
}

#[tokio::test]
async fn admin_auth_code_relay_should_exchange_code_and_import_account() {
    let token = test_jwt(
        "pkce-account",
        Some("pkce-user"),
        Some("pkce@example.com"),
        Some("plus"),
    );
    let exchange_calls = Arc::new(Mutex::new(Vec::new()));
    let client = StaticOAuthClient {
        device_response: Err(OAuthError::Rejected("not used".to_string())),
        poll_response: Err(OAuthError::AuthorizationPending),
        exchange_response: Ok(TokenPair {
            access_token: token,
            refresh_token: Some("pkce-refresh-secret".to_string()),
        }),
        poll_calls: Arc::new(Mutex::new(Vec::new())),
        exchange_calls: exchange_calls.clone(),
    };
    let (app, state, _pool, _dir) =
        admin_accounts_test_app_with_oauth_client("admin-auth-code-relay.sqlite", 116, client)
            .await;
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
        format!("http://localhost:1455/auth/callback?code=oauth-code&state={state_value}");

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/auth/code-relay")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_code_relay")
                .body(Body::from(json!({"callbackUrl": callback_url}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["success"], true);
    assert!(body["data"].get("accessToken").is_none());
    assert!(body["data"].get("refreshToken").is_none());
    let calls = exchange_calls.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].code, "oauth-code");
    assert!(!calls[0].code_verifier.is_empty());
    assert_eq!(calls[0].redirect_uri, "http://localhost:1455/auth/callback");
    drop(calls);
    let acquired = state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .unwrap()
        .account;
    assert_eq!(acquired.account_id.as_deref(), Some("pkce-account"));
}

#[tokio::test]
async fn admin_accounts_import_cli_should_read_auth_file_store_encrypted_and_sync_pool() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-import-cli.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([103u8; 32]);
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool.clone(),
        secret_box.clone(),
        ApiKeyHasher::new([104u8; 32]),
    );
    let app = router::router().with_state(state.clone());
    let codex_home = dir.path().join("codex-home");
    fs::create_dir_all(&codex_home).unwrap();
    let token = test_jwt(
        "cli-account",
        Some("cli-user"),
        Some("cli@example.com"),
        Some("plus"),
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
                .uri("/api/admin/accounts/import-cli")
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
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["requestId"], "req_import_cli");
    assert_eq!(body["data"]["sourceFormat"], "codex_cli");
    assert_eq!(body["data"]["imported"], 1);
    assert!(body["data"].get("token").is_none());
    assert!(body["data"].get("refreshToken").is_none());

    let stored = SqliteAccountStore::new(pool.clone(), secret_box)
        .list(None, 10)
        .await
        .unwrap()
        .items
        .remove(0);
    assert_ne!(stored.id, "caller-id");
    assert_eq!(stored.email.as_deref(), Some("cli@example.com"));
    assert_eq!(stored.account_id.as_deref(), Some("cli-account"));
    assert_eq!(stored.user_id.as_deref(), Some("cli-user"));
    assert_eq!(stored.label, None);
    assert_eq!(stored.plan_type.as_deref(), Some("plus"));
    assert_eq!(stored.access_token.expose_secret(), &token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "cli-refresh-secret"
    );
    let raw: (String, String) = sqlx::query_as(
        "select access_token_cipher, refresh_token_cipher from accounts where id = ?",
    )
    .bind(&stored.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(raw.0.starts_with("v1:"));
    assert!(!raw.0.contains(&token));
    assert!(raw.1.starts_with("v1:"));
    assert!(!raw.1.contains("cli-refresh-secret"));

    let acquired = state
        .services
        .account_pool
        .acquire("gpt-5.5", Utc::now())
        .await
        .unwrap()
        .account;
    assert_eq!(acquired.id, stored.id);
}
