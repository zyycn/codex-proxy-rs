//! 验证管理端来源经用例和 HTTP 出口到达受控记录，公开响应和扩展不泄漏

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn store_failure_is_recorded_once_with_the_incoming_request_id() {
    let fixture = super::AdminTestFixture::new().await;
    fixture.auth.insert_session("valid-session");
    let diagnostics = Arc::new(crate::support::RecordingDiagnostics::default());
    let bundle = crate::data_plane::api_bundle_with_diagnostics(
        fixture.services,
        gateway_api::ApiConfig {
            asset_directory: std::env::temp_dir(),
            request_timeout_seconds: None,
            cors_allowed_origins: Vec::new(),
            request_id_header: "x-request-id".to_owned(),
        },
        diagnostics.clone(),
    );
    let response = bundle
        .router()
        .oneshot(
            Request::builder()
                .uri("/api/admin/accounts")
                .header("cookie", "cpr_session=valid-session")
                .header("x-request-id", "admin-native-cause-test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        response
            .extensions()
            .get::<gateway_admin::model::AdminError>()
            .is_none()
    );
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE_ADMIN_NATIVE_CAUSE"));
    let failures = diagnostics.0.lock().unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].kind, "unavailable");
    assert_eq!(
        failures[0].correlation_id.as_deref(),
        Some("admin-native-cause-test")
    );
    assert!(
        failures[0]
            .details
            .as_ref()
            .unwrap()
            .as_str()
            .contains("PRIVATE_ADMIN_NATIVE_CAUSE")
    );
}
