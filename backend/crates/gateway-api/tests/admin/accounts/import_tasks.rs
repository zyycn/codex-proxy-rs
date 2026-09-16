use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use gateway_api::admin;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::super::{AdminTestFixture, AdminTestState};

async fn send(
    fixture: &AdminTestFixture,
    method: &str,
    uri: &str,
    body: Value,
    authenticated: bool,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", "import-task-contract")
        .header(header::CONTENT_TYPE, "application/json");
    if authenticated {
        request = request.header(header::COOKIE, "cpr_session=valid-session");
    }
    let response = admin::router::<AdminTestState>()
        .with_state(fixture.state())
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(!value.to_string().contains("synthetic-sensitive-token"));
    (status, value)
}

fn input() -> Value {
    json!({"submissionId": "01994fcb-a333-7333-8000-000000000001", "items": [{"provider": "openai", "data": {"refreshToken": "synthetic-sensitive-token"}}]})
}

#[tokio::test]
async fn import_tasks_should_accept_list_restore_stop_and_deduplicate_without_echoing_credentials()
{
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let (status, accepted) = send(
        &fixture,
        "POST",
        "/api/admin/accounts/import-tasks",
        input(),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = accepted["data"]["taskId"].as_str().unwrap();
    let (_, retried) = send(
        &fixture,
        "POST",
        "/api/admin/accounts/import-tasks",
        input(),
        true,
    )
    .await;
    assert_eq!(retried["data"]["taskId"], id);
    let (_, list) = send(
        &fixture,
        "GET",
        "/api/admin/accounts/import-tasks",
        Value::Null,
        true,
    )
    .await;
    assert_eq!(list["data"]["items"].as_array().unwrap().len(), 1);
    let (_, detail) = send(
        &fixture,
        "GET",
        &format!("/api/admin/accounts/import-tasks/detail?taskId={id}"),
        Value::Null,
        true,
    )
    .await;
    assert_eq!(detail["data"]["items"][0]["status"], "pending");
    let (_, stopped) = send(
        &fixture,
        "POST",
        "/api/admin/accounts/import-tasks/stop",
        json!({"taskId": id}),
        true,
    )
    .await;
    assert_eq!(stopped["data"]["counts"]["skipped"], 1);
    assert!(stopped["data"]["finishedAt"].is_string());
    let mut changed = input();
    changed["items"][0]["data"]["refreshToken"] = json!("changed-synthetic-input");
    assert_eq!(
        send(
            &fixture,
            "POST",
            "/api/admin/accounts/import-tasks",
            changed,
            true
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn import_tasks_should_require_admin_and_enforce_input_bounds() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    for (method, uri, body) in [
        ("POST", "/api/admin/accounts/import-tasks", input()),
        ("GET", "/api/admin/accounts/import-tasks", Value::Null),
        (
            "GET",
            "/api/admin/accounts/import-tasks/detail?taskId=01994fcb-a333-7333-8000-000000000001",
            Value::Null,
        ),
        (
            "POST",
            "/api/admin/accounts/import-tasks/stop",
            json!({"taskId": "01994fcb-a333-7333-8000-000000000001"}),
        ),
    ] {
        assert_eq!(
            send(&fixture, method, uri, body, false).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    for count in [0, 201] {
        let mut body = input();
        body["items"] = json!(vec![body["items"][0].clone(); count]);
        assert_eq!(
            send(
                &fixture,
                "POST",
                "/api/admin/accounts/import-tasks",
                body,
                true
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut body = input();
    body["items"][0]["data"]["refreshToken"] = json!("x".repeat(4 * 1024 * 1024));
    assert_eq!(
        send(
            &fixture,
            "POST",
            "/api/admin/accounts/import-tasks",
            body,
            true
        )
        .await
        .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}
