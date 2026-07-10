use super::*;

#[tokio::test]
async fn admin_client_keys_label_should_update_clear_and_validate_label() {
    let (app, _dir) = admin_client_key_test_app("admin-client-key-label").await;
    let (key_id, _key) = create_admin_client_key(&app, "label-key").await;

    let renamed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/keys/update")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"id": key_id, "label":"automation"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(renamed.status(), StatusCode::OK);
    assert_eq!(response_json(renamed).await["data"]["label"], "automation");

    let too_long = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/keys/update")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"id": key_id, "label": "x".repeat(65)}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(too_long.status(), StatusCode::BAD_REQUEST);

    let missing = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/keys/update")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"id": "missing", "label":"automation"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_client_keys_batch_delete_should_remove_found_keys_and_report_missing_ids() {
    let (app, _dir) = admin_client_key_test_app("admin-client-key-delete").await;
    let (key_a, api_key_a) = create_admin_client_key(&app, "batch-a").await;
    let (key_b, api_key_b) = create_admin_client_key(&app, "batch-b").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/keys/delete")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"ids": [key_a, "ghost", key_b]}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["data"]["deleted"], 2);
    assert_eq!(body["data"]["notFound"], json!(["ghost"]));

    for api_key in [api_key_a, api_key_b] {
        let rejected = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/models")
                    .header("authorization", format!("Bearer {api_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    }
}
