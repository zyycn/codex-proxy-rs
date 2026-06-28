use super::*;

#[tokio::test]
async fn admin_accounts_export_should_return_cpr_payload_for_all_accounts() {
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
                .uri("/api/admin/accounts/export")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let accounts = body["data"]["accounts"].as_array().unwrap();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["sourceFormat"], "cpr");
    assert_eq!(accounts.len(), 2);
    assert!(accounts.iter().any(|account| {
        account["id"] == "acct_export_a"
            && account["token"]
                .as_str()
                .is_some_and(|token| !token.is_empty())
            && account["refreshToken"] == "refresh-export-a"
    }));
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
                .uri("/api/admin/accounts/export?ids=acct_export_selected")
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
                .uri("/api/admin/accounts/export?ids=missing_account")
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
                    .uri("/api/admin/accounts/export?ids=acct_export_roundtrip")
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
