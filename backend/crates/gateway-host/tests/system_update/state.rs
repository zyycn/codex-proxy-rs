//! 系统更新状态恢复与任务中止持久化测试

use super::*;

#[tokio::test]
async fn invalid_update_state_should_keep_raw_details_out_of_public_error() {
    use std::error::Error as _;

    let fixture = Fixture::new();
    fs::write(
        fixture.state(),
        r#"{"operation":{"status":"private-state-sentinel"}}"#,
    )
    .expect("invalid state");
    let service = fixture.service_for_url("http://127.0.0.1:1/repos");
    let error = service.update_status().await.expect_err("invalid state");

    assert_eq!(error.kind(), SystemOperationErrorKind::Internal);
    assert_eq!(error.message(), "系统更新状态文件损坏");
    assert!(!format!("{error} {error:?}").contains("private-state-sentinel"));
    let source = error.source().expect("state decoding error");
    assert!(source.is::<serde_json::Error>());
    assert!(source.to_string().contains("private-state-sentinel"));
}

#[tokio::test]
async fn manual_deployment_reports_the_running_version_and_preserves_the_last_operation() {
    let fixture = Fixture::new();
    fs::write(fixture.state(), r#"{"previousVersion":"3.14.0","currentVersion":"3.14.1","operation":{"operationId":"completed-update","kind":"update","status":"succeeded","targetVersion":"3.14.1"}}"#).expect("completed update state");
    let mut config = fixture.config("https://api.github.com/repos");
    config.version = "3.15.0".to_owned();
    config.deployment_mode = "docker".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);
    let status = service.update_status().await.expect("status");
    assert!(!status.need_restart);
    assert_eq!(status.current_version.as_deref(), Some("3.15.0"));
    assert!(status.previous_version.is_none());
    assert_eq!(status.operation.target_version.as_deref(), Some("3.14.1"));
    assert_eq!(status.operation.status, SystemOperationStatus::Succeeded);
}

#[test]
fn dropped_update_task_should_persist_failure_even_before_its_first_poll() {
    let fixture = Fixture::new();
    let service = fixture.service_for_url("http://127.0.0.1:1/repos");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime
        .block_on(service.perform_test_update(Some(TARGET_VERSION.to_owned())))
        .expect("accepted");
    drop(runtime);
    let status: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.state()).expect("state")).expect("JSON");
    assert_eq!(status["operation"]["status"], "failed");
    assert!(status["operation"]["finishedAt"].is_string());
    assert!(!fixture.lock().exists());
}

#[tokio::test]
async fn status_should_recover_abandoned_running_state_without_stealing_a_live_lock() {
    let fixture = Fixture::new();
    fs::write(fixture.state(), r#"{"currentVersion":"1.0.0","operation":{"operationId":"abandoned","kind":"update","status":"running","targetVersion":"1.9.9"}}"#).expect("state");
    fs::write(fixture.lock(), "another process").expect("live lock");
    let service = fixture.service_for_url("http://127.0.0.1:1/repos");
    assert_eq!(
        service
            .update_status()
            .await
            .expect("locked status")
            .operation
            .status,
        SystemOperationStatus::Running
    );
    fs::remove_file(fixture.lock()).expect("release abandoned lock");
    let status = service.update_status().await.expect("recovered status");
    assert_eq!(status.operation.status, SystemOperationStatus::Failed);
    assert!(status.operation.finished_at.is_some());
    assert_eq!(status.operation.operation_id.as_deref(), Some("abandoned"));
}
