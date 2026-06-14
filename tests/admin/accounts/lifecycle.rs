use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tower::ServiceExt;

use crate::support::{
    admin_accounts::{admin_accounts_test_app, import_test_account},
    response_json,
};

#[tokio::test]
async fn admin_account_label_should_update_and_clear_label() {
    let (app, state, pool, _dir) = admin_accounts_test_app("admin-account-label.sqlite", 16).await;
    import_test_account(&app, "session_1", "acct_label").await;

    let set_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/accounts/acct_label/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_label")
                .body(Body::from(r#"{"label":"Team Alpha"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(set_response.status(), StatusCode::OK);
    let body = response_json(set_response).await;
    assert_eq!(body["data"]["label"], "Team Alpha");

    let clear_response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/accounts/acct_label/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":null}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(clear_response.status(), StatusCode::OK);
    let body = response_json(clear_response).await;
    assert!(body["data"]["label"].is_null());

    let stored: (Option<String>,) = sqlx::query_as("select label from accounts where id = ?")
        .bind("acct_label")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored.0, None);
    assert!(state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .is_some());
}

#[tokio::test]
async fn admin_account_label_should_reject_too_long_or_missing_account() {
    let (app, _state, _pool, _dir) =
        admin_accounts_test_app("admin-account-label-invalid.sqlite", 17).await;
    import_test_account(&app, "session_1", "acct_label_invalid").await;
    let long_label = "x".repeat(65);

    let too_long = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/accounts/acct_label_invalid/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({ "label": long_label }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(too_long.status(), StatusCode::BAD_REQUEST);

    let missing = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/accounts/missing/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":"Team Alpha"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_account_status_should_update_database_and_runtime_pool() {
    let (app, state, pool, _dir) = admin_accounts_test_app("admin-account-status.sqlite", 18).await;
    import_test_account(&app, "session_1", "acct_status").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/accounts/acct_status/status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_status")
                .body(Body::from(r#"{"status":"disabled"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["status"], "disabled");

    let stored: (String,) = sqlx::query_as("select status from accounts where id = ?")
        .bind("acct_status")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored.0, "disabled");
    assert!(state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .is_none());
}

#[tokio::test]
async fn admin_account_delete_should_remove_database_row_and_runtime_pool_entry() {
    let (app, state, pool, _dir) = admin_accounts_test_app("admin-account-delete.sqlite", 19).await;
    import_test_account(&app, "session_1", "acct_delete").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/accounts/acct_delete")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_delete")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["deleted"], true);

    let row_count: (i64,) = sqlx::query_as("select count(*) from accounts where id = ?")
        .bind("acct_delete")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row_count.0, 0);
    assert!(state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .is_none());

    let missing = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/admin/accounts/acct_delete")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_accounts_batch_delete_should_delete_found_accounts_and_report_missing_ids() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-accounts-batch-delete.sqlite", 20).await;
    import_test_account(&app, "session_1", "acct_batch_delete_a").await;
    import_test_account(&app, "session_1", "acct_batch_delete_b").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/batch-delete")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": ["acct_batch_delete_a", "ghost", "acct_batch_delete_b"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["deleted"], 2);
    assert_eq!(body["data"]["notFound"], json!(["ghost"]));

    let row_count: (i64,) = sqlx::query_as("select count(*) from accounts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row_count.0, 0);
    assert!(state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .is_none());

    let empty = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/batch-delete")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"ids":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_accounts_batch_status_should_update_found_accounts_and_reject_invalid_status() {
    let (app, state, pool, _dir) =
        admin_accounts_test_app("admin-accounts-batch-status.sqlite", 21).await;
    import_test_account(&app, "session_1", "acct_batch_status_a").await;
    import_test_account(&app, "session_1", "acct_batch_status_b").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/batch-status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": ["acct_batch_status_a", "ghost"],
                        "status": "disabled"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["updated"], 1);
    assert_eq!(body["data"]["notFound"], json!(["ghost"]));

    let statuses =
        sqlx::query_as::<_, (String, String)>("select id, status from accounts order by id asc")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        statuses,
        vec![
            ("acct_batch_status_a".to_string(), "disabled".to_string()),
            ("acct_batch_status_b".to_string(), "active".to_string())
        ]
    );
    let acquired = state
        .services
        .accounts
        .acquire_runtime_account("gpt-5.5")
        .await
        .unwrap();
    assert_eq!(acquired.id, "acct_batch_status_b");

    let invalid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/batch-status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": ["acct_batch_status_a"],
                        "status": "expired"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let empty = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/accounts/batch-status")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"ids":[],"status":"active"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
}
