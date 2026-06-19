use super::*;

#[tokio::test]
async fn admin_usage_stats_should_return_page_and_summary() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-usage.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    sqlx::query(
        "insert into accounts (id, email, label, plan_type, access_token_cipher, status, added_at, updated_at) values (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("acct_usage")
    .bind("usage@example.com")
    .bind("primary")
    .bind("plus")
    .bind("cipher")
    .bind("active")
    .bind("2026-06-18T00:00:00Z")
    .bind("2026-06-18T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into account_usage (account_id, request_count, empty_response_count, input_tokens, output_tokens, cached_tokens, image_input_tokens, image_output_tokens, image_request_count, image_request_failed_count, last_used_at) values (?, 3, 1, 21, 8, 5, 7, 2, 1, 0, ?)",
    )
    .bind("acct_usage")
    .bind("2026-06-18T00:10:00Z")
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([73u8; 32]),
        ApiKeyHasher::new([74u8; 32]),
    );
    let app = router::router().with_state(state);

    let page_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage-stats?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_page")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let page_status = page_response.status();
    let page_body = response_json(page_response).await;

    assert_eq!(page_status, StatusCode::OK);
    assert_eq!(page_body["data"][0]["accountId"], "acct_usage");
    assert_eq!(page_body["data"][0]["email"], "usage@example.com");
    assert_eq!(page_body["data"][0]["requestCount"], 3);
    assert_eq!(page_body["data"][0]["inputTokens"], 21);

    let summary_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage-stats/summary")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let summary_status = summary_response.status();
    let summary_body = response_json(summary_response).await;

    assert_eq!(summary_status, StatusCode::OK);
    assert_eq!(summary_body["data"]["accountCount"], 1);
    assert_eq!(summary_body["data"]["requestCount"], 3);
    assert_eq!(summary_body["data"]["outputTokens"], 8);
}

#[tokio::test]
async fn admin_usage_stats_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-usage-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([125u8; 32]),
        ApiKeyHasher::new([126u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage-stats")
                .header("x-request-id", "req_usage_auth")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 40101);
    assert_eq!(body["requestId"], "req_usage_auth");
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
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([127u8; 32]),
        ApiKeyHasher::new([128u8; 32]),
    );
    let app = router::router().with_state(state);

    let first_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/usage-stats?limit=1")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_usage_cursor")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let first_status = first_response.status();
    let first_body = response_json(first_response).await;

    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(first_body["code"], 200);
    assert_eq!(first_body["requestId"], "req_usage_cursor");
    assert_eq!(first_body["data"].as_array().unwrap().len(), 1);
    assert_eq!(first_body["data"][0]["accountId"], "acct_b");
    assert_eq!(first_body["data"][0]["requestCount"], 2);
    assert_eq!(first_body["data"][0]["emptyResponseCount"], 1);
    assert_eq!(first_body["data"][0]["inputTokens"], 7);
    assert_eq!(first_body["page"]["limit"], 1);
    let cursor = first_body["page"]["nextCursor"].as_str().unwrap();

    let second_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/admin/usage-stats?limit=1&cursor={cursor}"))
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let second_status = second_response.status();
    let second_body = response_json(second_response).await;

    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(second_body["data"][0]["accountId"], "acct_a");
    assert!(second_body["page"]["nextCursor"].is_null());
}

#[tokio::test]
async fn admin_account_quota_should_fetch_usage_store_quota_and_not_return_secrets() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-quota"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 25,
                    "reset_at": 1770000400,
                    "limit_window_seconds": 3600
                }
            }
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([95u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_quota".to_string(),
            email: Some("quota@example.com".to_string()),
            account_id: Some("chatgpt-quota".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-quota".to_string().into()),
            refresh_token: Some(SecretString::new("refresh-quota".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    let mut config = test_config(url);
    config.api.base_url = upstream.uri();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        config,
        pool.clone(),
        secret_box,
        ApiKeyHasher::new([96u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/acct_quota/quota")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_quota")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();

    assert_eq!(status, StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["quota"]["plan_type"], "plus");
    assert_eq!(body["data"]["quota"]["rate_limit"]["remaining_percent"], 75);
    assert_eq!(
        body["data"]["raw"]["rate_limit"]["primary_window"]["used_percent"],
        25
    );
    let serialized = serde_json::to_string(&body).unwrap();
    assert!(!serialized.contains("access-quota"));
    assert!(!serialized.contains("refresh-quota"));

    let stored: (String,) = sqlx::query_as("select quota_json from accounts where id = ?")
        .bind("acct_quota")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stored.0.contains("\"remaining_percent\":75"));
    assert!(!stored.0.contains("access-quota"));
}

#[tokio::test]
async fn admin_account_quota_should_return_bad_gateway_when_usage_fetch_fails() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-quota-fail"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({
            "error": {
                "message": "quota unavailable"
            }
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota-fail.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([97u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_quota_fail".to_string(),
            email: Some("quota-fail@example.com".to_string()),
            account_id: Some("chatgpt-quota-fail".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-quota-fail".to_string().into()),
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
        ApiKeyHasher::new([98u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/acct_quota_fail/quota")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_quota_fail")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["code"], 50201);
    assert_eq!(body["message"], "Failed to fetch quota from Codex API");
    assert!(body["data"]["error"]
        .as_str()
        .is_some_and(|error| error.contains("quota unavailable")));
    assert_eq!(body["requestId"], "req_quota_fail");
}

#[tokio::test]
async fn admin_account_quota_should_reject_inactive_account_without_calling_upstream() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&upstream)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota-inactive.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([99u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_quota_inactive".to_string(),
            email: Some("quota-inactive@example.com".to_string()),
            account_id: Some("chatgpt-quota-inactive".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-quota-inactive".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Disabled,
        },
    )
    .await;
    let mut config = test_config(url);
    config.api.base_url = upstream.uri();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        config,
        pool.clone(),
        secret_box,
        ApiKeyHasher::new([100u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/acct_quota_inactive/quota")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_quota_inactive")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let quota_json: (Option<String>,) =
        sqlx::query_as("select quota_json from accounts where id = ?")
            .bind("acct_quota_inactive")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], 40901);
    assert_eq!(body["message"], "Account is disabled, cannot query quota");
    assert_eq!(body["requestId"], "req_quota_inactive");
    assert!(quota_json.0.is_none());
}

#[tokio::test]
async fn admin_account_quota_should_return_store_error_when_quota_persistence_fails() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-quota-store-fail"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 40,
                    "reset_at": 1770000500,
                    "limit_window_seconds": 3600
                }
            }
        })))
        .expect(1)
        .mount(&upstream)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota-store-fail.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([101u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_quota_store_fail".to_string(),
            email: Some("quota-store-fail@example.com".to_string()),
            account_id: Some("chatgpt-quota-store-fail".to_string()),
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-quota-store-fail".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    sqlx::query(
        "create trigger quota_write_denied before update of quota_json on accounts begin select raise(abort, 'quota write denied'); end",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut config = test_config(url);
    config.api.base_url = upstream.uri();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        config,
        pool.clone(),
        secret_box,
        ApiKeyHasher::new([102u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/acct_quota_store_fail/quota")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_quota_store_fail")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let quota_json: (Option<String>,) =
        sqlx::query_as("select quota_json from accounts where id = ?")
            .bind("acct_quota_store_fail")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], 50001);
    assert_eq!(body["message"], "Failed to store account quota");
    assert_eq!(body["requestId"], "req_quota_store_fail");
    assert!(quota_json.0.is_none());
}

#[tokio::test]
async fn admin_account_quota_warnings_should_require_admin_session_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota-warnings-auth.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        SecretBox::new([87u8; 32]),
        ApiKeyHasher::new([88u8; 32]),
    );
    let app = router::router().with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/accounts/quota-warnings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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
        },
    )
    .await;
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_quiet".to_string(),
            email: Some("quiet@example.com".to_string()),
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-quiet".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    sqlx::query(
        "update accounts set quota_json = ?, quota_fetched_at = ?, updated_at = ? where id = ?",
    )
    .bind(
        json!({
            "rate_limit": {
                "used_percent": 85,
                "reset_at": 1770000100
            },
            "secondary_rate_limit": {
                "used_percent": 91,
                "reset_at": 1770000200
            }
        })
        .to_string(),
    )
    .bind("2026-06-13T00:00:00Z")
    .bind("2026-06-13T00:00:00Z")
    .bind("acct_warn")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "update accounts set quota_json = ?, quota_fetched_at = ?, updated_at = ? where id = ?",
    )
    .bind(
        json!({
            "rate_limit": {
                "used_percent": 25,
                "reset_at": 1770000300
            },
            "secondary_rate_limit": null
        })
        .to_string(),
    )
    .bind("2026-06-13T01:00:00Z")
    .bind("2026-06-13T01:00:00Z")
    .bind("acct_quiet")
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        secret_box,
        ApiKeyHasher::new([90u8; 32]),
    );
    let app = router::router().with_state(state);

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
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["updatedAt"], "2026-06-13T00:00:00+00:00");
    let warnings = body["data"]["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 2);
    assert!(warnings
        .iter()
        .all(|warning| warning["accountId"] == "acct_warn"));
    assert!(warnings.iter().any(|warning| {
        warning["window"] == "primary"
            && warning["level"] == "warning"
            && warning["usedPercent"] == 85.0
            && warning["resetAt"] == 1770000100
    }));
    assert!(warnings.iter().any(|warning| {
        warning["window"] == "secondary"
            && warning["level"] == "critical"
            && warning["usedPercent"] == 91.0
            && warning["resetAt"] == 1770000200
    }));
}

#[tokio::test]
async fn admin_account_quota_warnings_should_ignore_invalid_and_below_threshold_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("admin-account-quota-warnings-edge.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let secret_box = SecretBox::new([85u8; 32]);
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_invalid_quota".to_string(),
            email: Some("invalid-quota@example.com".to_string()),
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-invalid-quota".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    seed_encrypted_account(
        &pool,
        secret_box.clone(),
        NewAccount {
            id: "acct_below_threshold".to_string(),
            email: Some("below-threshold@example.com".to_string()),
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: SecretString::new("access-below-threshold".to_string().into()),
            refresh_token: None,
            access_token_expires_at: None,
            status: AccountStatus::Active,
        },
    )
    .await;
    sqlx::query(
        "update accounts set quota_json = ?, quota_fetched_at = ?, updated_at = ? where id = ?",
    )
    .bind("{not valid json")
    .bind("2026-06-13T00:00:00Z")
    .bind("2026-06-13T00:00:00Z")
    .bind("acct_invalid_quota")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "update accounts set quota_json = ?, quota_fetched_at = ?, updated_at = ? where id = ?",
    )
    .bind(
        json!({
            "rate_limit": {
                "used_percent": 79,
                "reset_at": 1770000600
            },
            "secondary_rate_limit": {
                "used_percent": 79,
                "reset_at": 1770000700
            }
        })
        .to_string(),
    )
    .bind("2026-06-13T01:00:00Z")
    .bind("2026-06-13T01:00:00Z")
    .bind("acct_below_threshold")
    .execute(&pool)
    .await
    .unwrap();
    let state = AppState::with_pool_secret_and_api_key_hasher(
        test_config(url),
        pool,
        secret_box,
        ApiKeyHasher::new([86u8; 32]),
    );
    let app = router::router().with_state(state);

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
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["warnings"].as_array().unwrap().is_empty());
    assert!(body["data"]["updatedAt"].is_null());
}
