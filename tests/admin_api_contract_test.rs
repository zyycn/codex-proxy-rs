use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};

use codex_proxy_rs::{
    http::admin::{AdminEnvelope, AdminPageEnvelope, AdminResponse},
    pagination::Page,
};

#[test]
fn admin_envelope_serializes_lower_camel_case_request_id() {
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
fn admin_page_envelope_exposes_items_as_data_with_page_metadata() {
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
fn admin_response_keeps_http_status_separate_from_body_code() {
    let body = AdminEnvelope::new(40101, "Admin login required", (), "req_1");
    let response: Response = AdminResponse::new(StatusCode::UNAUTHORIZED, body).into_response();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn admin_error_body_uses_null_data() {
    let body = AdminEnvelope::new(40101, "Admin login required", (), "req_1");

    let value = serde_json::to_value(body).unwrap();

    assert_eq!(value["data"], Value::Null);
}
