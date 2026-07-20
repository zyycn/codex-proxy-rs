use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use gateway_api::admin::settings::{self, UpdateRuntimeSettingsRequest};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{AdminTestFixture, AdminTestState};

fn app(state: AdminTestState) -> Router {
    settings::router::<AdminTestState>().with_state(state)
}

fn request(method: Method, path: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::COOKIE, "cpr_admin_session=valid-session")
        .header("x-request-id", "req_admin_settings");
    let body = if let Some(value) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        Body::from(value.to_string())
    } else {
        Body::empty()
    };
    builder.body(body).expect("build settings request")
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read response body");
    serde_json::from_slice(&bytes).expect("parse response JSON")
}

fn update_body() -> Value {
    json!({
        "expectedConfigRevision": 7,
        "providerModelMappings": {
            "openai": { "gpt-5.4": "gpt-5.5" },
            "xai": { "grok-latest": "grok-4.5" }
        },
        "refreshMarginSeconds": 1800,
        "refreshConcurrency": 4,
        "maxConcurrentPerAccount": 5,
        "requestIntervalMs": 25,
        "rotationStrategy": "round_robin",
        "usageRetentionDays": 32,
        "opsEventRetentionDays": 31,
        "auditRetentionDays": 91
    })
}

#[test]
fn settings_request_should_reject_unknown_rotation_strategy() {
    let mut body = update_body();
    body["rotationStrategy"] = json!("random");
    let request: UpdateRuntimeSettingsRequest =
        serde_json::from_value(body).expect("decode settings");

    assert_eq!(request.validate().unwrap_err().field(), "rotationStrategy");
}

#[test]
fn settings_request_should_require_positive_revision() {
    let mut body = update_body();
    body["expectedConfigRevision"] = json!(0);
    let request: UpdateRuntimeSettingsRequest =
        serde_json::from_value(body).expect("decode settings");
    assert_eq!(
        request.validate().unwrap_err().field(),
        "expectedConfigRevision"
    );

    let mut missing = update_body();
    missing
        .as_object_mut()
        .expect("settings object")
        .remove("expectedConfigRevision");
    assert!(serde_json::from_value::<UpdateRuntimeSettingsRequest>(missing).is_err());
}

#[test]
fn settings_request_should_reject_removed_bucket_retention() {
    let mut body = update_body();
    body["bucketRetentionDays"] = json!(365);

    assert!(serde_json::from_value::<UpdateRuntimeSettingsRequest>(body).is_err());
}

#[tokio::test]
async fn settings_get_should_preserve_provider_scoped_model_mappings() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let response = app(fixture.state())
        .oneshot(request(Method::GET, "/api/admin/settings", None))
        .await
        .expect("settings response");
    let data = response_json(response).await["data"].clone();

    assert_eq!(
        (
            data["providerModelMappings"]["openai"]["coding-default"].as_str(),
            data["providerModelMappings"]["xai"]["grok-latest"].as_str(),
            data["rotationStrategy"].as_str()
        ),
        (Some("gpt-5.4"), Some("grok-4.5"), Some("smart"))
    );
}

#[tokio::test]
async fn settings_post_should_replace_provider_scoped_model_mappings() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let response = app(fixture.state())
        .oneshot(request(
            Method::POST,
            "/api/admin/settings",
            Some(update_body()),
        ))
        .await
        .expect("settings update response");
    let data = response_json(response).await["data"].clone();

    assert_eq!(data["configRevision"], 8);
    assert_eq!(
        data["providerModelMappings"]["openai"]["gpt-5.4"],
        "gpt-5.5"
    );
    assert_eq!(
        data["providerModelMappings"]["xai"]["grok-latest"],
        "grok-4.5"
    );
}

#[test]
fn settings_request_should_reject_invalid_provider_mapping_slug() {
    let mut body = update_body();
    body["providerModelMappings"] = json!({
        "OpenAI": { "gpt-5.4": "gpt-5.5" }
    });
    let request: UpdateRuntimeSettingsRequest =
        serde_json::from_value(body).expect("decode settings");

    assert_eq!(
        request.validate().unwrap_err().field(),
        "providerModelMappings"
    );
}

#[tokio::test]
async fn settings_should_require_admin_auth() {
    let fixture = AdminTestFixture::new().await;
    let response = app(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/settings")
                .header("x-request-id", "req_unauthorized")
                .body(Body::empty())
                .expect("unauthorized request"),
        )
        .await
        .expect("unauthorized response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_key_should_return_secret_only_on_regenerate() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let response = app(fixture.state())
        .oneshot(request(
            Method::POST,
            "/api/admin/settings/admin-api-key/regenerate",
            None,
        ))
        .await
        .expect("regenerate response");
    let data = response_json(response).await["data"].clone();

    assert!(
        data["key"]
            .as_str()
            .is_some_and(|key| key.starts_with("admin-") && key.len() == 70)
    );
}

#[tokio::test]
async fn admin_key_delete_should_use_fixed_post_path() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    fixture.settings.set_api_key("admin-valid-test-key");
    let response = app(fixture.state())
        .oneshot(request(
            Method::POST,
            "/api/admin/settings/admin-api-key",
            None,
        ))
        .await
        .expect("delete response");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn settings_should_accept_admin_api_key_header() {
    let fixture = AdminTestFixture::new().await;
    let key = format!("admin-{}", "a".repeat(64));
    fixture.auth.set_api_key(&key);
    let response = app(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/settings")
                .header("x-api-key", key)
                .header("x-request-id", "req_api_key")
                .body(Body::empty())
                .expect("api key request"),
        )
        .await
        .expect("api key response");

    assert_eq!(response.status(), StatusCode::OK);
}
