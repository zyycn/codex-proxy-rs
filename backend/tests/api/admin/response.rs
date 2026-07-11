use axum::{
    body::to_bytes,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use codex_proxy_rs::api::admin::response::{
    AdminEnvelope, AdminError, AdminPageEnvelope, AdminResponse,
};
use codex_proxy_rs::infra::json::NumberedPage;
use serde_json::{json, Value};

#[test]
fn admin_envelope_should_not_duplicate_request_id_in_body() {
    let body = AdminEnvelope::ok(json!({ "id": "acct_1" }));

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
fn admin_page_envelope_should_expose_stable_page_metadata() {
    let page = NumberedPage {
        items: vec![json!({ "id": "acct_1" })],
        total: 21,
        page: 2,
        page_size: 10,
    };
    let body = AdminPageEnvelope::ok(page);

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
    let body = serde_json::json!({
        "code": 40101,
        "message": "Admin session required",
        "data": null,
    });
    let response: Response = AdminResponse::new(StatusCode::UNAUTHORIZED, body).into_response();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn admin_error_body_should_use_null_data() {
    let body = AdminEnvelope::ok(());

    let value = serde_json::to_value(body).unwrap();

    assert_eq!(value["data"], Value::Null);
}

#[tokio::test]
async fn admin_error_into_response_should_use_admin_envelope() {
    let response: Response = AdminError::admin_session_required().into_response();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        value,
        json!({
            "code": 40101,
            "message": "Admin session required",
            "data": null
        })
    );
}
