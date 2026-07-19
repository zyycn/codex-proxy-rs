use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use chrono::Utc;
use gateway_api::admin::{
    AdminRequestContext, AdminServiceError, AdminSessionResolver, AdminSessionState,
    RuntimeSettingsView, UpdateRuntimeSettingsRequest,
    settings::{self, AdminSettingsService, AdminSettingsState},
};
use serde_json::{Value, json};
use tower::ServiceExt;

struct FakeSettings {
    settings: Mutex<RuntimeSettingsView>,
    key_exists: Mutex<bool>,
}

impl Default for FakeSettings {
    fn default() -> Self {
        Self {
            settings: Mutex::new(runtime_settings()),
            key_exists: Mutex::new(false),
        }
    }
}

#[async_trait]
impl AdminSessionResolver for FakeSettings {
    async fn resolve_admin_user_id(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<String>, AdminServiceError> {
        Ok((session_id == Some("valid-session")).then(|| "admin_1".to_owned()))
    }

    async fn verify_admin_api_key(&self, key: &str) -> Result<bool, AdminServiceError> {
        Ok(key == "admin-valid-test-key")
    }
}

#[async_trait]
impl AdminSettingsService for FakeSettings {
    async fn load(&self) -> Result<RuntimeSettingsView, AdminServiceError> {
        Ok(self.settings.lock().expect("settings state").clone())
    }

    async fn replace(
        &self,
        context: &AdminRequestContext,
        request: UpdateRuntimeSettingsRequest,
    ) -> Result<RuntimeSettingsView, AdminServiceError> {
        assert_eq!(context.admin_user_id(), Some("admin_1"));
        let settings = RuntimeSettingsView {
            config_revision: request.expected_config_revision + 1,
            provider_model_mappings: request.provider_model_mappings,
            refresh_margin_seconds: request.refresh_margin_seconds,
            refresh_concurrency: request.refresh_concurrency,
            max_concurrent_per_account: request.max_concurrent_per_account,
            request_interval_ms: request.request_interval_ms,
            rotation_strategy: request.rotation_strategy,
            usage_retention_days: request.usage_retention_days,
            ops_event_retention_days: request.ops_event_retention_days,
            audit_retention_days: request.audit_retention_days,
            updated_at: Utc::now(),
        };
        *self.settings.lock().expect("settings state") = settings.clone();
        Ok(settings)
    }

    async fn admin_api_key_exists(&self) -> Result<bool, AdminServiceError> {
        Ok(*self.key_exists.lock().expect("key state"))
    }

    async fn regenerate_admin_api_key(
        &self,
        _context: &AdminRequestContext,
    ) -> Result<String, AdminServiceError> {
        *self.key_exists.lock().expect("key state") = true;
        Ok("admin-generated-secret-once".to_owned())
    }

    async fn delete_admin_api_key(
        &self,
        _context: &AdminRequestContext,
    ) -> Result<(), AdminServiceError> {
        *self.key_exists.lock().expect("key state") = false;
        Ok(())
    }
}

#[derive(Clone)]
struct TestState(Arc<FakeSettings>);

impl AdminSessionState for TestState {
    fn admin_session_resolver(&self) -> &dyn AdminSessionResolver {
        self.0.as_ref()
    }
}

impl AdminSettingsState for TestState {
    fn admin_settings_service(&self) -> &dyn AdminSettingsService {
        self.0.as_ref()
    }
}

fn app(settings: Arc<FakeSettings>) -> Router {
    settings::router::<TestState>().with_state(TestState(settings))
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

fn runtime_settings() -> RuntimeSettingsView {
    RuntimeSettingsView {
        config_revision: 7,
        provider_model_mappings: BTreeMap::from([
            (
                "openai".to_owned(),
                BTreeMap::from([("coding-default".to_owned(), "gpt-5.4".to_owned())]),
            ),
            (
                "xai".to_owned(),
                BTreeMap::from([("grok-latest".to_owned(), "grok-4.5".to_owned())]),
            ),
        ]),
        refresh_margin_seconds: 3600,
        refresh_concurrency: 2,
        max_concurrent_per_account: 3,
        request_interval_ms: 50,
        rotation_strategy: "smart".into(),
        usage_retention_days: 31,
        ops_event_retention_days: 30,
        audit_retention_days: 90,
        updated_at: Utc::now(),
    }
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
    let response = app(Arc::new(FakeSettings::default()))
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
    let response = app(Arc::new(FakeSettings::default()))
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
    let response = app(Arc::new(FakeSettings::default()))
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
    let response = app(Arc::new(FakeSettings::default()))
        .oneshot(request(
            Method::POST,
            "/api/admin/settings/admin-api-key/regenerate",
            None,
        ))
        .await
        .expect("regenerate response");
    let data = response_json(response).await["data"].clone();

    assert_eq!(data["key"], "admin-generated-secret-once");
}

#[tokio::test]
async fn admin_key_delete_should_use_fixed_post_path() {
    let settings = Arc::new(FakeSettings::default());
    *settings.key_exists.lock().expect("key state") = true;
    let response = app(settings)
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
    let response = app(Arc::new(FakeSettings::default()))
        .oneshot(
            Request::builder()
                .uri("/api/admin/settings")
                .header("x-api-key", "admin-valid-test-key")
                .header("x-request-id", "req_api_key")
                .body(Body::empty())
                .expect("api key request"),
        )
        .await
        .expect("api key response");

    assert_eq!(response.status(), StatusCode::OK);
}
