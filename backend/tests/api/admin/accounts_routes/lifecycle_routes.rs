use super::*;

#[tokio::test]
async fn admin_accounts_lifecycle_should_update_and_delete_accounts() {
    let (pool, _dir) = init_test_db("admin-accounts-lifecycle").await;
    let redis = create_test_redis("admin-accounts-lifecycle").await;
    seed_admin_session(&pool, &redis, "session_1").await;
    sqlx::query("insert into accounts (id, email, access_token, status, added_at, updated_at) values ($1, $2, $3, $4, $5, $6)")
        .bind("acct_lifecycle").bind("life@example.com").bind("cipher").bind("active")
        .bind(crate::support::storage::timestamp("2026-06-18T00:00:00Z")).bind(crate::support::storage::timestamp("2026-06-18T00:00:00Z")).execute(&pool).await.unwrap();
    seed_account_related_rows(&pool, "acct_lifecycle").await;
    let config = test_config(test_database_url());
    let stores = background_task_stores(pool.clone(), redis.clone());
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = Arc::new(Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let state = AppState::from(services.as_ref());
    let session_affinity =
        codex_proxy_rs::dispatch::affinity::RedisSessionAffinityStore::new(redis);
    session_affinity
        .upsert(
            "resp_lifecycle",
            &codex_proxy_rs::dispatch::affinity::SessionAffinityEntry {
                account_id: "acct_lifecycle".to_string(),
                conversation_id: "conv_lifecycle".to_string(),
                turn_state: None,
                instructions_hash: None,
                input_tokens: None,
                function_call_ids: Vec::new(),
                variant_hash: None,
                continuation_scope: codex_proxy_rs::upstream::openai::protocol::responses::PreviousResponseScope::ConnectionLocal,
                replay: None,
                created_at: chrono::Utc::now(),
            },
            chrono::Duration::hours(1),
        )
        .await
        .unwrap();
    let app = codex_proxy_rs::api::router::router().with_state(state.clone());

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

    let forbidden_metadata = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/update")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"id": "acct_lifecycle", "email": "updated-life@example.com"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden_metadata.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(forbidden_metadata).await["message"],
        "email is not editable"
    );

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
    assert_account_related_rows_deleted(&pool, "acct_lifecycle").await;
    assert!(session_affinity
        .get(
            "resp_lifecycle",
            chrono::Utc::now(),
            chrono::Duration::hours(1),
        )
        .await
        .unwrap()
        .is_none());
}

async fn seed_account_related_rows(pool: &PgPool, account_id: &str) {
    sqlx::query("insert into account_usage (account_id, request_count) values ($1, 1)")
        .bind(account_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into account_model_usage (account_id, model, request_count) values ($1, $2, 1)",
    )
    .bind(account_id)
    .bind("gpt-5.5")
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("insert into account_cookies (id, account_id, domain, name, value, updated_at) values ($1, $2, $3, $4, $5, $6)")
        .bind("cookie_lifecycle")
        .bind(account_id)
        .bind("chatgpt.com")
        .bind("cf_clearance")
        .bind("cipher")
        .bind(crate::support::storage::timestamp("2026-06-18T00:00:00Z"))
        .execute(pool)
        .await
        .unwrap();
}

async fn assert_account_related_rows_deleted(pool: &PgPool, account_id: &str) {
    let accounts: i64 = sqlx::query_scalar("select count(*) from accounts where id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
        .unwrap();
    let usage: i64 = sqlx::query_scalar("select count(*) from account_usage where account_id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
        .unwrap();
    let model_usage: i64 =
        sqlx::query_scalar("select count(*) from account_model_usage where account_id = $1")
            .bind(account_id)
            .fetch_one(pool)
            .await
            .unwrap();
    let cookies: i64 =
        sqlx::query_scalar("select count(*) from account_cookies where account_id = $1")
            .bind(account_id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(accounts, 0);
    assert_eq!(usage, 0);
    assert_eq!(model_usage, 0);
    assert_eq!(cookies, 0);
}

#[tokio::test]
async fn admin_account_status_update_should_update_proxy_account_pool() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-account-status-runtime-pool", 121).await;
    seed_account(
        &pool,
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
        .restore_from_store()
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
                    json!({"id": "acct_runtime_status", "status": "disabled"}).to_string(),
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
async fn admin_account_status_update_should_reject_non_manual_statuses() {
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-account-status-reject-banned", 125).await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_reject_status".to_string(),
            email: Some("reject-status@example.com".to_string()),
            account_id: Some("chatgpt-reject-status".to_string()),
            user_id: Some("user-reject-status".to_string()),
            label: None,
            plan_type: None,
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt-reject-status",
                    Some("user-reject-status"),
                    Some("reject-status@example.com"),
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

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/update")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"id": "acct_reject_status", "status": "banned"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let stored = PgAccountStore::new(pool)
        .get("acct_reject_status")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(stored.status, AccountStatus::Active);
}

#[tokio::test]
async fn admin_account_refresh_should_mark_account_expired_when_refresh_token_reused() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/oauth/token"))
        .respond_with(wiremock::ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "refresh_token_reused"
        })))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_oauth_token_endpoint(
        "admin-account-refresh-rt-reused",
        124,
        format!("{}/oauth/token", server.uri()),
    )
    .await;
    seed_account(
        &pool,
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
    let status = response.status();
    let body = response_json(response).await;
    let stored = PgAccountStore::new(pool)
        .get("acct_refresh_rt_reused")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["result"], "dead");
    assert_eq!(body["data"]["previousStatus"], "active");
    assert_eq!(stored.status, AccountStatus::Expired);
}

#[tokio::test]
async fn admin_account_refresh_should_preserve_quota_exhausted_status_on_success() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/oauth/token"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "access_token": test_jwt(
                "chatgpt-quota-refresh-success",
                Some("user-quota-refresh-success"),
                Some("quota-refresh-success@example.com"),
                Some("plus"),
            ),
            "refresh_token": "refresh-quota-success-rotated"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_oauth_token_endpoint(
        "admin-account-refresh-quota-preserved",
        124,
        format!("{}/oauth/token", server.uri()),
    )
    .await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_refresh_quota_success".to_string(),
            email: Some("quota-refresh-success@example.com".to_string()),
            account_id: Some("chatgpt-quota-refresh-success".to_string()),
            user_id: Some("user-quota-refresh-success".to_string()),
            label: None,
            plan_type: None,
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt-quota-refresh-success",
                    Some("user-quota-refresh-success"),
                    Some("quota-refresh-success@example.com"),
                    Some("plus"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new(
                "refresh-quota-success".to_string().into(),
            )),
            access_token_expires_at: None,
            status: AccountStatus::QuotaExhausted,
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
                    json!({"id": "acct_refresh_quota_success"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let stored = PgAccountStore::new(pool)
        .get("acct_refresh_quota_success")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(body["data"]["result"], "alive");
    assert_eq!(body["data"]["previousStatus"], "quota_exhausted");
    assert_eq!(stored.status, AccountStatus::QuotaExhausted);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "refresh-quota-success-rotated"
    );
}

#[tokio::test]
async fn admin_account_refresh_should_skip_disabled_account_without_consuming_refresh_token() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/oauth/token"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "access_token": test_jwt(
                "chatgpt-disabled-refresh",
                Some("user-disabled-refresh"),
                Some("disabled-refresh@example.com"),
                Some("plus"),
            ),
            "refresh_token": "refresh-disabled-rotated"
        })))
        .expect(0)
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_oauth_token_endpoint(
        "admin-account-refresh-disabled-skip",
        124,
        format!("{}/oauth/token", server.uri()),
    )
    .await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_refresh_disabled".to_string(),
            email: Some("disabled-refresh@example.com".to_string()),
            account_id: Some("chatgpt-disabled-refresh".to_string()),
            user_id: Some("user-disabled-refresh".to_string()),
            label: None,
            plan_type: None,
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt-disabled-refresh",
                    Some("user-disabled-refresh"),
                    Some("disabled-refresh@example.com"),
                    Some("plus"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new("refresh-disabled".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Disabled,
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
                    json!({"id": "acct_refresh_disabled"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let stored = PgAccountStore::new(pool)
        .get("acct_refresh_disabled")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["result"], "skipped");
    assert_eq!(body["data"]["error"], "manually disabled");
    assert_eq!(stored.status, AccountStatus::Disabled);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "refresh-disabled"
    );
}

#[tokio::test]
async fn admin_account_refresh_should_skip_expired_account_without_consuming_refresh_token() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/oauth/token"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "access_token": test_jwt(
                "chatgpt-expired-refresh",
                Some("user-expired-refresh"),
                Some("expired-refresh@example.com"),
                Some("plus"),
            ),
            "refresh_token": "refresh-expired-rotated"
        })))
        .expect(0)
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_oauth_token_endpoint(
        "admin-account-refresh-expired-skip",
        124,
        format!("{}/oauth/token", server.uri()),
    )
    .await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_refresh_expired".to_string(),
            email: Some("expired-refresh@example.com".to_string()),
            account_id: Some("chatgpt-expired-refresh".to_string()),
            user_id: Some("user-expired-refresh".to_string()),
            label: None,
            plan_type: None,
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt-expired-refresh",
                    Some("user-expired-refresh"),
                    Some("expired-refresh@example.com"),
                    Some("plus"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new("refresh-expired".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Expired,
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
                    json!({"id": "acct_refresh_expired"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let stored = PgAccountStore::new(pool)
        .get("acct_refresh_expired")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["result"], "skipped");
    assert_eq!(body["data"]["error"], "account expired");
    assert_eq!(stored.status, AccountStatus::Expired);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "refresh-expired"
    );
}

#[tokio::test]
async fn admin_account_refresh_should_skip_account_without_refresh_token() {
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-account-refresh-no-rt-skip", 124).await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_refresh_no_rt".to_string(),
            email: Some("no-rt-refresh@example.com".to_string()),
            account_id: Some("chatgpt-no-rt-refresh".to_string()),
            user_id: Some("user-no-rt-refresh".to_string()),
            label: None,
            plan_type: None,
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt-no-rt-refresh",
                    Some("user-no-rt-refresh"),
                    Some("no-rt-refresh@example.com"),
                    Some("plus"),
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

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/refresh")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({"id": "acct_refresh_no_rt"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let stored = PgAccountStore::new(pool)
        .get("acct_refresh_no_rt")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["result"], "skipped");
    assert_eq!(body["data"]["error"], "no refresh token");
    assert_eq!(stored.status, AccountStatus::Active);
}

#[tokio::test]
async fn admin_account_refresh_should_not_change_status_for_non_permanent_refresh_error() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/oauth/token"))
        .respond_with(wiremock::ResponseTemplate::new(400).set_body_string("quota exceeded"))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_oauth_token_endpoint(
        "admin-account-refresh-quota-temporary",
        124,
        format!("{}/oauth/token", server.uri()),
    )
    .await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_refresh_quota".to_string(),
            email: Some("quota-refresh@example.com".to_string()),
            account_id: Some("chatgpt-quota-refresh".to_string()),
            user_id: Some("user-quota-refresh".to_string()),
            label: None,
            plan_type: None,
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt-quota-refresh",
                    Some("user-quota-refresh"),
                    Some("quota-refresh@example.com"),
                    Some("plus"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new("refresh-quota".to_string().into())),
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
                .body(Body::from(json!({"id": "acct_refresh_quota"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let stored = PgAccountStore::new(pool)
        .get("acct_refresh_quota")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["result"], "dead");
    assert_eq!(stored.status, AccountStatus::Active);
}

#[tokio::test]
async fn admin_accounts_health_check_should_return_alive_dead_and_skipped_summary() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/oauth/token"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "access_token": test_jwt(
                "chatgpt-health-alive",
                Some("user-health-alive"),
                Some("health-alive@example.com"),
                Some("plus"),
            ),
            "refresh_token": "refresh-health-alive-rotated"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_oauth_token_endpoint(
        "admin-accounts-health-check",
        124,
        format!("{}/oauth/token", server.uri()),
    )
    .await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_health_alive".to_string(),
            email: Some("health-alive@example.com".to_string()),
            account_id: Some("chatgpt-health-alive".to_string()),
            user_id: Some("user-health-alive".to_string()),
            label: None,
            plan_type: None,
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt-health-alive",
                    Some("user-health-alive"),
                    Some("health-alive@example.com"),
                    Some("plus"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new("refresh-health-alive".to_string().into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_health_no_rt".to_string(),
            email: Some("health-no-rt@example.com".to_string()),
            account_id: Some("chatgpt-health-no-rt".to_string()),
            user_id: Some("user-health-no-rt".to_string()),
            label: None,
            plan_type: None,
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt-health-no-rt",
                    Some("user-health-no-rt"),
                    Some("health-no-rt@example.com"),
                    Some("plus"),
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
    seed_account(
        &pool,
        NewAccount {
            id: "acct_health_disabled".to_string(),
            email: Some("health-disabled@example.com".to_string()),
            account_id: Some("chatgpt-health-disabled".to_string()),
            user_id: Some("user-health-disabled".to_string()),
            label: None,
            plan_type: None,
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt-health-disabled",
                    Some("user-health-disabled"),
                    Some("health-disabled@example.com"),
                    Some("plus"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new(
                "refresh-health-disabled".to_string().into(),
            )),
            access_token_expires_at: None,
            status: AccountStatus::Disabled,
            added_at: None,
        },
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/health-check")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({"stagger_ms": 0}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let stored_alive = PgAccountStore::new(pool)
        .get("acct_health_alive")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["summary"]["total"], 3);
    assert_eq!(body["data"]["summary"]["alive"], 1);
    assert_eq!(body["data"]["summary"]["dead"], 0);
    assert_eq!(body["data"]["summary"]["skipped"], 2);
    assert_eq!(stored_alive.status, AccountStatus::Active);
    assert_eq!(
        stored_alive.refresh_token.unwrap().expose_secret(),
        "refresh-health-alive-rotated"
    );
}

#[tokio::test]
async fn admin_account_create_should_derive_claims_and_store_plain_tokens() {
    let (pool, _dir) = init_test_db("admin-account-create").await;
    let redis = create_test_redis("admin-account-create").await;
    seed_admin_session(&pool, &redis, "session_1").await;
    let token = test_jwt(
        "jwt-account",
        Some("jwt-user"),
        Some("jwt@example.com"),
        Some("team"),
    );
    let config = test_config(test_database_url());
    let stores = background_task_stores(pool.clone(), redis);
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = Arc::new(Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let state = AppState::from(services.as_ref());
    let app = codex_proxy_rs::api::router::router().with_state(state.clone());

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
    let (pool, _dir) = init_test_db("admin-account-create-invalid").await;
    let redis = create_test_redis("admin-account-create-invalid").await;
    seed_admin_session(&pool, &redis, "session_1").await;
    let config = test_config(test_database_url());
    let stores = background_task_stores(pool.clone(), redis);
    let fingerprint = crate::support::fingerprint::test_fingerprint();
    let services = Arc::new(Services::new(
        &config,
        stores,
        runtime_fingerprint(fingerprint),
    ));
    let state = AppState::from(services.as_ref());
    let app = codex_proxy_rs::api::router::router().with_state(state);

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
async fn admin_account_manual_create_from_refresh_token_should_not_store_consumed_refresh_token_when_not_rotated(
) {
    let server = wiremock::MockServer::start().await;
    let access_token = test_jwt(
        "rt-create-account",
        Some("rt-create-user"),
        Some("rt-create@example.com"),
        Some("plus"),
    );
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/oauth/token"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
            "access_token": access_token,
        })))
        .mount(&server)
        .await;
    let (app, _state, pool, _dir) = admin_accounts_test_app_with_oauth_token_endpoint(
        "admin-account-create-rt-no-rotation",
        126,
        format!("{}/oauth/token", server.uri()),
    )
    .await;

    let response = post_admin_account(&app, json!({"refreshToken": "rt-create-source"})).await;
    let status = response.status();
    let body = response_json(response).await;
    let stored = PgAccountStore::new(pool)
        .get(body["data"]["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert!(stored.refresh_token.is_none());
    assert!(stored.next_refresh_at.is_none());
}

#[tokio::test]
async fn admin_account_manual_create_should_accept_current_openai_token_without_chatgpt_account_id()
{
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-account-create-current-openai-token", 119).await;
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
    let stored = PgAccountStore::new(pool)
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
