//! 验证安装状态恢复、外部文件变化与回滚备份校验

use super::*;

#[tokio::test]
async fn manual_image_replacement_should_clear_pending_state_and_invalid_backup() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release(
            &server,
            TARGET_VERSION,
            ArchiveKind::Safe,
            ChecksumKind::Valid,
        )
        .await;
    let service = fixture.service(&server);
    assert!(complete_update(&service, TARGET_VERSION).await.need_restart);
    // 模拟重建镜像：数据卷保留，应用文件和容器内备份被重新替换
    fixture.write_executable("manually-installed-binary");
    fs::remove_file(fixture.root.path().join("codex-proxy-rs.backup"))
        .expect("remove image backup");
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = "1.1.0".into();
    config.deployment_mode = "docker".into();
    let redeployed = ProcessSystemOperations::new(CancellationToken::new(), config);
    let status = redeployed.update_status().await.expect("reconciled");
    assert!(!status.need_restart);
    assert_eq!(status.current_version.as_deref(), Some("1.1.0"));
    assert!(status.previous_version.is_none());
    assert_eq!(
        status.operation.target_version.as_deref(),
        Some(TARGET_VERSION)
    );
    assert!(
        redeployed
            .update_detail(true, None)
            .await
            .expect("check")
            .has_update
    );
    assert_eq!(
        complete_update(&redeployed, TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Succeeded
    );
}

#[tokio::test]
async fn rollback_after_restart_should_require_restart_even_for_a_lower_version() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release(
            &server,
            TARGET_VERSION,
            ArchiveKind::Safe,
            ChecksumKind::Valid,
        )
        .await;
    let service = fixture.service(&server);
    assert!(complete_update(&service, TARGET_VERSION).await.need_restart);
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = TARGET_VERSION.into();
    let restarted = ProcessSystemOperations::new(CancellationToken::new(), config);
    let status = restarted.update_status().await.expect("new process");
    assert!(!status.need_restart);
    assert_eq!(status.previous_version.as_deref(), Some("1.0.0"));
    restarted
        .rollback(Arc::new(AllowingUpdatePreflight))
        .await
        .expect("rollback");
    let status = restarted.update_status().await.expect("pending rollback");
    assert!(status.need_restart);
    assert_eq!(status.current_version.as_deref(), Some("1.0.0"));
    assert!(
        !fixture
            .service(&server)
            .update_status()
            .await
            .expect("rollback restarted")
            .need_restart
    );
}

#[tokio::test]
async fn external_change_during_runtime_should_not_be_reported_as_a_successful_installation() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = fixture.service(&server);
    service.update_status().await.expect("baseline");
    fs::write(fixture.web().join("index.html"), "unexpected-web").expect("external replacement");
    assert_eq!(
        service
            .update_status()
            .await
            .expect_err("mixed deployment")
            .kind(),
        SystemOperationErrorKind::Conflict
    );
    assert!(
        service
            .perform_test_update(Some(TARGET_VERSION.into()))
            .await
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"old-binary"
    );
}

#[tokio::test]
async fn pending_update_should_revoke_rollback_when_backup_content_changes() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release(
            &server,
            TARGET_VERSION,
            ArchiveKind::Safe,
            ChecksumKind::Valid,
        )
        .await;
    let service = fixture.service(&server);
    assert!(complete_update(&service, TARGET_VERSION).await.need_restart);
    fs::write(
        fixture.root.path().join("codex-proxy-rs.backup"),
        "unrelated-backup",
    )
    .expect("changed backup");
    let status = service
        .update_status()
        .await
        .expect("pending with invalid backup");
    assert!(status.need_restart);
    assert!(status.previous_version.is_none());
    assert!(
        service
            .rollback(Arc::new(AllowingUpdatePreflight))
            .await
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"new-binary"
    );
}
