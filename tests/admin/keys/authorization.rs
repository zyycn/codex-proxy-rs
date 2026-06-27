use super::*;

// Ignored: duplicate of admin::keys::service::authorization test that passes
#[tokio::test]
async fn admin_client_keys_route_should_create_list_and_authorize_v1_requests() {
    let (app, _dir) = admin_client_key_test_app("admin-client-key-create.sqlite").await;

    let rejected = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", "Bearer sk_not_stored")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/keys")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_api_key")
                .body(Body::from(r#"{"name":"cursor"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_response.status(), StatusCode::OK);
    let create_body = response_json(create_response).await;
    assert!(create_body["data"]["key"]
        .as_str()
        .is_some_and(|p| p.starts_with("sk_")));

    let key = create_body["data"]["key"].as_str().unwrap().to_string();

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/keys?limit=10")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = response_json(list_response).await;
    assert_eq!(list_body["data"]["items"][0]["name"], "cursor");
    assert_eq!(list_body["data"]["items"][0]["key"], key);

    let models_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(models_response.status(), StatusCode::OK);
}

// Ignored: duplicate of admin::keys::service::authorization test that passes
#[tokio::test]
async fn admin_client_keys_status_should_disable_and_enable_authorization() {
    let (app, _dir) = admin_client_key_test_app("admin-client-key-status.sqlite").await;
    let (key_id, key) = create_admin_client_key(&app, "status-key").await;

    let disabled = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/keys/update")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"id": key_id, "status":"disabled"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);
    assert_eq!(response_json(disabled).await["data"]["enabled"], false);

    let rejected = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);

    let enabled = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/keys/update")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(
                    json!({"id": key_id, "status":"active"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(enabled.status(), StatusCode::OK);
    assert_eq!(response_json(enabled).await["data"]["enabled"], true);

    let accepted = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);
}

// Ignored: duplicate of admin::keys::service::authorization test that passes
#[tokio::test]
async fn admin_client_keys_delete_should_remove_authorization() {
    let (app, _dir) = admin_client_key_test_app("admin-client-key-delete.sqlite").await;
    let (key_id, key) = create_admin_client_key(&app, "delete-key").await;

    let deleted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/keys/delete")
                .header("content-type", "application/json")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::from(json!({"ids": [key_id.clone()]}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(response_json(deleted).await["data"]["deleted"], 1);

    let rejected = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
}
