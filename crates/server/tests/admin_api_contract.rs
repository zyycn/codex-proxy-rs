use axum::{
    body::to_bytes,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use codex_proxy_platform::json::Page;
use codex_proxy_server::admin_api::{AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse};
use serde_json::{json, Value};

#[test]
fn admin_envelope_should_serialize_lower_camel_case_request_id() {
    let body = AdminEnvelope::ok(json!({ "id": "acct_1" }), "req_1");

    let value = serde_json::to_value(body).unwrap();

    assert_eq!(
        value,
        json!({
            "code": 200,
            "message": "OK",
            "data": { "id": "acct_1" },
            "requestId": "req_1"
        })
    );
}

#[test]
fn admin_page_envelope_should_expose_items_as_data_with_page_metadata() {
    let page = Page {
        items: vec![json!({ "id": "evt_1" })],
        next_cursor: Some("cursor_1".to_string()),
    };
    let body = AdminPageEnvelope::ok(page, 50, "req_1");

    let value = serde_json::to_value(body).unwrap();

    assert_eq!(
        value,
        json!({
            "code": 200,
            "message": "OK",
            "data": [{ "id": "evt_1" }],
            "page": {
                "limit": 50,
                "nextCursor": "cursor_1"
            },
            "requestId": "req_1"
        })
    );
}

#[test]
fn admin_response_should_keep_http_status_separate_from_body_code() {
    let body = AdminEnvelope::new(40101, "Admin login required", (), "req_1");
    let response: Response = AdminResponse::new(StatusCode::UNAUTHORIZED, body).into_response();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn admin_error_body_should_use_null_data() {
    let body = AdminEnvelope::new(40101, "Admin login required", (), "req_1");

    let value = serde_json::to_value(body).unwrap();

    assert_eq!(value["data"], Value::Null);
}

#[tokio::test]
async fn admin_error_into_response_should_use_admin_envelope() {
    let response: Response = AdminError::new(
        StatusCode::UNAUTHORIZED,
        40101,
        "Admin login required",
        "req_1",
    )
    .into_response();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        value,
        json!({
            "code": 40101,
            "message": "Admin login required",
            "data": null,
            "requestId": "req_1"
        })
    );
}
