//! 验证系统升级 HTTP 接口的确认参数、异步状态与事件流

use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use chrono::Utc;
use futures::stream;
use gateway_admin::{
    model::system::{
        SystemOperationAccepted, SystemOperationKind, SystemOperationState, SystemOperationStatus,
        SystemUpdateDetail, SystemUpdateEvent, SystemUpdateEventLevel, SystemUpdateStatus,
        SystemVersion,
    },
    ports::system::{SystemOperationError, SystemOperations, SystemUpdateEventStream},
};
use gateway_api::admin::system::{self, UpdateDetailQuery, UpdateRequest};
use tower::ServiceExt as _;

use super::{AdminTestFixture, AdminTestState, unavailable_system};

#[test]
fn update_detail_query_should_default_to_no_refresh_and_reject_unknown_fields() {
    let query: UpdateDetailQuery = serde_json::from_str("{}").expect("empty query object");
    assert!(!query.refresh());
    assert!(serde_json::from_str::<UpdateDetailQuery>(r#"{"refresh":true,"extra":1}"#).is_err());
}

#[test]
fn update_request_should_preserve_target_version_for_domain_validation() {
    let request: UpdateRequest =
        serde_json::from_str(r#"{"targetVersion":"v0.2.0"}"#).expect("update request");
    assert_eq!(request.into_target_version(), "v0.2.0");
    assert!(
        serde_json::from_str::<UpdateRequest>(r#"{"targetVersion":"v0.2.0","extra":1}"#).is_err()
    );
}

#[tokio::test]
async fn update_event_stream_should_preserve_event_id_and_download_progress_percent() {
    let fixture = AdminTestFixture::with_system(Arc::new(ProgressSystem)).await;
    fixture.auth.insert_session("valid-session");
    let response = app(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/system/update/events")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_system_update_events")
                .body(Body::empty())
                .expect("update event request"),
        )
        .await
        .expect("update event response");
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("update event body");
    let body = String::from_utf8(body.to_vec()).expect("UTF-8 event stream");

    assert!(
        body.contains(r#""id":"update-progress-1""#),
        "unexpected update event stream: {body}"
    );
    assert!(
        body.contains(r#""progressPercent":40"#),
        "unexpected update event stream: {body}"
    );
}

#[tokio::test]
async fn update_should_return_accepted_and_report_restart_only_through_status() {
    let fixture = AdminTestFixture::with_system(Arc::new(ProgressSystem)).await;
    fixture.auth.insert_session("valid-session");
    let router = app(fixture.state());
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/system/update")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_system_update")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"targetVersion":"0.2.0"}"#))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.expect("body"))
            .expect("JSON");
    assert_eq!(body["data"]["operationId"], "update-1");
    assert_eq!(body["data"]["targetVersion"], "0.2.0");
    assert!(body["data"].get("needRestart").is_none());
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/admin/system/update/status")
                .header(header::COOKIE, "cpr_session=valid-session")
                .header("x-request-id", "req_system_update")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.expect("body"))
            .expect("JSON");
    assert_eq!(body["data"]["operation"]["status"], "succeeded");
    assert_eq!(body["data"]["needRestart"], true);
}

fn app(state: AdminTestState) -> Router {
    system::router::<AdminTestState>().with_state(state)
}

struct ProgressSystem;

#[async_trait]
impl SystemOperations for ProgressSystem {
    async fn version(&self) -> Result<SystemVersion, SystemOperationError> {
        Err(unavailable_system())
    }

    async fn update_detail(
        &self,
        _: bool,
        channel: Option<gateway_admin::model::system::SystemUpdateChannel>,
    ) -> Result<SystemUpdateDetail, SystemOperationError> {
        use gateway_admin::model::system::{SystemUpdateChannel, SystemUpdatePolicy};
        Ok(SystemUpdateDetail {
            policy: SystemUpdatePolicy {
                channel: channel.unwrap_or(SystemUpdateChannel::Stable),
                available_channels: vec![SystemUpdateChannel::Stable, SystemUpdateChannel::Beta],
            },
            current_version: "0.1.0".into(),
            latest_version: "0.2.0".into(),
            has_update: true,
            deployment_mode: "docker".into(),
            build_type: "release".into(),
            release_url: None,
            notes: None,
            cached: false,
            update_supported: true,
            unsupported_reason: None,
            warning: None,
        })
    }

    fn update_events(&self) -> SystemUpdateEventStream {
        Box::pin(stream::once(async {
            SystemUpdateEvent {
                id: "update-progress-1".to_owned(),
                operation_id: Some("update-1".to_owned()),
                level: SystemUpdateEventLevel::Info,
                step: Some("download".to_owned()),
                message: "已下载 4.0 MiB / 10.0 MiB (40%)".to_owned(),
                terminal: true,
                progress_percent: Some(40),
                occurred_at: Utc::now(),
            }
        }))
    }

    async fn perform_update(
        &self,
        target: Option<String>,
        _: Option<gateway_admin::model::system::SystemUpdateChannel>,
        _: Arc<dyn gateway_admin::ports::system::SystemUpdatePreflight>,
    ) -> Result<SystemOperationAccepted, SystemOperationError> {
        Ok(SystemOperationAccepted::Update {
            operation_id: "update-1".to_owned(),
            deployment_mode: "docker".to_owned(),
            message: "更新已开始".to_owned(),
            target_version: target.expect("target"),
        })
    }

    async fn update_status(&self) -> Result<SystemUpdateStatus, SystemOperationError> {
        Ok(SystemUpdateStatus {
            previous_version: Some("0.1.0".to_owned()),
            current_version: Some("0.2.0".to_owned()),
            need_restart: true,
            operation: SystemOperationState {
                operation_id: Some("update-1".to_owned()),
                kind: Some(SystemOperationKind::Update),
                status: SystemOperationStatus::Succeeded,
                target_version: Some("0.2.0".to_owned()),
                message: Some("done".to_owned()),
                error: None,
                started_at: Some(Utc::now()),
                finished_at: Some(Utc::now()),
            },
        })
    }

    async fn rollback(
        &self,
        _: Arc<dyn gateway_admin::ports::system::SystemUpdatePreflight>,
    ) -> Result<SystemOperationAccepted, SystemOperationError> {
        Err(unavailable_system())
    }

    async fn restart(
        &self,
        _preflight: Arc<dyn gateway_admin::ports::system::SystemRestartPreflight>,
    ) -> Result<SystemOperationAccepted, SystemOperationError> {
        Err(unavailable_system())
    }
}

#[tokio::test]
async fn update_detail_should_validate_and_forward_the_temporary_channel() {
    let fixture = AdminTestFixture::with_system(Arc::new(ProgressSystem)).await;
    fixture.auth.insert_session("valid-session");
    let router = app(fixture.state());
    for (authenticated, query, expected) in [
        (false, "channel=beta", StatusCode::UNAUTHORIZED),
        (true, "channel=nightly", StatusCode::BAD_REQUEST),
        (true, "channel=beta&extra=true", StatusCode::BAD_REQUEST),
        (true, "channel=beta", StatusCode::OK),
        (true, "", StatusCode::OK),
    ] {
        let mut request = Request::builder()
            .uri(format!("/api/admin/system/update/detail?{query}"))
            .header("x-request-id", "req_channel_check");
        if authenticated {
            request = request.header(header::COOKIE, "cpr_session=valid-session");
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            let body: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.expect("body"))
                    .expect("json");
            assert_eq!(
                body["data"]["policy"]["channel"],
                if query.is_empty() { "stable" } else { "beta" }
            );
        }
    }
}
