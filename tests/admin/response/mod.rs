use axum::{
    body::to_bytes,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use codex_proxy_rs::admin::response::{
    AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse,
};
use codex_proxy_rs::infra::json::{NumberedPage, Page};
use serde_json::{json, Value};

#[test]
fn admin_envelope_should_not_duplicate_request_id_in_body() {
    let body = AdminEnvelope::ok(json!({ "id": "acct_1" }), "req_1");

    let value = serde_json::to_value(body).unwrap();

    assert_eq!(
        value,
        json!({
            "code": 200,
            "message": "OK",
            "data": { "id": "acct_1" }
        })
    );
}

#[test]
fn admin_page_envelope_should_expose_items_and_page_metadata_inside_data() {
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
            "data": {
                "items": [{ "id": "evt_1" }],
                "page": {
                    "limit": 50,
                    "nextCursor": "cursor_1"
                }
            }
        })
    );
}

#[test]
fn admin_page_envelope_should_skip_empty_next_cursor() {
    let page = Page {
        items: vec![json!({ "id": "evt_1" })],
        next_cursor: None,
    };
    let body = AdminPageEnvelope::ok(page, 50, "req_1");

    let value = serde_json::to_value(body).unwrap();

    assert_eq!(
        value,
        json!({
            "code": 200,
            "message": "OK",
            "data": {
                "items": [{ "id": "evt_1" }],
                "page": {
                    "limit": 50
                }
            }
        })
    );
}

#[test]
fn admin_page_envelope_should_expose_numbered_page_metadata() {
    let page = NumberedPage {
        items: vec![json!({ "id": "acct_1" })],
        total: 21,
        page: 2,
        page_size: 10,
    };
    let body = AdminPageEnvelope::numbered(page, "req_1");

    let value = serde_json::to_value(body).unwrap();

    assert_eq!(
        value,
        json!({
            "code": 200,
            "message": "OK",
            "data": {
                "items": [{ "id": "acct_1" }],
                "page": {
                    "page": 2,
                    "pageSize": 10,
                    "total": 21,
                    "totalPages": 3
                }
            }
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
            "data": null
        })
    );
}
