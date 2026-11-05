//! 系统更新、回滚与重启的受理、互斥和文件交换测试

use super::*;

#[tokio::test]
async fn update_and_rollback_preserve_a_running_executable() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fs::copy(
        env!("CARGO_BIN_EXE_gateway-host-process-worker"),
        fixture.executable(),
    )
    .expect("running executable");
    let original = fs::read(fixture.executable()).expect("original executable");
    fs::write(fixture.root.path().join("mode"), b"update-probe").expect("worker mode");
    let mut child = tokio::process::Command::new(fixture.executable())
        .current_dir(fixture.root.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("start old executable");
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(5),
            child.stdout.as_mut().unwrap().read_u8()
        )
        .await
        .expect("worker ready")
        .expect("ready byte"),
        b'R'
    );
    fixture
        .mount_release(
            &server,
            TARGET_VERSION,
            ArchiveKind::Safe,
            ChecksumKind::Valid,
        )
        .await;
    let service = fixture.service(&server);
    let status = complete_update(&service, TARGET_VERSION).await;
    assert_eq!(
        status.operation.status,
        SystemOperationStatus::Succeeded,
        "{status:?}"
    );
    assert!(status.need_restart);
    assert_eq!(fs::read(fixture.backup()).expect("backup"), original);
    assert_eq!(
        fs::read(fixture.executable()).expect("installed"),
        b"new-binary"
    );
    assert!(child.try_wait().expect("old process state").is_none());

    service
        .rollback(Arc::new(AllowingUpdatePreflight))
        .await
        .expect("rollback running executable");
    assert_eq!(
        fs::read(fixture.executable()).expect("restored executable"),
        original
    );
    assert!(
        !service
            .update_status()
            .await
            .expect("restored status")
            .need_restart
    );

    service
        .perform_update(
            Some(TARGET_VERSION.to_owned()),
            None,
            Arc::new(ChangingRevisionPreflight {
                confirmations: AtomicUsize::new(0),
            }),
        )
        .await
        .expect("second update accepted");
    let failed = wait_for_update(&service).await;
    assert_eq!(
        failed.operation.status,
        SystemOperationStatus::Failed,
        "{failed:?}"
    );
    assert_eq!(
        fs::read(fixture.executable()).expect("restored after failure"),
        original
    );
    assert_eq!(
        fs::read(fixture.web().join("index.html")).expect("restored web"),
        b"old-web"
    );

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_u8(b'X')
        .await
        .expect("release old process");
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("worker exited")
            .expect("exit status")
            .success()
    );
    assert_eq!(
        fs::read(fixture.root.path().join("unblocked")).expect("old process result"),
        b"still-running"
    );
}

#[tokio::test]
async fn restart_should_not_shutdown_when_replacement_spawn_fails() {
    let fixture = Fixture::new();
    let mut config = fixture.config("http://127.0.0.1:1/repos");
    config.self_restart_enabled = true;
    config.executable_path = Some(fixture.root.path().join("missing"));
    let shutdown = CancellationToken::new();
    let service = ProcessSystemOperations::new(shutdown.clone(), config);

    assert!(
        service
            .restart(Arc::new(AllowingUpdatePreflight))
            .await
            .is_err()
    );
    assert!(!shutdown.is_cancelled());
}

#[tokio::test]
async fn restart_should_request_process_restart_inside_docker() {
    let fixture = Fixture::new();
    let mut config = fixture.config("http://127.0.0.1:1/repos");
    config.self_restart_enabled = true;
    config.deployment_mode = "docker".to_owned();
    let shutdown = CancellationToken::new();
    let service = ProcessSystemOperations::new(shutdown.clone(), config);
    service
        .restart(Arc::new(AllowingUpdatePreflight))
        .await
        .expect("restart accepted");

    tokio::time::timeout(Duration::from_secs(2), shutdown.cancelled())
        .await
        .expect("shutdown requested");
}

#[tokio::test(start_paused = true)]
async fn accepted_restart_excludes_system_operations_until_service_release() {
    let fixture = Fixture::new();
    let mut config = fixture.config("http://127.0.0.1:1/repos");
    config.self_restart_enabled = true;
    config.deployment_mode = "docker".to_owned();
    let shutdown = CancellationToken::new();
    let service = ProcessSystemOperations::new(shutdown.clone(), config.clone());
    service
        .restart(Arc::new(AllowingUpdatePreflight))
        .await
        .expect("restart accepted");

    assert!(!shutdown.is_cancelled(), "response delivery still has time");
    assert_system_operations_rejected(&service, "服务正在重启").await;
    assert!(fixture.lock().exists());
    let competing = ProcessSystemOperations::new(CancellationToken::new(), config);
    assert_eq!(
        competing
            .restart(Arc::new(AllowingUpdatePreflight))
            .await
            .expect_err("another service cannot replace files during restart")
            .kind(),
        SystemOperationErrorKind::Conflict
    );

    shutdown.cancelled().await;
    assert_system_operations_rejected(&service, "服务正在重启").await;
    service
        .update_status()
        .await
        .expect("status remains readable");
    assert!(fixture.lock().exists(), "draining retains the file lock");

    drop(service);
    assert!(
        !fixture.lock().exists(),
        "service release clears the file lock"
    );
}

#[tokio::test]
async fn shutdown_rejects_new_system_operations() {
    let fixture = Fixture::new();
    let mut config = fixture.config("http://127.0.0.1:1/repos");
    config.self_restart_enabled = true;
    config.deployment_mode = "docker".to_owned();
    let shutdown = CancellationToken::new();
    let service = ProcessSystemOperations::new(shutdown.clone(), config);
    shutdown.cancel();

    assert_system_operations_rejected(&service, "服务正在关闭").await;
    assert!(!fixture.lock().exists());
}

#[tokio::test]
async fn shutdown_during_restart_preflight_does_not_schedule_replacement() {
    struct CancellingPreflight(CancellationToken);
    #[async_trait::async_trait]
    impl gateway_admin::ports::system::SystemRestartPreflight for CancellingPreflight {
        async fn prepare(
            &self,
            _: Option<SystemUpdateCandidate>,
        ) -> Result<(), SystemOperationError> {
            self.0.cancel();
            Ok(())
        }
    }

    let fixture = Fixture::new();
    let mut config = fixture.config("http://127.0.0.1:1/repos");
    config.self_restart_enabled = true;
    config.deployment_mode = "docker".to_owned();
    let shutdown = CancellationToken::new();
    let service = ProcessSystemOperations::new(shutdown.clone(), config);
    let error = service
        .restart(Arc::new(CancellingPreflight(shutdown)))
        .await
        .expect_err("shutdown won during preflight");

    assert_eq!(error.kind(), SystemOperationErrorKind::Conflict);
    assert_eq!(error.message(), "服务正在关闭");
    assert!(!fixture.lock().exists());
}

async fn assert_system_operations_rejected(service: &ProcessSystemOperations, reason: &str) {
    for result in [
        service.restart(Arc::new(AllowingUpdatePreflight)).await,
        service.rollback(Arc::new(AllowingUpdatePreflight)).await,
        service
            .perform_test_update(Some(TARGET_VERSION.to_owned()))
            .await,
    ] {
        let error = result.expect_err("system operation must not be accepted");
        assert_eq!(error.kind(), SystemOperationErrorKind::Conflict);
        assert_eq!(error.message(), reason);
    }
    assert_eq!(
        service
            .restart_candidate()
            .await
            .expect_err("no restart plan")
            .message(),
        reason
    );
}

#[tokio::test]
async fn restart_should_wait_for_service_release_before_starting_replacement() {
    let fixture = Fixture::new();
    let replacement_marker = fixture.root.path().join("replacement-ran");
    fs::copy(
        env!("CARGO_BIN_EXE_gateway-host-process-worker"),
        fixture.executable(),
    )
    .expect("replacement executable");
    fs::write(fixture.root.path().join("restart-probe"), b"").expect("restart probe");
    let mut config = fixture.config("http://127.0.0.1:1/repos");
    config.self_restart_enabled = true;
    let shutdown = CancellationToken::new();
    let service = ProcessSystemOperations::new(shutdown.clone(), config);

    assert_eq!(
        service
            .restart(Arc::new(AllowingUpdatePreflight))
            .await
            .expect("replacement scheduled")
            .kind(),
        SystemOperationKind::Restart
    );
    tokio::time::timeout(Duration::from_secs(2), shutdown.cancelled())
        .await
        .expect("shutdown requested after spawn");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.root.path().join("replacement-waiting").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("replacement waiting for old service");
    assert!(
        !replacement_marker.exists(),
        "new service must wait for resource release"
    );
    drop(service);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !replacement_marker.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("replacement executed before fixture cleanup");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn restart_should_use_startup_path_after_running_executable_is_renamed() {
    const CHILD_ENV: &str = "CPR_TEST_RESTART_RENAMED_EXECUTABLE_CHILD";
    let executable = std::env::current_exe().expect("test executable");
    if std::env::var_os(CHILD_ENV).is_none() {
        // 用例只移动目录项，不修改旧程序内容；同盘硬链接避免复制整个调试二进制
        let directory =
            tempfile::tempdir_in(executable.parent().unwrap()).expect("isolated test directory");
        let child = directory.path().join("restart-test");
        fs::hard_link(&executable, &child).expect("link isolated test executable");
        let output = std::process::Command::new(child)
            .args([
                "--exact",
                "system_update::operations::restart_should_use_startup_path_after_running_executable_is_renamed",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .expect("isolated restart test");
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    if executable.file_name() == Some(std::ffi::OsStr::new("codex-proxy-rs.backup")) {
        // 误启动旧程序时留下可观察结果并退出，避免旧副本再次发起重启
        fs::write(
            executable
                .parent()
                .expect("installation directory")
                .join("replacement-version"),
            "old",
        )
        .expect("old replacement marker");
        return;
    }

    let fixture = Fixture::new_in(executable.parent().unwrap());
    fs::rename(executable, fixture.executable()).expect("move isolated running executable");
    let mut config = fixture.config("http://127.0.0.1:1/repos");
    config.executable_path = None;
    config.self_restart_enabled = true;
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);

    let backup = fixture.backup();
    fs::rename(fixture.executable(), &backup).expect("back up running executable");
    fixture.write_executable(
        "#!/bin/sh\ncat >/dev/null\nprintf new > \"${0%/*}/replacement-version\"\n",
    );
    assert_eq!(std::env::current_exe().expect("renamed executable"), backup);

    service
        .restart(Arc::new(AllowingUpdatePreflight))
        .await
        .expect("restart accepted");
    drop(service);
    let marker = fixture.root.path().join("replacement-version");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("replacement executed");
    assert_eq!(
        fs::read_to_string(marker).expect("replacement version"),
        "new"
    );
}

#[tokio::test]
async fn restart_should_conflict_while_another_system_operation_is_running() {
    let fixture = Fixture::new();
    // 裸 TCP 监听不回包：update 持有操作锁后停在 release 拉取阶段，accept
    // 信号保证断言时锁一定已被占用
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("release listener");
    let api_base = format!("http://{}/repos", listener.local_addr().expect("addr"));
    let (connected_tx, connected_rx) = tokio::sync::oneshot::channel();
    let upstream = tokio::spawn(async move {
        let connection = listener.accept().await;
        let _ = connected_tx.send(());
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(connection);
    });
    let mut config = fixture.config(&api_base);
    config.self_restart_enabled = true;
    config.deployment_mode = "docker".to_owned();
    let shutdown = CancellationToken::new();
    let service = Arc::new(ProcessSystemOperations::new(shutdown.clone(), config));
    let update = tokio::spawn({
        let service = Arc::clone(&service);
        async move {
            service
                .perform_test_update(Some(TARGET_VERSION.to_owned()))
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), connected_rx)
        .await
        .expect("update reaches release fetch while holding the operation lock")
        .expect("connected signal");

    let error = service
        .restart(Arc::new(AllowingUpdatePreflight))
        .await
        .expect_err("restart during update");
    assert_eq!(error.kind(), SystemOperationErrorKind::Conflict);
    assert!(!shutdown.is_cancelled());

    update.await.expect("request").expect("accepted");
    assert!(fixture.lock().exists());
    upstream.abort();
    assert_eq!(
        wait_for_update(&service).await.operation.status,
        SystemOperationStatus::Failed
    );
    service
        .restart(Arc::new(AllowingUpdatePreflight))
        .await
        .expect("restart after lock release");
}

#[tokio::test]
async fn rollback_should_restore_binary_web_and_version_state() {
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
    let updated = complete_update(&service, TARGET_VERSION).await;
    assert_eq!(
        updated.operation.status,
        SystemOperationStatus::Succeeded,
        "{updated:?}"
    );
    assert_eq!(
        fs::read(fixture.official().join("plugin-release-manifest.json")).expect("manifest"),
        b"new-manifest"
    );
    assert!(fixture.official().join("new-plugin.tar.gz").is_file());
    assert!(!fixture.official().join("old-plugin.tar.gz").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        assert_eq!(
            fs::metadata(fixture.official())
                .expect("official directory")
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
        for name in ["plugin-release-manifest.json", "new-plugin.tar.gz"] {
            assert_eq!(
                fs::metadata(fixture.official().join(name))
                    .expect("official plugin file")
                    .permissions()
                    .mode()
                    & 0o777,
                0o444,
                "{name}"
            );
        }
    }
    service
        .rollback(Arc::new(AllowingUpdatePreflight))
        .await
        .expect("rollback");

    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"old-binary"
    );
    assert_eq!(
        fs::read(fixture.web().join("index.html")).expect("web"),
        b"old-web"
    );
    assert_eq!(
        fs::read(fixture.official().join("plugin-release-manifest.json")).expect("manifest"),
        OLD_RELEASE_MANIFEST.as_bytes()
    );
    assert_eq!(
        fs::read(fixture.official().join("old-plugin.tar.gz")).expect("old plugin"),
        b"old-plugin"
    );
    assert!(!fixture.official().join("new-plugin.tar.gz").exists());
    assert_eq!(
        service
            .update_status()
            .await
            .expect("status")
            .operation
            .status,
        SystemOperationStatus::Succeeded
    );
    assert_eq!(
        complete_update(&service, TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Succeeded,
        "第二次更新必须能清理只读的官方目录备份"
    );
    assert_eq!(
        fs::read(fixture.official().join("plugin-release-manifest.json")).expect("manifest"),
        b"new-manifest"
    );
}

#[tokio::test]
async fn update_should_restore_all_files_when_plugin_revision_changes_during_swap() {
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
    SystemOperations::perform_update(
        &service,
        Some(TARGET_VERSION.to_owned()),
        None,
        Arc::new(ChangingRevisionPreflight {
            confirmations: AtomicUsize::new(0),
        }),
    )
    .await
    .expect("update accepted");

    let status = wait_for_update(&service).await;
    assert_eq!(status.operation.status, SystemOperationStatus::Failed);
    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"old-binary"
    );
    assert_eq!(
        fs::read(fixture.web().join("index.html")).expect("web"),
        b"old-web"
    );
    assert_eq!(
        fs::read(fixture.official().join("plugin-release-manifest.json")).expect("manifest"),
        OLD_RELEASE_MANIFEST.as_bytes()
    );
    assert!(fixture.official().join("old-plugin.tar.gz").is_file());
    assert!(!fixture.official().join("new-plugin.tar.gz").exists());
}

#[tokio::test]
async fn rollback_should_restore_current_release_when_plugin_revision_changes_during_swap() {
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
    assert_eq!(
        complete_update(&service, TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Succeeded
    );

    let error = service
        .rollback(Arc::new(ChangingRollbackRevisionPreflight {
            confirmations: AtomicUsize::new(0),
        }))
        .await
        .expect_err("revision changed after rollback swap");

    assert_eq!(error.kind(), SystemOperationErrorKind::Conflict);
    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"new-binary"
    );
    assert_eq!(
        fs::read(fixture.web().join("index.html")).expect("web"),
        b"new-web"
    );
    assert_eq!(
        fs::read(fixture.official().join("plugin-release-manifest.json")).expect("manifest"),
        b"new-manifest"
    );
    assert_eq!(
        service
            .update_status()
            .await
            .expect("status")
            .operation
            .status,
        SystemOperationStatus::Failed
    );
}

#[tokio::test]
async fn cancelled_rollback_should_restore_current_release_after_the_swap() {
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
    assert_eq!(
        complete_update(&service, TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Succeeded
    );
    let reached_after_swap = Arc::new(tokio::sync::Notify::new());
    let task_service = service.clone();
    let task_reached = Arc::clone(&reached_after_swap);
    let task = tokio::spawn(async move {
        task_service
            .rollback(Arc::new(BlockingRollbackPreflight {
                confirmations: AtomicUsize::new(0),
                reached_after_swap: task_reached,
            }))
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), reached_after_swap.notified())
        .await
        .expect("post-swap confirmation reached");
    task.abort();
    assert!(
        task.await
            .expect_err("rollback task cancelled")
            .is_cancelled()
    );

    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"new-binary"
    );
    assert_eq!(
        fs::read(fixture.web().join("index.html")).expect("web"),
        b"new-web"
    );
    assert_eq!(
        fs::read(fixture.official().join("plugin-release-manifest.json")).expect("manifest"),
        b"new-manifest"
    );
    assert_eq!(
        service
            .update_status()
            .await
            .expect("recovered status")
            .operation
            .status,
        SystemOperationStatus::Failed
    );
}

#[tokio::test]
async fn experimental_update_should_install_same_channel_release() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release(
            &server,
            "3.10.0-exp.3",
            ArchiveKind::Safe,
            ChecksumKind::Valid,
        )
        .await;
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = "3.10.0-exp.2".to_owned();
    config.build_type = "experimental".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);

    assert_eq!(
        complete_update(&service, "3.10.0-exp.3")
            .await
            .operation
            .status,
        SystemOperationStatus::Succeeded
    );
    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"new-binary"
    );
    assert_eq!(
        fs::read(fixture.web().join("index.html")).expect("web"),
        b"new-web"
    );
    assert_eq!(
        service
            .update_status()
            .await
            .expect("status")
            .operation
            .status,
        SystemOperationStatus::Succeeded
    );
}

#[tokio::test]
async fn update_should_reject_cross_channel_targets_before_fetching_or_replacing_files() {
    for (build_type, current, target) in [
        ("experimental", "3.10.0-exp.2", "3.12.0"),
        ("experimental", "3.10.0-exp.2", "3.10.0"),
        ("experimental", "3.10.0-exp.2", "3.10.1-exp.3"),
        ("experimental", "3.10.0-exp.2", "3.10.0-exp.1"),
        ("release", "3.10.0-beta.2", "3.10.0-alpha.3"),
        ("release", "3.10.0", "3.10.0+new"),
        ("release", "3.10.0", "3.9.0"),
        ("experimental", "3.10.0-exp.2", "3.11.0-beta.1"),
        ("experimental", "3.10.0-exp.2", "3.10.0-exp.other.3"),
        ("release", "3.10.0", "3.11.0-exp.1"),
        ("release", "3.10.0", "3.11.0-beta.1"),
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let mut config = fixture.config(&format!("{}/repos", server.uri()));
        config.version = current.to_owned();
        config.build_type = build_type.to_owned();
        let service = ProcessSystemOperations::new(CancellationToken::new(), config);

        let error = service
            .perform_test_update(Some(target.to_owned()))
            .await
            .expect_err("cross-channel update");
        assert_eq!(error.kind(), SystemOperationErrorKind::Conflict);
        assert!(error.to_string().contains("所选通道"));
        assert!(
            server
                .received_requests()
                .await
                .expect("requests")
                .is_empty()
        );
        assert_eq!(
            fs::read(fixture.executable()).expect("binary"),
            b"old-binary"
        );
        assert_eq!(
            fs::read(fixture.web().join("index.html")).expect("web"),
            b"old-web"
        );
        assert!(!fixture.state().exists());
    }
}

#[tokio::test]
async fn prerelease_updates_should_install_later_stages_and_stable_release() {
    for (current, target) in [
        ("3.12.0-alpha.1", "3.12.0-beta.1"),
        ("3.12.0-beta.1", "3.12.0-rc.1"),
        ("3.12.0-rc.1", "3.12.0"),
        ("3.12.0-alpha.1", "3.12.0"),
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        fixture
            .mount_release(&server, target, ArchiveKind::Safe, ChecksumKind::Valid)
            .await;
        let mut config = fixture.config(&format!("{}/repos", server.uri()));
        config.version = current.to_owned();
        let service = ProcessSystemOperations::new(CancellationToken::new(), config);
        assert!(
            service
                .update_detail(true, None)
                .await
                .expect("detail")
                .has_update
        );
        assert_eq!(
            complete_update(&service, target).await.operation.status,
            SystemOperationStatus::Succeeded
        );
        assert_eq!(
            fs::read(fixture.executable()).expect("binary"),
            b"new-binary"
        );
        assert_eq!(
            fs::read(fixture.web().join("index.html")).expect("web"),
            b"new-web"
        );
        assert_eq!(
            service
                .update_status()
                .await
                .expect("status")
                .operation
                .status,
            SystemOperationStatus::Succeeded
        );
    }
}

#[tokio::test]
async fn update_should_reject_cross_major_target_before_fetching_release() {
    let fixture = Fixture::new();
    let service = ProcessSystemOperations::new(
        CancellationToken::new(),
        fixture.config("http://127.0.0.1:1/repos"),
    );

    let error = service
        .perform_test_update(Some(CROSS_MAJOR_VERSION.to_owned()))
        .await
        .expect_err("cross-major target must be rejected");
    assert_eq!(error.kind(), SystemOperationErrorKind::Conflict);
    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"old-binary"
    );
}

#[tokio::test]
async fn update_should_fail_when_release_checksum_is_missing() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release(
            &server,
            TARGET_VERSION,
            ArchiveKind::Safe,
            ChecksumKind::Missing,
        )
        .await;

    assert_eq!(
        complete_update(&fixture.service(&server), TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Failed,
    );
}

#[tokio::test]
async fn update_should_fail_when_release_checksum_mismatches() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release(
            &server,
            TARGET_VERSION,
            ArchiveKind::Safe,
            ChecksumKind::Mismatch,
        )
        .await;

    assert_eq!(
        complete_update(&fixture.service(&server), TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Failed,
    );
}

#[tokio::test]
async fn update_should_reject_insecure_release_archive() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_custom_release(&server, "http://github.com/archive", None)
        .await;

    assert_eq!(
        complete_update(&fixture.service(&server), TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Failed,
    );
}

#[tokio::test]
async fn update_should_reject_release_archive_from_untrusted_host() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_custom_release(&server, "https://evil.example/archive", None)
        .await;

    assert_eq!(
        complete_update(&fixture.service(&server), TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Failed,
    );
}

#[tokio::test]
async fn update_should_reject_release_archive_with_unsafe_path() {
    for unsafe_path in [
        "../escape",
        #[cfg(windows)]
        "web/dist/index.html:payload",
        #[cfg(windows)]
        "web/dist/NUL.txt",
        #[cfg(windows)]
        "web/dist/COM¹",
        #[cfg(windows)]
        "web/dist/trailing. ",
        #[cfg(windows)]
        "C:/escape",
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        fixture
            .mount_release(
                &server,
                TARGET_VERSION,
                ArchiveKind::UnsafePath(unsafe_path),
                ChecksumKind::Valid,
            )
            .await;

        assert_eq!(
            complete_update(&fixture.service(&server), TARGET_VERSION)
                .await
                .operation
                .status,
            SystemOperationStatus::Failed,
        );
        assert_eq!(
            fs::read(fixture.executable()).expect("unchanged binary"),
            b"old-binary"
        );
    }
}

#[tokio::test]
async fn update_should_require_a_flat_regular_official_plugin_bundle() {
    for archive_kind in [
        ArchiveKind::MissingOfficialManifest,
        ArchiveKind::NestedOfficialAsset,
        ArchiveKind::OfficialSymlink,
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        fixture
            .mount_release(&server, TARGET_VERSION, archive_kind, ChecksumKind::Valid)
            .await;

        assert_eq!(
            complete_update(&fixture.service(&server), TARGET_VERSION)
                .await
                .operation
                .status,
            SystemOperationStatus::Failed,
        );
    }
}

#[tokio::test]
async fn update_should_reject_untrusted_github_api_base() {
    let fixture = Fixture::new();
    let service = ProcessSystemOperations::new(
        CancellationToken::new(),
        fixture.config("https://api.github.example/repos"),
    );
    let error = service
        .perform_test_update(Some(TARGET_VERSION.to_owned()))
        .await
        .expect_err("untrusted API rejected");

    assert_eq!(error.kind(), SystemOperationErrorKind::Conflict);
}

#[tokio::test]
async fn update_should_reject_when_confirmed_target_differs_from_remote_latest() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release(&server, "1.9.8", ArchiveKind::Safe, ChecksumKind::Valid)
        .await;

    assert_eq!(
        complete_update(&fixture.service(&server), TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Failed,
    );
}

#[tokio::test]
async fn update_should_remove_stale_file_lock_and_continue() {
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
    fs::write(fixture.lock(), "stale").expect("lock");
    filetime::set_file_mtime(fixture.lock(), FileTime::from_unix_time(1, 0)).expect("old mtime");

    assert_eq!(
        complete_update(&fixture.service(&server), TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Succeeded,
    );
}

#[tokio::test]
async fn update_and_rollback_should_use_the_default_api_asset_directory() {
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
    let mut host: gateway_host::HostConfig = serde_json::from_value(serde_json::json!({
        "listen": { "host": "127.0.0.1", "port": 8080 },
        "runtime_data_dir": "../.runtime/data",
        "logging": {
            "level": "info", "stdout": true,
            "file": { "enabled": false, "directory": "../.runtime/logs", "max_file_size_mb": 20 }
        }
    }))
    .expect("binary host config");
    host.system_update = fixture.config(&format!("{}/repos", server.uri()));
    host.system_update.web_dist_dir = None;
    host.resolve_and_validate(&fixture.root.path().join("deploy"), &fixture.web())
        .expect("inherit API asset directory");
    let service = ProcessSystemOperations::new(CancellationToken::new(), host.system_update);
    assert_eq!(
        complete_update(&service, TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Succeeded
    );

    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"new-binary"
    );
    assert_eq!(
        fs::read(fixture.web().join("index.html")).expect("web"),
        b"new-web"
    );

    service
        .rollback(Arc::new(AllowingUpdatePreflight))
        .await
        .expect("rollback");
    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"old-binary"
    );
    assert_eq!(
        fs::read(fixture.web().join("index.html")).expect("web"),
        b"old-web"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn update_should_replace_web_assets_across_filesystems() {
    let Ok(external) = tempfile::tempdir_in("/dev/shm") else {
        return;
    };
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
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    let web_dist = external.path().join("dist");
    config.web_dist_dir = Some(web_dist.clone());
    fs::create_dir_all(&web_dist).expect("web dir");
    fs::write(web_dist.join("index.html"), "old-web").expect("web");
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);
    assert_eq!(
        complete_update(&service, TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Succeeded
    );

    assert_eq!(
        fs::read(web_dist.join("index.html")).expect("web"),
        b"new-web"
    );
}

#[tokio::test]
async fn update_should_restore_web_assets_when_binary_backup_fails() {
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
    fs::create_dir(fixture.backup()).expect("blocking backup dir");

    assert_eq!(
        complete_update(&fixture.service(&server), TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Failed,
    );
    assert_eq!(
        fs::read(fixture.web().join("index.html")).expect("web"),
        b"old-web"
    );
    assert_eq!(
        fs::read(fixture.official().join("plugin-release-manifest.json")).expect("manifest"),
        OLD_RELEASE_MANIFEST.as_bytes()
    );
    assert!(fixture.official().join("old-plugin.tar.gz").is_file());
    assert!(!fixture.official().join("new-plugin.tar.gz").exists());
}

#[tokio::test]
async fn version_should_return_backend_build_metadata() {
    let fixture = Fixture::new();
    let mut config = fixture.config("https://api.github.com/repos");
    config.version = "2.3.4".to_owned();
    config.git_sha = "abc123".to_owned();
    config.update_repository = None;
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);
    let version = service.version().await.expect("version");

    assert_eq!(
        (version.version.as_str(), version.git_sha.as_str()),
        ("2.3.4", "abc123")
    );
}

#[tokio::test]
async fn accepted_update_should_survive_a_lost_http_response() {
    let upstream = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release(
            &upstream,
            TARGET_VERSION,
            ArchiveKind::Safe,
            ChecksumKind::Valid,
        )
        .await;
    Mock::given(method("GET"))
        .and(path("/archive"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(release_archive(ArchiveKind::Safe))
                .set_delay(Duration::from_secs(2)),
        )
        .with_priority(1)
        .mount(&upstream)
        .await;
    let service = Arc::new(fixture.service(&upstream));
    let handler_service = Arc::clone(&service);
    let accepted = Arc::new(tokio::sync::Notify::new());
    let handler_accepted = Arc::clone(&accepted);
    let router = axum::Router::new().route(
        "/update",
        axum::routing::post(move || {
            let service = Arc::clone(&handler_service);
            let accepted = Arc::clone(&handler_accepted);
            async move {
                service
                    .perform_test_update(Some(TARGET_VERSION.to_owned()))
                    .await
                    .expect("accepted");
                accepted.notify_one();
                // 模拟受理响应丢失，客户端断开会取消此 HTTP handler
                std::future::pending::<String>().await
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    let client = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("http://{address}/update"))
            .send()
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), accepted.notified())
        .await
        .expect("accepted before slow download finishes");
    client.abort();
    let _ = client.await;
    assert_eq!(
        service
            .update_status()
            .await
            .expect("running status")
            .operation
            .status,
        SystemOperationStatus::Running
    );
    assert_eq!(
        service
            .perform_test_update(Some(TARGET_VERSION.to_owned()))
            .await
            .expect_err("duplicate update")
            .kind(),
        SystemOperationErrorKind::Conflict
    );
    assert_eq!(
        service
            .rollback(Arc::new(AllowingUpdatePreflight))
            .await
            .expect_err("rollback during update")
            .kind(),
        SystemOperationErrorKind::Conflict
    );
    let status = wait_for_update(&service).await;
    assert_eq!(status.operation.status, SystemOperationStatus::Succeeded);
    assert!(status.need_restart);
    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"new-binary"
    );
    assert!(!fixture.lock().exists());
    assert_eq!(
        service
            .perform_test_update(Some(TARGET_VERSION.to_owned()))
            .await
            .expect_err("restart required")
            .kind(),
        SystemOperationErrorKind::Conflict
    );
    let mut restarted_config = fixture.config(&format!("{}/repos", upstream.uri()));
    restarted_config.version = TARGET_VERSION.to_owned();
    let restarted = ProcessSystemOperations::new(CancellationToken::new(), restarted_config);
    assert!(
        !restarted
            .update_status()
            .await
            .expect("restarted status")
            .need_restart
    );
    server.abort();
}

#[tokio::test]
async fn host_shutdown_should_finish_an_accepted_update_and_release_its_lock() {
    let upstream = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release(
            &upstream,
            TARGET_VERSION,
            ArchiveKind::Safe,
            ChecksumKind::Valid,
        )
        .await;
    let cancellation = CancellationToken::new();
    let service = ProcessSystemOperations::new(
        cancellation.clone(),
        fixture.config(&format!("{}/repos", upstream.uri())),
    );
    // 在后台任务第一次 poll 之前触发关闭，覆盖受理后的生命周期交接
    service
        .perform_test_update(Some(TARGET_VERSION.to_owned()))
        .await
        .expect("accepted");
    cancellation.cancel();
    let status = wait_for_update(&service).await;
    assert_eq!(status.operation.status, SystemOperationStatus::Failed);
    assert!(status.operation.error.expect("reason").contains("中断"));
    assert!(!fixture.lock().exists());
    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"old-binary"
    );
}

#[tokio::test]
async fn restart_checks_plugins_before_shutdown_and_releases_locks_on_rejection() {
    let fixture = Fixture::new();
    let mut config = fixture.config("http://127.0.0.1:1/repos");
    config.self_restart_enabled = true;
    config.deployment_mode = "docker".into();
    let shutdown = CancellationToken::new();
    let service = ProcessSystemOperations::new(shutdown.clone(), config);
    assert!(
        service
            .restart(Arc::new(RejectingRestartPreflight))
            .await
            .is_err()
    );
    assert!(!shutdown.is_cancelled());
    assert!(service.restart_candidate().await.unwrap().is_some());
    service
        .restart(Arc::new(AllowingUpdatePreflight))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), shutdown.cancelled())
        .await
        .unwrap();
}
