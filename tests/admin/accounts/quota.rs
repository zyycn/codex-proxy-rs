use super::*;

#[tokio::test]
async fn admin_usage_stats_should_return_page_and_summary() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-usage.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    sqlx::query("insert into accounts (id, email, label, plan_type, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?)")
        .bind("acct_usage").bind("usage@example.com").bind("primary").bind("plus")
        .bind("cipher").bind("active").bind("2026-06-18T00:00:00Z").bind("2026-06-18T00:00:00Z")
        .execute(&pool).await.unwrap();
    sqlx::query("insert into account_usage (account_id, request_count, empty_response_count, input_tokens, output_tokens, cached_tokens, last_used_at) values (?, 3, 1, 21, 8, 5, ?)")
        .bind("acct_usage").bind("2026-06-18T00:10:00Z").execute(&pool).await.unwrap();
    let config = test_config(url);
    let secret_box = SecretBox::new([73u8; 32]);
    let hasher = ApiKeyHasher::new([74u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), SecretBox::new([73u8; 32])),
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

    let page = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage?page=1&pageSize=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_page")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let page_body = response_json(page).await;
    assert_eq!(page_body["data"]["items"][0]["accountId"], "acct_usage");
    assert_eq!(page_body["data"]["items"][0]["requestCount"], 3);
    assert_eq!(page_body["data"]["page"]["page"], 1);
    assert_eq!(page_body["data"]["page"]["pageSize"], 10);
    assert_eq!(page_body["data"]["page"]["total"], 1);
    assert_eq!(page_body["data"]["page"]["totalPages"], 1);

    let summary = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage/summary")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(summary.status(), StatusCode::OK);
    assert_eq!(response_json(summary).await["data"]["accountCount"], 1);
}

#[tokio::test]
async fn admin_usage_stats_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-usage-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let config = test_config(url);
    let secret_box = SecretBox::new([125u8; 32]);
    let hasher = ApiKeyHasher::new([126u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), SecretBox::new([125u8; 32])),
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
                .uri("/api/admin/usage")
                .header("x-request-id", "req_usage_auth")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response_json(response).await["code"], 40101);
}

#[tokio::test]
async fn admin_usage_stats_should_cursor_page_account_usage() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-usage-cursor.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    seed_usage_account(
        &pool,
        "acct_a",
        "a@example.com",
        "primary",
        "plus",
        3,
        0,
        12,
        5,
        1,
        "2026-06-11T00:00:00Z",
    )
    .await;
    seed_usage_account(
        &pool,
        "acct_b",
        "b@example.com",
        "backup",
        "free",
        2,
        1,
        7,
        3,
        2,
        "2026-06-11T00:01:00Z",
    )
    .await;
    let config = test_config(url);
    let secret_box = SecretBox::new([127u8; 32]);
    let hasher = ApiKeyHasher::new([128u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), SecretBox::new([127u8; 32])),
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

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage?limit=1")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_cursor")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let first_body = response_json(first).await;
    assert_eq!(first_body["code"], 200);
    assert_eq!(first_body["data"]["items"].as_array().unwrap().len(), 1);
    let cursor = first_body["data"]["page"]["nextCursor"].as_str().unwrap();

    let second = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/admin/usage?limit=1&cursor={cursor}"))
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        response_json(second).await["data"]["items"][0]["accountId"],
        "acct_a"
    );
}

#[tokio::test]
async fn admin_account_quota_warnings_should_return_threshold_matches_from_cached_quota() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota-warnings.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([89u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_warn".to_string(),
            email: Some("warn@example.com".to_string()),
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-warn".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;
    sqlx::query("update accounts set quota_json = ?, quota_fetched_at = ?, updated_at = ? where id = ?")
        .bind(json!({"rate_limit": {"used_percent": 85, "reset_at": 1770000100}, "secondary_rate_limit": {"used_percent": 91, "reset_at": 1770000200}}).to_string())
        .bind("2026-06-13T00:00:00Z").bind("2026-06-13T00:00:00Z").bind("acct_warn")
        .execute(&pool).await.unwrap();
    let config = test_config(url);
    let hasher = ApiKeyHasher::new([90u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), SecretBox::new([89u8; 32])),
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
                .uri("/api/admin/accounts/quota-warnings")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let warnings = body["data"]["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 2);
}
