use super::*;

#[tokio::test]
async fn admin_client_keys_label_should_update_clear_and_validate_label() {
    let (app, _dir) = admin_client_key_test_app("admin-client-key-label.sqlite").await;
    let (key_id, _plaintext) = create_admin_client_key(&app, "label-key").await;

    let renamed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/api-keys/{key_id}/label"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":"automation"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let renamed_status = renamed.status();
    let renamed_body = response_json(renamed).await;

    assert_eq!(renamed_status, StatusCode::OK);
    assert_eq!(renamed_body["data"]["name"], "label-key");
    assert_eq!(renamed_body["data"]["label"], "automation");

    let cleared = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/api-keys/{key_id}/label"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":null}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let cleared_status = cleared.status();
    let cleared_body = response_json(cleared).await;

    assert_eq!(cleared_status, StatusCode::OK);
    assert_eq!(cleared_body["data"]["name"], "label-key");
    assert!(cleared_body["data"]["label"].is_null());

    let too_long = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/api-keys/{key_id}/label"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({ "label": "x".repeat(65) }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(too_long.status(), StatusCode::BAD_REQUEST);

    let missing = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/admin/api-keys/missing/label")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"label":"automation"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_client_keys_batch_delete_should_remove_found_keys_and_report_missing_ids() {
    let (app, _dir) = admin_client_key_test_app("admin-client-key-batch-delete.sqlite").await;
    let (key_a, _plaintext_a) = create_admin_client_key(&app, "batch-a").await;
    let (key_b, _plaintext_b) = create_admin_client_key(&app, "batch-b").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/api-keys/batch-delete")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({
                        "ids": [key_a, "ghost", key_b]
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
    assert_eq!(body["data"]["deleted"], 2);
    assert_eq!(body["data"]["notFound"], json!(["ghost"]));

    let empty = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/api-keys/batch-delete")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"ids":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
}
