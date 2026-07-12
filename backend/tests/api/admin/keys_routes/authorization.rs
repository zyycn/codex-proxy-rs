use super::*;

#[tokio::test]
async fn admin_client_keys_route_should_create_list_and_authorize_v1_requests() {
    let (app, _pool, _dir) = admin_client_key_test_app("admin-client-key-create").await;

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
                .body(Body::from(r#"{"name":"primary"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_response.status(), StatusCode::OK);
    let create_body = response_json(create_response).await;
    assert!(
        create_body["data"]["key"]
            .as_str()
            .is_some_and(|p| p.starts_with("sk_"))
    );

    let key = create_body["data"]["key"].as_str().unwrap().to_string();

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/keys?page=1&pageSize=10")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list_body = response_json(list_response).await;
    assert_eq!(list_body["data"]["items"][0]["name"], "primary");
    assert_eq!(list_body["data"]["items"][0]["key"], key);
    assert_eq!(list_body["data"]["page"]["page"], 1);
    assert_eq!(list_body["data"]["page"]["pageSize"], 10);
    assert_eq!(list_body["data"]["page"]["total"], 1);
    assert!(key.starts_with(list_body["data"]["items"][0]["prefix"].as_str().unwrap()));

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

#[tokio::test]
async fn admin_client_keys_status_should_disable_and_enable_authorization() {
    let (app, _pool, _dir) = admin_client_key_test_app("admin-client-key-status").await;
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

#[tokio::test]
async fn admin_client_keys_delete_should_remove_authorization() {
    let (app, _pool, _dir) = admin_client_key_test_app("admin-client-key-delete").await;
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

#[tokio::test]
async fn admin_client_keys_list_should_page_and_search_the_complete_key_set() {
    let (app, pool, _dir) = admin_client_key_test_app("admin-client-key-pagination").await;
    sqlx::query(
        r"
insert into client_api_keys (id, name, label, prefix, key, enabled, created_at)
select
  'key_' || lpad(sequence::text, 3, '0'),
  'name-' || sequence,
  case when sequence = 7 then 'nightly-needle' end,
  'sk_test_' || sequence,
  'sk_test_key_' || sequence,
  true,
  timestamptz '2026-07-01T00:00:00Z' + sequence * interval '1 second'
from generate_series(1, 55) as seeded(sequence)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let page_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/keys?page=3&pageSize=20")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let page_body = response_json(page_response).await;
    assert_eq!(page_body["data"]["page"]["total"], 55);
    assert_eq!(page_body["data"]["page"]["totalPages"], 3);
    assert_eq!(page_body["data"]["items"].as_array().unwrap().len(), 15);
    assert_eq!(page_body["data"]["items"][0]["id"], "key_015");

    let search_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/keys?page=1&pageSize=20&search=nightly-needle")
                .header("cookie", "cpr_admin_session=session_1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let search_body = response_json(search_response).await;
    assert_eq!(search_body["data"]["page"]["total"], 1);
    assert_eq!(search_body["data"]["items"][0]["id"], "key_007");
}
