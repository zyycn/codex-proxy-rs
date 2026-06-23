use super::*;

#[tokio::test]
async fn admin_client_keys_export_should_return_metadata_without_secret_material() {
    let (app, _dir) = admin_client_key_test_app("admin-client-key-export.sqlite").await;
    let (key_id, _plaintext) = create_admin_client_key(&app, "export-key").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/admin/keys/export?ids={key_id}"))
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_api_key_export")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["requestId"], "req_api_key_export");
    assert_eq!(body["data"]["apiKeys"][0]["id"], key_id);
    assert_eq!(body["data"]["apiKeys"][0]["name"], "export-key");
    assert!(body["data"]["apiKeys"][0].get("plaintext").is_none());
}
