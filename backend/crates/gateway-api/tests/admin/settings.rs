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
        "modelMappings": {
            "gpt-5.4": "gpt-5.5",
            "grok-latest": "grok-4.5"
        },
        "refreshMarginSeconds": 1800,
        "refreshConcurrency": 4,
        "maxConcurrentPerAccount": 5,
        "requestIntervalMs": 25,
        "rotationStrategy": "round_robin",
        "minCodexDesktopVersion": "26.825.6671",
        "minCodexCliVersion": "0.40.0",
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
fn settings_request_should_reject_non_semver_client_min() {
    let mut body = update_body();
    body["minCodexCliVersion"] = json!("v0.40.0");
    let request: UpdateRuntimeSettingsRequest =
        serde_json::from_value(body).expect("decode settings");

    assert_eq!(
        request.validate().unwrap_err().field(),
        "minCodexCliVersion"
    );
}

#[test]
fn settings_response_should_cover_the_full_runtime_settings_contract() {
    use std::collections::BTreeMap;

    use chrono::{TimeZone, Utc};
    use gateway_admin::model::Revision;
    use gateway_admin::model::settings::RuntimeSettings;
    use gateway_api::admin::settings::RuntimeSettingsView;
    use gateway_core::engine::credential::RotationStrategy;
    use gateway_core::routing::{PublicModelId, UpstreamModelId};

    let settings = RuntimeSettings {
        config_revision: Revision::new(7).expect("revision"),
        model_mappings: BTreeMap::from_iter([
            (
                PublicModelId::new("gpt-5.4").expect("public model"),
                UpstreamModelId::new("gpt-5.5").expect("upstream model"),
            ),
            (
                PublicModelId::new("grok-latest").expect("public model"),
                UpstreamModelId::new("grok-4.5").expect("upstream model"),
            ),
        ]),
        refresh_margin_seconds: 1800,
        refresh_concurrency: 4,
        max_concurrent_per_account: 5,
        request_interval_ms: 25,
        rotation_strategy: RotationStrategy::RoundRobin,
        min_codex_desktop_version: Some("26.825.6671".to_owned()),
        min_codex_cli_version: Some("0.40.0".to_owned()),
        usage_retention_days: 32,
        ops_event_retention_days: 31,
        audit_retention_days: 91,
        updated_at: Utc
            .with_ymd_and_hms(2026, 8, 2, 10, 30, 0)
            .single()
            .expect("timestamp"),
    };

    let value = serde_json::to_value(RuntimeSettingsView::from(settings)).expect("serialize view");
    assert_eq!(
        value,
        json!({
            "modelMappings": {
                "gpt-5.4": "gpt-5.5",
                "grok-latest": "grok-4.5"
            },
            "refreshMarginSeconds": 1800,
            "refreshConcurrency": 4,
            "maxConcurrentPerAccount": 5,
            "requestIntervalMs": 25,
            "rotationStrategy": "round_robin",
            "minCodexDesktopVersion": "26.825.6671",
            "minCodexCliVersion": "0.40.0",
            "usageRetentionDays": 32,
            "opsEventRetentionDays": 31,
            "auditRetentionDays": 91,
            "updatedAt": "2026-08-02T10:30:00Z"
        })
    );
}

#[test]
fn settings_request_and_response_fields_should_stay_in_lockstep() {
    use std::collections::{BTreeMap, BTreeSet};

    use gateway_admin::model::Revision;
    use gateway_admin::model::settings::RuntimeSettings;
    use gateway_api::admin::settings::RuntimeSettingsView;
    use gateway_core::engine::credential::RotationStrategy;
    use gateway_core::routing::{PublicModelId, UpstreamModelId};

    let request: UpdateRuntimeSettingsRequest =
        serde_json::from_value(update_body()).expect("decode settings");
    request.validate().expect("fixture settings must validate");

    let request_fields: BTreeSet<String> = update_body()
        .as_object()
        .expect("request body object")
        .keys()
        .cloned()
        .collect();
    let settings = RuntimeSettings {
        config_revision: Revision::new(7).expect("revision"),
        model_mappings: request
            .model_mappings
            .iter()
            .map(|(public, upstream)| {
                Ok((
                    PublicModelId::new(public.clone())?,
                    UpstreamModelId::new(upstream.clone())?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, gateway_core::error::IdentifierError>>()
            .expect("valid model mappings"),
        refresh_margin_seconds: request.refresh_margin_seconds,
        refresh_concurrency: u32::try_from(request.refresh_concurrency).expect("u32"),
        max_concurrent_per_account: u32::try_from(request.max_concurrent_per_account).expect("u32"),
        request_interval_ms: request.request_interval_ms,
        rotation_strategy: RotationStrategy::parse(&request.rotation_strategy)
            .expect("fixture rotation strategy"),
        min_codex_desktop_version: request.min_codex_desktop_version,
        min_codex_cli_version: request.min_codex_cli_version,
        usage_retention_days: u32::try_from(request.usage_retention_days).expect("u32"),
        ops_event_retention_days: u32::try_from(request.ops_event_retention_days).expect("u32"),
        audit_retention_days: u32::try_from(request.audit_retention_days).expect("u32"),
        updated_at: chrono::Utc::now(),
    };

    let response_fields: BTreeSet<String> =
        serde_json::to_value(RuntimeSettingsView::from(settings))
            .expect("serialize view")
            .as_object()
            .expect("view object")
            .keys()
            .cloned()
            .collect();
    let mut expected_fields = request_fields;
    expected_fields.insert("updatedAt".to_owned());

    assert_eq!(response_fields, expected_fields);
}

#[test]
fn settings_request_should_reject_unknown_revision_field() {
    let mut body = update_body();
    body["expectedConfigRevision"] = json!(7);

    assert!(serde_json::from_value::<UpdateRuntimeSettingsRequest>(body).is_err());
}

#[test]
fn settings_request_should_reject_removed_bucket_retention() {
    let mut body = update_body();
    body["bucketRetentionDays"] = json!(365);

    assert!(serde_json::from_value::<UpdateRuntimeSettingsRequest>(body).is_err());
}

#[tokio::test]
async fn settings_get_should_preserve_global_model_mappings() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let response = app(fixture.state())
        .oneshot(request(Method::GET, "/api/admin/settings", None))
        .await
        .expect("settings response");
    let data = response_json(response).await["data"].clone();

    assert_eq!(
        (
            data["modelMappings"]["coding-default"].as_str(),
            data["modelMappings"]["grok-latest"].as_str(),
            data["rotationStrategy"].as_str()
        ),
        (Some("gpt-5.4"), Some("grok-4.5"), Some("smart"))
    );
}

#[tokio::test]
async fn settings_post_should_replace_global_model_mappings() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let response = app(fixture.state())
        .oneshot(request(
            Method::POST,
            "/api/admin/settings/update",
            Some(update_body()),
        ))
        .await
        .expect("settings update response");
    let data = response_json(response).await["data"].clone();

    assert!(data.get("configRevision").is_none());
    assert_eq!(data["modelMappings"]["gpt-5.4"], "gpt-5.5");
    assert_eq!(data["modelMappings"]["grok-latest"], "grok-4.5");
}

#[tokio::test]
async fn client_downloads_should_return_validated_direct_links() {
    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let response = app(fixture.state())
        .oneshot(request(
            Method::GET,
            "/api/admin/settings/client-downloads/codex-desktop/windows?refresh=true",
            None,
        ))
        .await
        .expect("client downloads response");

    assert_eq!(response.status(), StatusCode::OK);
    let data = response_json(response).await["data"].clone();
    assert_eq!(data["packages"][0]["architecture"], "x64");
    assert_eq!(data["packages"][0]["source"], "microsoft_store");
    assert_eq!(data["packages"][0]["version"], "26.825.6671.0");
    assert!(
        data["packages"][0]["downloadUrl"]
            .as_str()
            .is_some_and(|url| url.starts_with("https://dl.delivery.mp.microsoft.com/"))
    );
}

#[test]
fn settings_request_should_reject_invalid_model_mapping_name() {
    let mut body = update_body();
    body["modelMappings"] = json!({ "\0": "gpt-5.5" });
    let request: UpdateRuntimeSettingsRequest =
        serde_json::from_value(body).expect("decode settings");

    assert_eq!(request.validate().unwrap_err().field(), "modelMappings");
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
            "/api/admin/settings/admin-api-key/delete",
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

#[tokio::test]
async fn admin_auth_should_accept_a_configured_request_id_header_name() {
    use axum::http::HeaderName;
    use tower_http::request_id::{MakeRequestUuid, SetRequestIdLayer};

    let fixture = AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    // 部署把 api.request_id_header 改名后，注入的 header 不再叫 x-request-id；
    // 管理请求仍须拿到请求上下文，而不是退化为 500。
    let custom = HeaderName::from_static("x-trace-id");
    let app = app(fixture.state()).layer(SetRequestIdLayer::new(custom, MakeRequestUuid));
    let unlabelled = Request::builder()
        .method(Method::GET)
        .uri("/api/admin/settings")
        .header(header::COOKIE, "cpr_admin_session=valid-session")
        .body(Body::empty())
        .expect("build settings request");

    let response = app.oneshot(unlabelled).await.expect("settings response");

    assert_eq!(response.status(), StatusCode::OK);
}
