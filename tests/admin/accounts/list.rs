use super::*;

#[tokio::test]
async fn admin_accounts_list_should_not_decrypt_account_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    sqlx::query("insert into accounts (id, email, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)")
        .bind("acct_corrupt").bind("user@example.com").bind("not-a-secret-box-cipher")
        .bind("active").bind("2026-06-18T00:00:00Z").bind("2026-06-18T00:00:00Z")
        .execute(&pool).await.unwrap();
    let config = test_config(url);
    let secret_box = SecretBox::new([63u8; 32]);
    let hasher = ApiKeyHasher::new([64u8; 32]);
    let stores = BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone(), secret_box),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone(), SecretBox::new([63u8; 32])),
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
                .uri("/api/admin/accounts?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["items"][0]["id"], "acct_corrupt");
    assert_eq!(body["data"]["items"][0]["email"], "user@example.com");
}

#[tokio::test]
async fn admin_accounts_list_should_return_numbered_page_with_search_total() {
    let (app, _state, pool, _dir, _secret_box) =
        admin_accounts_test_app("admin-accounts-numbered.sqlite", 67).await;
    for (id, email, label, added_at) in [
        (
            "acct_prod_new",
            "new-prod@example.com",
            "prod primary",
            "2026-06-18T00:02:00Z",
        ),
        (
            "acct_stage",
            "stage@example.com",
            "stage",
            "2026-06-18T00:01:00Z",
        ),
        (
            "acct_prod_old",
            "old@example.com",
            "prod backup",
            "2026-06-18T00:00:00Z",
        ),
    ] {
        sqlx::query("insert into accounts (id, email, label, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?, ?)")
            .bind(id)
            .bind(email)
            .bind(label)
            .bind("cipher")
            .bind("active")
            .bind(added_at)
            .bind(added_at)
            .execute(&pool)
            .await
            .unwrap();
    }

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts?page=1&pageSize=1&search=prod")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_numbered")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["items"][0]["id"], "acct_prod_new");
    assert_eq!(body["data"]["page"]["page"], 1);
    assert_eq!(body["data"]["page"]["pageSize"], 1);
    assert_eq!(body["data"]["page"]["total"], 2);
    assert_eq!(body["data"]["page"]["totalPages"], 2);
}
