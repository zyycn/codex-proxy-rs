use super::*;
use crate::support::jwt::unsigned_jwt;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn admin_accounts_import_should_store_cpr_account_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts-import.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let access_token = test_jwt(
        "chatgpt_import",
        Some("user_import"),
        Some("import@example.com"),
        Some("team"),
    );
    let config = test_config(url);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        usage_records: SqliteUsageRecordStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = Arc::new(Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let state = AppState {
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
    let stored = SqliteAccountStore::new(pool)
        .get("acct_import")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["skipped"], 0);
    assert_eq!(body["data"]["sourceFormat"], "cpr");
    assert_eq!(stored.access_token.expose_secret(), access_token);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "refresh-import"
    );
    assert_eq!(stored.status, AccountStatus::Disabled);
}

#[tokio::test]
async fn admin_accounts_import_should_store_cpr_access_token_from_at_field() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "account_id": "cpr-at-account",
            "user_id": "cpr-at-user",
            "email": "cpr-at@example.com",
            "plan_type": "k12",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 9,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 18_000
                }
            }
        })))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-accounts-import-cpr-at.sqlite",
        129,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    let access_token = unsigned_jwt(&json!({
        "exp": 4_102_444_800i64,
        "https://api.openai.com/auth": {},
        "https://api.openai.com/profile": {},
    }));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_cpr_at")
                .body(Body::from(
                    json!({
                        "accounts": [{
                            "id": "acct_import_cpr_at",
                            "at": access_token.clone(),
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
    let stored = SqliteAccountStore::new(pool)
        .get("acct_import_cpr_at")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["sourceFormat"], "cpr");
    assert_eq!(stored.account_id.as_deref(), Some("cpr-at-account"));
    assert_eq!(stored.user_id.as_deref(), Some("cpr-at-user"));
    assert_eq!(stored.email.as_deref(), Some("cpr-at@example.com"));
    assert_eq!(stored.plan_type.as_deref(), Some("k12"));
    assert_eq!(stored.access_token.expose_secret(), access_token);
    assert!(stored.refresh_token.is_none());
    assert!(stored.next_refresh_at.is_none());
}

#[tokio::test]
async fn admin_accounts_import_should_read_sub2api_backup_access_token_only_accounts() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "account_id": "sub2api-at-account",
            "user_id": "sub2api-at-user",
            "email": "sub2api-at@example.com",
            "plan_type": "k12",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 7,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 18_000
                }
            }
        })))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-accounts-import-sub2api-backup-at.sqlite",
        127,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    let access_token = unsigned_jwt(&json!({
        "exp": 4_102_444_800i64,
        "https://api.openai.com/auth": {},
        "https://api.openai.com/profile": {},
    }));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_sub2api_backup_at")
                .body(Body::from(
                    json!({
                        "sourceFormat": "sub2api",
                        "exported_at": "2026-07-03T15:46:38.717Z",
                        "proxies": [],
                        "accounts": [{
                            "name": "sub2api-at@example.com",
                            "platform": "openai",
                            "type": "oauth",
                            "credentials": {
                                "at": access_token.clone(),
                                "refresh_token": "",
                                "expires_at": 4_102_444_800i64
                            },
                            "extra": {},
                            "concurrency": 3,
                            "priority": 50,
                            "rate_multiplier": null,
                            "auto_pause_on_expired": true
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
    let store = SqliteAccountStore::new(pool);
    let stored_metadata = store
        .list_metadata_page(1, 10, None)
        .await
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();
    let stored = store.get(&stored_metadata.id).await.unwrap().unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["sourceFormat"], "sub2api");
    assert_eq!(stored.account_id.as_deref(), Some("sub2api-at-account"));
    assert_eq!(stored.user_id.as_deref(), Some("sub2api-at-user"));
    assert_eq!(stored.email.as_deref(), Some("sub2api-at@example.com"));
    assert_eq!(stored.plan_type.as_deref(), Some("k12"));
    assert_eq!(stored.access_token.expose_secret(), access_token);
    assert!(stored.refresh_token.is_none());
    assert!(stored.next_refresh_at.is_none());
}

#[tokio::test]
async fn admin_accounts_import_should_read_cliproxyapi_codex_auth_files() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "account_id": "cliproxyapi-account",
            "user_id": "cliproxyapi-user",
            "email": "cliproxyapi@example.com",
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 3,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 18_000
                }
            }
        })))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-accounts-import-cliproxyapi.sqlite",
        128,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    let access_token = unsigned_jwt(&json!({
        "exp": 4_102_444_800i64,
        "https://api.openai.com/auth": {},
        "https://api.openai.com/profile": {},
    }));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_cliproxyapi")
                .body(Body::from(
                    json!({
                        "sourceFormat": "cliproxyapi",
                        "accounts": [{
                            "type": "codex",
                            "access_token": access_token.clone(),
                            "refresh_token": "cliproxyapi-refresh",
                            "expired": "2100-01-01T00:00:00Z",
                            "email": "cliproxyapi@example.com",
                            "label": "CLIProxyAPI"
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
    let stored = SqliteAccountStore::new(pool)
        .list_metadata_page(1, 10, None)
        .await
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["sourceFormat"], "cliproxyapi");
    assert_eq!(stored.account_id.as_deref(), Some("cliproxyapi-account"));
    assert_eq!(stored.user_id.as_deref(), Some("cliproxyapi-user"));
    assert_eq!(stored.email.as_deref(), Some("cliproxyapi@example.com"));
    assert_eq!(stored.plan_type.as_deref(), Some("plus"));
}

#[tokio::test]
async fn admin_accounts_import_should_fetch_wham_usage_for_current_openai_token_identity() {
    const CURRENT_IMPORT_ACCOUNT_ID: &str = "acct_import_current_token";
    const CURRENT_IMPORT_EMAIL: &str = "current-import@example.test";
    const CURRENT_IMPORT_ORG_ID: &str = "org-import-current";
    const CURRENT_IMPORT_USER_ID: &str = "user-import-current";

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user_id": CURRENT_IMPORT_USER_ID,
            "account_id": "wham-account-current",
            "email": CURRENT_IMPORT_EMAIL,
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
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_api_base_url(
        "admin-accounts-import-current-openai-token.sqlite",
        118,
        format!("{}/backend-api", server.uri()),
    )
    .await;
    let payload = json!({
        "exp": 4_102_444_800i64,
        "https://api.openai.com/auth": {
            "user_id": CURRENT_IMPORT_USER_ID,
            "poid": CURRENT_IMPORT_ORG_ID,
        },
        "https://api.openai.com/profile": {
            "email": CURRENT_IMPORT_EMAIL,
            "email_verified": true,
        },
    });
    let access_token = unsigned_jwt(&payload);

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
                            "id": CURRENT_IMPORT_ACCOUNT_ID,
                            "email": CURRENT_IMPORT_EMAIL,
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
    let stored = SqliteAccountStore::new(pool)
        .get(CURRENT_IMPORT_ACCOUNT_ID)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(stored.email.as_deref(), Some(CURRENT_IMPORT_EMAIL));
    assert_eq!(stored.account_id.as_deref(), Some("wham-account-current"));
    assert_eq!(stored.user_id.as_deref(), Some(CURRENT_IMPORT_USER_ID));
    assert_eq!(stored.plan_type.as_deref(), Some("free"));
    assert_eq!(stored.access_token.expose_secret(), access_token);
    assert_eq!(stored.status, AccountStatus::Active);
}

#[tokio::test]
async fn admin_accounts_import_should_complete_chatgpt_account_id_from_refresh_token() {
    let server = MockServer::start().await;
    let access_token = test_jwt_with_exp(
        None,
        Some("rt-import-user"),
        Some("rt-import@example.com"),
        None,
        4_102_444_800,
    );
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": access_token,
            "refresh_token": "rt-import-rotated"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "account_id": "wham-account-from-rt",
            "user_id": "rt-import-user",
            "email": "rt-import@example.com",
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 0,
                    "reset_at": 1_800_000_000,
                    "limit_window_seconds": 18_000
                }
            }
        })))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) =
        admin_accounts_test_app_with_api_base_url_and_oauth_token_endpoint(
            "admin-accounts-import-rt-complete.sqlite",
            125,
            format!("{}/backend-api", server.uri()),
            format!("{}/oauth/token", server.uri()),
        )
        .await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_rt_complete")
                .body(Body::from(
                    json!({
                        "accounts": [{ "refreshToken": "rt-import-source" }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let store = SqliteAccountStore::new(pool);
    let stored = store
        .list_metadata_page(1, 10, None)
        .await
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();
    let list = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts?page=1&pageSize=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_rt_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = response_json(list).await;

    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(stored.account_id.as_deref(), Some("wham-account-from-rt"));
    assert_eq!(
        list_body["data"]["items"][0]["accountId"],
        "wham-account-from-rt"
    );
}

#[tokio::test]
async fn admin_accounts_import_from_refresh_token_should_not_store_consumed_refresh_token_when_not_rotated(
) {
    let server = MockServer::start().await;
    let access_token = test_jwt(
        "rt-import-no-rotation-account",
        Some("rt-import-no-rotation-user"),
        Some("rt-import-no-rotation@example.com"),
        Some("plus"),
    );
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": access_token,
        })))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_oauth_token_endpoint(
        "admin-accounts-import-rt-no-rotation.sqlite",
        126,
        format!("{}/oauth/token", server.uri()),
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_import_rt_no_rotation")
                .body(Body::from(
                    json!({
                        "accounts": [{ "refreshToken": "rt-import-no-rotation-source" }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let store = SqliteAccountStore::new(pool);
    let stored = store
        .list_metadata_page(1, 10, None)
        .await
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();

    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(
        stored.account_id.as_deref(),
        Some("rt-import-no-rotation-account")
    );
    let account = store.get(&stored.id).await.unwrap().unwrap();
    assert!(account.refresh_token.is_none());
    assert!(account.next_refresh_at.is_none());
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
    let mut config = test_config(url);
    config.api.base_url = format!("{}/backend-api", server.uri());
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        usage_records: SqliteUsageRecordStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = Arc::new(Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let state = AppState {
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
    let stored = SqliteAccountStore::new(pool.clone())
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
    assert_eq!(quota["snapshots"][0]["blocked"], false);

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
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-accounts-import-update-existing.sqlite", 116).await;
    let new_access_token = test_jwt(
        "chatgpt-import-update",
        Some("user-import-update"),
        Some("new@example.com"),
        Some("plus"),
    );
    seed_account(
        &pool,
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

    let response = app
        .clone()
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
                            "id": "acct_import_update", "email": "new@example.com",
                            "accountId": "chatgpt-import-update", "userId": "user-import-update",
                            "label": "new-label", "planType": "plus",
                            "token": new_access_token, "refreshToken": "new-refresh",
                            "status": "active",
                            "cachedQuota": {
                                "plan_type": "plus",
                                "snapshots": [{
                                    "source": "core",
                                    "limit_name": null,
                                    "metered_feature": null,
                                    "allowed": true,
                                    "limit_reached": false,
                                    "blocked": false,
                                    "primary": {
                                        "used_percent": 5,
                                        "remaining_percent": 95,
                                        "reset_at": null,
                                        "window_minutes": 300,
                                        "limit_reached": false
                                    },
                                    "secondary": null
                                }],
                                "monthly_limit": null,
                                "credits": null,
                                "spend_control": null
                            },
                            "quotaFetchedAt": "2026-06-19T14:00:00Z", "quotaVerifyRequired": false,
                            "proxyApiKey": "exported-key-prefix", "usage": {"requestCount": 9}
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
    let stored = SqliteAccountStore::new(pool.clone())
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
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        usage_records: SqliteUsageRecordStore::new(pool.clone()),
    };
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = Arc::new(Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let state = AppState {
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
async fn admin_accounts_import_should_store_plain_tokens_and_list_sanitized_accounts() {
    let (app, _state, pool, _dir) =
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

    let stored: (String, String) =
        sqlx::query_as("select access_token, refresh_token from accounts where id = ?")
            .bind("acct_imported_sanitized")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored.0, access_token);
    assert_eq!(stored.1, "refresh-secret");

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
