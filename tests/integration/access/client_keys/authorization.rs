use super::*;

#[tokio::test]
async fn admin_client_keys_route_should_create_list_and_authorize_v1_requests() {
    let (app, _dir) = admin_client_key_test_app("admin-client-key-create.sqlite").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/api-keys")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_api_key")
                .body(Body::from(r#"{"name":"cursor"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let create_status = create_response.status();
    let create_body = response_json(create_response).await;

    assert_eq!(create_status, StatusCode::OK);
    assert!(create_body["data"]["plaintext"]
        .as_str()
        .is_some_and(|plaintext| plaintext.starts_with("cpr_")));
    assert_eq!(create_body["requestId"], "req_api_key");

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/api-keys?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_status = list_response.status();
    let list_body = response_json(list_response).await;

    assert_eq!(list_status, StatusCode::OK);
    assert_eq!(list_body["data"][0]["name"], "cursor");
    assert!(list_body["data"][0].get("plaintext").is_none());
    assert!(list_body["data"][0].get("keyHash").is_none());
}

#[tokio::test]
async fn admin_client_keys_status_should_disable_and_enable_authorization() {
    let (app, _dir) = admin_client_key_test_app("admin-client-key-status.sqlite").await;
    let (key_id, _plaintext) = create_admin_client_key(&app, "status-key").await;

    let disabled = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/api-keys/{key_id}/status"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"status":"disabled"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let disabled_status = disabled.status();
    let disabled_body = response_json(disabled).await;

    assert_eq!(disabled_status, StatusCode::OK);
    assert_eq!(disabled_body["data"]["enabled"], false);

    let enabled = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/api-keys/{key_id}/status"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"status":"active"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let enabled_status = enabled.status();
    let enabled_body = response_json(enabled).await;

    assert_eq!(enabled_status, StatusCode::OK);
    assert_eq!(enabled_body["data"]["enabled"], true);

    let invalid = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/admin/api-keys/{key_id}/status"))
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(r#"{"status":"expired"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_client_keys_delete_should_remove_authorization() {
    let (app, _dir) = admin_client_key_test_app("admin-client-key-delete.sqlite").await;
    let (key_id, _plaintext) = create_admin_client_key(&app, "delete-key").await;

    let deleted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/admin/api-keys/{key_id}"))
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let delete_status = deleted.status();
    let delete_body = response_json(deleted).await;

    assert_eq!(delete_status, StatusCode::OK);
    assert_eq!(delete_body["data"]["deleted"], true);

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/api-keys?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = response_json(list_response).await;
    assert_eq!(list_body["data"].as_array().unwrap().len(), 0);

    let missing = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/admin/api-keys/{key_id}"))
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}
