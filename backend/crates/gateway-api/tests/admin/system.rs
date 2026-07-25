use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, header},
};
use chrono::Utc;
use futures::stream;
use gateway_admin::{
    model::system::{
        SystemOperationAccepted, SystemUpdateDetail, SystemUpdateEvent, SystemUpdateEventLevel,
        SystemUpdateStatus, SystemVersion,
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
async fn update_event_stream_should_preserve_download_progress_percent() {
    let fixture = AdminTestFixture::with_system(Arc::new(ProgressSystem)).await;
    fixture.auth.insert_session("valid-session");
    let response = app(fixture.state())
        .oneshot(
            Request::builder()
                .uri("/api/admin/system/update/events")
                .header(header::COOKIE, "cpr_admin_session=valid-session")
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
        body.contains(r#""progressPercent":40"#),
        "unexpected update event stream: {body}"
    );
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

    async fn update_detail(&self, _: bool) -> Result<SystemUpdateDetail, SystemOperationError> {
        Err(unavailable_system())
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
        _: Option<String>,
    ) -> Result<SystemOperationAccepted, SystemOperationError> {
        Err(unavailable_system())
    }

    async fn update_status(&self) -> Result<SystemUpdateStatus, SystemOperationError> {
        Err(unavailable_system())
    }

    async fn rollback(&self) -> Result<SystemOperationAccepted, SystemOperationError> {
        Err(unavailable_system())
    }

    async fn restart(&self) -> Result<SystemOperationAccepted, SystemOperationError> {
        Err(unavailable_system())
    }
}
