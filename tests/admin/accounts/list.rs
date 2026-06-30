use super::*;

#[tokio::test]
async fn admin_accounts_list_should_not_expose_account_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-accounts.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    sqlx::query("insert into accounts (id, email, access_token, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?)")
        .bind("acct_plain").bind("user@example.com").bind("plain-access-token")
        .bind("active").bind("2026-06-18T00:00:00Z").bind("2026-06-18T00:00:00Z")
        .execute(&pool).await.unwrap();
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
    assert_eq!(body["data"]["items"][0]["id"], "acct_plain");
    assert_eq!(body["data"]["items"][0]["email"], "user@example.com");
    assert_eq!(
        body["data"]["items"][0]["addedAt"],
        "2026-06-18T08:00:00+08:00"
    );
}

#[tokio::test]
async fn admin_accounts_list_should_include_usage_quota_and_model_stats() {
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-accounts-stats.sqlite", 68).await;
    seed_usage_account(
        &pool,
        UsageAccountSeed {
            id: "acct_stats",
            email: "stats@example.com",
            label: "stats",
            plan_type: "free",
            request_count: 41,
            empty_response_count: 5,
            input_tokens: 3_500_000,
            output_tokens: 13_900,
            cached_tokens: 3_400_000,
            window_request_count: 7,
            window_input_tokens: 120_000,
            window_output_tokens: 4_000,
            window_cached_tokens: 40_000,
            window_started_at: "2026-06-20T00:00:00Z",
            window_reset_at: "2026-06-27T00:00:00Z",
            limit_window_seconds: 604_800,
            last_used_at: "2026-06-23T08:50:13Z",
        },
    )
    .await;
    sqlx::query("update accounts set quota_json = ?, quota_fetched_at = ? where id = ?")
        .bind(
            json!({
                "plan_type": "free",
                "snapshots": [{
                    "source": "core",
                    "limit_name": null,
                    "metered_feature": null,
                    "allowed": true,
                    "limit_reached": false,
                    "blocked": false,
                    "primary": {
                        "used_percent": 87.8,
                        "remaining_percent": 12,
                        "reset_at": 1782737460,
                        "window_minutes": 10080,
                        "limit_reached": false
                    },
                    "secondary": {
                        "used_percent": 12.4,
                        "remaining_percent": 88,
                        "reset_at": 1782140000,
                        "window_minutes": 300,
                        "limit_reached": false
                    }
                }, {
                    "source": "core",
                    "limit_name": null,
                    "metered_feature": null,
                    "allowed": true,
                    "limit_reached": false,
                    "blocked": false,
                    "primary": {
                        "used_percent": 0,
                        "remaining_percent": 100,
                        "limit_reached": false
                    },
                    "secondary": null
                }],
                "monthly_limit": {
                    "key": "spend-control-monthly",
                    "source": "spend_control",
                    "used_percent": 32,
                    "remaining_percent": 68,
                    "reset_at": 1784268840,
                    "window_minutes": 43200,
                    "limit_reached": false
                },
                "spend_control": {
                    "reached": false,
                    "individual_limit": {
                        "used_percent": 32,
                        "remaining_percent": 68,
                        "reset_at": 1784268840
                    }
                }
            })
            .to_string(),
        )
        .bind("2026-06-23T08:51:09Z")
        .bind("acct_stats")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into account_model_usage (account_id, model, request_count, input_tokens, output_tokens, cached_tokens, last_used_at) values (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_stats")
    .bind("gpt-5.5")
    .bind(2)
    .bind(3_500_100)
    .bind(13_920)
    .bind(3_400_010)
    .bind("2026-06-23T08:51:13Z")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into account_model_usage (account_id, model, request_count, input_tokens, output_tokens, cached_tokens, last_used_at) values (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_stats")
    .bind("gpt-5")
    .bind(1)
    .bind(200)
    .bind(30)
    .bind(20)
    .bind("2026-06-23T08:52:13Z")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into usage_time_buckets (bucket_start, account_id, model, request_count, input_tokens, output_tokens, cached_tokens, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("2026-06-23T08:45:00+00:00")
    .bind("acct_stats")
    .bind("gpt-5")
    .bind(3)
    .bind(60_000)
    .bind(1_500)
    .bind(20_000)
    .bind("2026-06-23T08:45:00Z")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into usage_time_buckets (bucket_start, account_id, model, request_count, input_tokens, output_tokens, cached_tokens, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("2026-06-23T09:00:00+00:00")
    .bind("acct_stats")
    .bind("gpt-5.5")
    .bind(4)
    .bind(60_000)
    .bind(2_500)
    .bind(20_000)
    .bind("2026-06-23T09:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into usage_time_buckets (bucket_start, account_id, model, request_count, input_tokens, output_tokens, cached_tokens, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("2026-06-19T23:45:00+00:00")
    .bind("acct_stats")
    .bind("gpt-outside-window")
    .bind(99)
    .bind(900_000)
    .bind(90_000)
    .bind(9_000)
    .bind("2026-06-19T23:45:00Z")
    .execute(&pool)
    .await
    .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts?page=1&pageSize=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_accounts_stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let item = &body["data"]["items"][0];
    assert_eq!(item["usage"]["requestCount"], 7);
    assert_eq!(item["usage"]["inputTokens"], 120_000);
    assert_eq!(item["usage"]["outputTokens"], 4_000);
    assert_eq!(item["usage"]["cachedTokens"], 40_000);
    assert_eq!(item["usage"]["totalTokens"], 124_000);
    assert_eq!(item["quota"]["windows"].as_array().unwrap().len(), 3);
    assert_eq!(item["quota"]["windows"][0]["labelDisplay"], "月限额");
    assert_eq!(item["quota"]["windows"][0]["group"], "monthly");
    assert_eq!(item["quota"]["windows"][1]["labelDisplay"], "5小时限额");
    assert_eq!(item["quota"]["windows"][1]["group"], "shortTerm");
    assert_eq!(item["quota"]["windows"][2]["labelDisplay"], "周限额");
    assert_eq!(item["quota"]["windows"][2]["group"], "shortTerm");
    assert!(item["quota"]["windows"][2]["windowUsedDisplay"]
        .as_str()
        .is_some_and(|value| value.contains(" / 7.0d")));
    assert_eq!(item["usage"]["createdTokens"], 80_000);
    assert_eq!(item["usage"]["readTokens"], 40_000);
    assert_eq!(item["usage"]["models"].as_array().unwrap().len(), 2);
    assert_eq!(item["usage"]["models"][0]["model"], "gpt-5.5");
    assert_eq!(item["usage"]["models"][0]["requestCount"], 4);
    assert_eq!(item["usage"]["models"][1]["model"], "gpt-5");
}

#[tokio::test]
async fn admin_accounts_list_should_return_numbered_page_with_search_total() {
    let (app, _state, pool, _dir) =
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
        sqlx::query("insert into accounts (id, email, label, access_token, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?, ?)")
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
    assert_eq!(body["data"]["summary"]["total"], 3);
    assert_eq!(body["data"]["summary"]["active"], 3);
}
