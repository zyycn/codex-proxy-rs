use super::*;

const EXPORT_CONFIRM: &str = "confirm=export_sensitive_accounts";

#[tokio::test]
async fn admin_accounts_export_should_reject_empty_ids() {
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-accounts-export-all.sqlite", 131).await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_export_a".into(),
            email: Some("export-a@example.com".into()),
            account_id: Some("chatgpt_export_a".into()),
            user_id: Some("user_export_a".into()),
            label: Some("primary".into()),
            plan_type: Some("team".into()),
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt_export_a",
                    Some("user_export_a"),
                    Some("export-a@example.com"),
                    Some("team"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new("refresh-export-a".into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_export_b".into(),
            email: Some("export-b@example.com".into()),
            account_id: Some("chatgpt_export_b".into()),
            user_id: Some("user_export_b".into()),
            label: Some("backup".into()),
            plan_type: Some("plus".into()),
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt_export_b",
                    Some("user_export_b"),
                    Some("export-b@example.com"),
                    Some("plus"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new("refresh-export-b".into())),
            access_token_expires_at: None,
            status: AccountStatus::Disabled,
            added_at: None,
        },
    )
    .await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/admin/accounts/export?{EXPORT_CONFIRM}"))
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], 40001);
    assert_eq!(body["message"], "account ids are required");
}

#[tokio::test]
async fn admin_accounts_export_should_filter_by_ids() {
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-accounts-export-filter.sqlite", 132).await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_export_selected".into(),
            email: Some("selected@example.com".into()),
            account_id: Some("chatgpt_export_selected".into()),
            user_id: Some("user_export_selected".into()),
            label: None,
            plan_type: Some("pro".into()),
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt_export_selected",
                    Some("user_export_selected"),
                    Some("selected@example.com"),
                    Some("pro"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new("refresh-selected".into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_export_ignored".into(),
            email: Some("ignored@example.com".into()),
            account_id: Some("chatgpt_export_ignored".into()),
            user_id: Some("user_export_ignored".into()),
            label: None,
            plan_type: Some("free".into()),
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt_export_ignored",
                    Some("user_export_ignored"),
                    Some("ignored@example.com"),
                    Some("free"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new("refresh-ignored".into())),
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
                .uri(format!(
                    "/api/admin/accounts/export?ids=acct_export_selected&{EXPORT_CONFIRM}"
                ))
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let accounts = body["data"]["accounts"].as_array().unwrap();

    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["id"], "acct_export_selected");
}

#[tokio::test]
async fn admin_accounts_export_should_fail_when_selected_id_is_missing() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-accounts-export-missing.sqlite", 133).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/api/admin/accounts/export?ids=missing_account&{EXPORT_CONFIRM}"
                ))
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_accounts_export_payload_should_import_directly() {
    let (source_app, _source_state, source_pool, _source_dir) =
        admin_accounts_test_app("admin-accounts-export-source.sqlite", 134).await;
    seed_account(
        &source_pool,
        NewAccount {
            id: "acct_export_roundtrip".into(),
            email: Some("roundtrip@example.com".into()),
            account_id: Some("chatgpt_export_roundtrip".into()),
            user_id: Some("user_export_roundtrip".into()),
            label: Some("roundtrip".into()),
            plan_type: Some("team".into()),
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt_export_roundtrip",
                    Some("user_export_roundtrip"),
                    Some("roundtrip@example.com"),
                    Some("team"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new("refresh-roundtrip".into())),
            access_token_expires_at: None,
            status: AccountStatus::Active,
            added_at: None,
        },
    )
    .await;
    let exported = response_json(
        source_app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/api/admin/accounts/export?ids=acct_export_roundtrip&{EXPORT_CONFIRM}"
                    ))
                    .header("cookie", "cpr_admin_session=session_1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    let payload = exported["data"].clone();

    let (target_app, _target_state, target_pool, _target_dir) =
        admin_accounts_test_app("admin-accounts-export-target.sqlite", 135).await;
    let response = target_app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/import")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let stored = SqliteAccountStore::new(target_pool)
        .get("acct_export_roundtrip")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(
        stored.refresh_token.unwrap().expose_secret(),
        "refresh-roundtrip"
    );
}

#[tokio::test]
async fn admin_accounts_export_should_require_explicit_sensitive_export_confirmation() {
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-accounts-export-confirm.sqlite", 136).await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_export_confirm".into(),
            email: Some("confirm@example.com".into()),
            account_id: Some("chatgpt_export_confirm".into()),
            user_id: Some("user_export_confirm".into()),
            label: None,
            plan_type: Some("pro".into()),
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt_export_confirm",
                    Some("user_export_confirm"),
                    Some("confirm@example.com"),
                    Some("pro"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new("refresh-confirm".into())),
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
                .uri("/api/admin/accounts/export?ids=acct_export_confirm")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_export_missing_confirmation")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["message"],
        "account export requires confirm=export_sensitive_accounts"
    );
    assert_eq!(
        usage_record_count_by_request_id(&pool, "req_export_missing_confirmation").await,
        0
    );
}

#[tokio::test]
async fn admin_accounts_export_should_not_write_usage_record_on_success() {
    let (app, _state, pool, _dir) =
        admin_accounts_test_app("admin-accounts-export-usage.sqlite", 137).await;
    seed_account(
        &pool,
        NewAccount {
            id: "acct_export_audit".into(),
            email: Some("audit@example.com".into()),
            account_id: Some("chatgpt_export_audit".into()),
            user_id: Some("user_export_audit".into()),
            label: None,
            plan_type: Some("team".into()),
            access_token: SecretString::new(
                test_jwt(
                    "chatgpt_export_audit",
                    Some("user_export_audit"),
                    Some("audit@example.com"),
                    Some("team"),
                )
                .into(),
            ),
            refresh_token: Some(SecretString::new("refresh-audit".into())),
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
                .uri(format!(
                    "/api/admin/accounts/export?ids=acct_export_audit&{EXPORT_CONFIRM}"
                ))
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_export_success_audit")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    assert_eq!(
        usage_record_count_by_request_id(&pool, "req_export_success_audit").await,
        0
    );
}

async fn usage_record_count_by_request_id(pool: &SqlitePool, request_id: &str) -> i64 {
    sqlx::query_scalar("select count(*) from usage_records where request_id = ?")
        .bind(request_id)
        .fetch_one(pool)
        .await
        .unwrap()
}
