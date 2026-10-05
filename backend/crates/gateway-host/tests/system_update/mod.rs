//! 系统更新测试入口，以及重启准备和操作互斥测试

mod channels;
mod installation;

use std::fs;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use filetime::FileTime;
use flate2::{Compression, write::GzEncoder};
use futures::StreamExt as _;
use gateway_admin::model::{
    Revision,
    system::{
        SystemOperationAccepted, SystemOperationKind, SystemOperationStatus,
        SystemUpdateEventLevel, SystemUpdateStatus,
    },
};
use gateway_admin::ports::system::{
    SystemOperationError, SystemOperationErrorKind, SystemOperations, SystemUpdateCandidate,
    SystemUpdatePreflight,
};
use gateway_core::lifecycle::CancellationToken;
use gateway_host::system_update::{
    ProcessSystemOperations, SystemUpdateConfig, validate_download_url,
};
use sha2::{Digest as _, Sha256};
use tar::{Builder, EntryType, Header};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OLD_RELEASE_MANIFEST: &str = r#"{"gateway_version":"1.0.0"}"#;

const TARGET_VERSION: &str = "1.9.9";
const CROSS_MAJOR_VERSION: &str = "2.0.0";

struct AllowingUpdatePreflight;
#[async_trait::async_trait]
impl gateway_admin::ports::system::SystemRestartPreflight for AllowingUpdatePreflight {
    async fn prepare(&self, _: Option<SystemUpdateCandidate>) -> Result<(), SystemOperationError> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl SystemUpdatePreflight for AllowingUpdatePreflight {
    async fn validate(&self, _: SystemUpdateCandidate) -> Result<Revision, SystemOperationError> {
        Revision::new(1).map_err(|_| {
            SystemOperationError::new(SystemOperationErrorKind::Internal, "invalid test revision")
        })
    }

    async fn confirm_revision(&self, _: Revision) -> Result<(), SystemOperationError> {
        Ok(())
    }
}

#[async_trait::async_trait]
trait TestSystemUpdate {
    async fn perform_test_update(
        &self,
        target: Option<String>,
    ) -> Result<SystemOperationAccepted, SystemOperationError>;
}

#[async_trait::async_trait]
impl TestSystemUpdate for ProcessSystemOperations {
    async fn perform_test_update(
        &self,
        target: Option<String>,
    ) -> Result<SystemOperationAccepted, SystemOperationError> {
        SystemOperations::perform_update(self, target, None, Arc::new(AllowingUpdatePreflight))
            .await
    }
}

struct ChangingRevisionPreflight {
    confirmations: AtomicUsize,
}

#[async_trait::async_trait]
impl SystemUpdatePreflight for ChangingRevisionPreflight {
    async fn validate(
        &self,
        candidate: SystemUpdateCandidate,
    ) -> Result<Revision, SystemOperationError> {
        assert_eq!(candidate.target_version, TARGET_VERSION);
        assert_eq!(candidate.release_manifest.as_ref(), b"new-manifest");
        Ok(Revision::new(7).expect("revision"))
    }

    async fn confirm_revision(&self, _: Revision) -> Result<(), SystemOperationError> {
        if self.confirmations.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(())
        } else {
            Err(SystemOperationError::new(
                SystemOperationErrorKind::Conflict,
                "fixture revision changed",
            ))
        }
    }
}

struct ChangingRollbackRevisionPreflight {
    confirmations: AtomicUsize,
}

#[async_trait::async_trait]
impl SystemUpdatePreflight for ChangingRollbackRevisionPreflight {
    async fn validate(
        &self,
        candidate: SystemUpdateCandidate,
    ) -> Result<Revision, SystemOperationError> {
        assert_eq!(candidate.target_version, "1.0.0");
        assert_eq!(
            candidate.release_manifest.as_ref(),
            OLD_RELEASE_MANIFEST.as_bytes()
        );
        Ok(Revision::new(11).expect("revision"))
    }

    async fn confirm_revision(&self, _: Revision) -> Result<(), SystemOperationError> {
        if self.confirmations.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(())
        } else {
            Err(SystemOperationError::new(
                SystemOperationErrorKind::Conflict,
                "fixture revision changed",
            ))
        }
    }
}

struct BlockingRollbackPreflight {
    confirmations: AtomicUsize,
    reached_after_swap: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl SystemUpdatePreflight for BlockingRollbackPreflight {
    async fn validate(
        &self,
        candidate: SystemUpdateCandidate,
    ) -> Result<Revision, SystemOperationError> {
        assert_eq!(candidate.target_version, "1.0.0");
        assert_eq!(
            candidate.release_manifest.as_ref(),
            OLD_RELEASE_MANIFEST.as_bytes()
        );
        Ok(Revision::new(13).expect("revision"))
    }

    async fn confirm_revision(&self, _: Revision) -> Result<(), SystemOperationError> {
        if self.confirmations.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(());
        }
        self.reached_after_swap.notify_one();
        std::future::pending().await
    }
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

#[tokio::test]
async fn restart_should_spawn_replacement_before_shutdown_outside_docker() {
    let fixture = Fixture::new();
    let replacement_marker = fixture.root.path().join("replacement-ran");
    fixture.write_executable("#!/bin/sh\n: > \"${0%/*}/replacement-ran\"\n");
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
                "system_update::restart_should_use_startup_path_after_running_executable_is_renamed",
            ])
            .env(CHILD_ENV, "1")
            .env("CPR_RESTART_DELAY_MS", "1200")
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

    let backup = fixture.root.path().join("codex-proxy-rs.backup");
    fs::rename(fixture.executable(), &backup).expect("back up running executable");
    fixture.write_executable("#!/bin/sh\nprintf new > \"${0%/*}/replacement-version\"\n");
    assert_eq!(std::env::current_exe().expect("renamed executable"), backup);

    service
        .restart(Arc::new(AllowingUpdatePreflight))
        .await
        .expect("restart accepted");
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
async fn update_detail_should_report_refresh_failure_and_clear_previous_update() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture.mount_release_once(&server, TARGET_VERSION).await;
    let service = fixture.service(&server);
    service
        .update_detail(true, None)
        .await
        .expect("prime cache");

    let detail = service
        .update_detail(true, None)
        .await
        .expect("failure detail");
    assert!(!detail.has_update);
    assert!(detail.warning.is_some());
    assert!(!detail.cached);
    assert_eq!(detail.latest_version, "1.0.0");
    assert!(detail.notes.is_none());
    let version = service
        .version()
        .await
        .expect("version after failed refresh");
    assert!(!version.has_update);
    assert!(version.update_warning.is_some());
}

#[tokio::test]
async fn update_detail_should_offer_latest_non_draft_release_in_current_channel() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "tag_name": "v1.1.0-beta.3",
                "name": "Draft 1.1.0-beta.3",
                "prerelease": true,
                "draft": true,
                "assets": [],
            },
            {
                "tag_name": "v1.1.0-beta.2",
                "name": "Release 1.1.0-beta.2",
                "body": "notes",
                "html_url": "https://github.com/owner/repository/releases",
                "prerelease": true,
                "assets": [],
            },
        ])))
        .mount(&server)
        .await;
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = "1.1.0-beta.1".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);

    let detail = service.update_detail(true, None).await.expect("detail");
    assert_eq!(detail.latest_version, "1.1.0-beta.2");
    assert!(detail.has_update);
}

#[tokio::test]
async fn update_detail_should_reject_untrusted_github_api_base() {
    let fixture = Fixture::new();
    let service = ProcessSystemOperations::new(
        CancellationToken::new(),
        fixture.config("https://api.github.example/repos"),
    );

    let detail = service
        .update_detail(true, None)
        .await
        .expect("safe rejection");
    assert!(!detail.update_supported);
    assert!(detail.warning.is_some() || detail.unsupported_reason.is_some());
}

#[tokio::test]
async fn update_detail_should_withhold_updates_for_source_builds() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.build_type = "source".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);

    let detail = service.update_detail(true, None).await.expect("detail");
    assert_eq!(detail.latest_version, "1.0.0");
    assert!(!detail.update_supported);
    assert!(detail.unsupported_reason.is_some());
    assert!(!detail.has_update);
    assert!(detail.notes.is_none());
    assert!(detail.release_url.is_none());
    assert!(!service.version().await.expect("version").has_update);
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}

#[tokio::test]
async fn experimental_build_should_ignore_newer_stable_and_other_experimental_releases() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "tag_name": "v4.0.0", "prerelease": false },
            { "tag_name": "v3.12.0", "prerelease": false },
            { "tag_name": "v3.10.0", "prerelease": false },
            { "tag_name": "v3.11.0-exp.3", "prerelease": true },
            { "tag_name": "v3.10.1-exp.3", "prerelease": true },
            { "tag_name": "v3.11.0-beta.1", "prerelease": true },
            { "tag_name": "v3.10.0-exp.other.3", "prerelease": true },
            { "tag_name": "v3.10.0-exp.3", "prerelease": true, "draft": true },
            { "tag_name": "v3.10.0-exp.4", "prerelease": false },
            { "tag_name": "invalid", "prerelease": true }
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = "3.10.0-exp.2".to_owned();
    config.build_type = "experimental".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);

    let detail = service.update_detail(true, None).await.expect("detail");
    assert_eq!(detail.latest_version, "3.10.0-exp.2");
    assert!(!detail.has_update);
    assert!(detail.update_supported);
    assert!(detail.notes.is_none());
    assert!(detail.release_url.is_none());
    assert!(detail.warning.is_none());
    let version = service.version().await.expect("version");
    assert!(!version.has_update);
    assert_eq!(version.update_channel, "exp");
    let cached = service
        .update_detail(false, None)
        .await
        .expect("cached detail");
    assert!(cached.cached);
    assert!(!cached.has_update);
}

#[tokio::test]
async fn experimental_update_should_find_highest_same_channel_version_across_pages() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let mut first_page = vec![
        serde_json::json!({
            "tag_name": "v3.12.0", "prerelease": false
        });
        100
    ];
    first_page[1] = serde_json::json!({ "tag_name": "v3.10.0-exp.3", "prerelease": true });
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .and(query_param("page", "1"))
        .and(query_param("per_page", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(first_page))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "tag_name": "v3.10.0-exp.10", "prerelease": true, "body": "exp.10 notes" },
            { "tag_name": "v3.10.0-exp.2", "prerelease": true }
        ])))
        .mount(&server)
        .await;
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = "3.10.0-exp.2".to_owned();
    config.build_type = "experimental".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);

    let detail = service.update_detail(true, None).await.expect("detail");
    assert_eq!(detail.latest_version, "3.10.0-exp.10");
    assert_eq!(detail.notes.as_deref(), Some("exp.10 notes"));
    assert!(detail.has_update);
    assert!(detail.update_supported);
    assert!(service.version().await.expect("version").has_update);
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
async fn update_checks_should_follow_stage_promotion_and_experimental_isolation() {
    let cases = [
        ("3.12.0-alpha.1", "alpha", [true, true, true, true, false]),
        ("3.12.0-beta.1", "beta", [false, true, true, true, false]),
        ("3.12.0-rc.1", "rc", [false, false, true, true, false]),
        ("3.12.0-exp.1", "exp", [false, false, false, false, true]),
        ("3.11.0", "stable", [false, false, false, true, false]),
    ];
    for (current, channel, expected) in cases {
        for (target, allowed) in [
            "3.12.0-alpha.2",
            "3.12.0-beta.2",
            "3.12.0-rc.2",
            "3.12.0",
            "3.12.0-exp.2",
        ]
        .into_iter()
        .zip(expected)
        {
            let server = MockServer::start().await;
            let fixture = Fixture::new();
            fixture.mount_release_once(&server, target).await;
            let mut config = fixture.config(&format!("{}/repos", server.uri()));
            config.version = current.to_owned();
            let service = ProcessSystemOperations::new(CancellationToken::new(), config);

            let detail = service.update_detail(true, None).await.expect("detail");
            assert_eq!(detail.has_update, allowed, "{current} -> {target}");
            assert_eq!(
                detail.latest_version,
                if allowed { target } else { current }
            );
            assert_eq!(detail.notes.is_some(), allowed);
            assert_eq!(detail.release_url.is_some(), allowed);
            assert!(detail.warning.is_none());
            let version = service.version().await.expect("version");
            assert_eq!(version.has_update, allowed);
            assert_eq!(version.update_channel, channel);
        }
    }
}

#[tokio::test]
async fn update_detail_should_keep_current_release_notes_without_allowing_reinstallation() {
    for current in [
        "3.12.1",
        "3.12.1-alpha.1",
        "3.12.1-beta.1",
        "3.12.1-rc.1",
        "3.12.1-exp.1",
        "3.12.1+build.1",
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let release_url = format!("https://github.com/owner/repository/releases/tag/v{current}");
        Mock::given(method("GET"))
            .and(path("/repos/owner/repository/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "tag_name": format!("v{current}"),
                    "prerelease": !semver::Version::parse(current).expect("version").pre.is_empty(),
                    "body": "## 当前版本\n\n- 修复更新状态",
                    "html_url": release_url,
                }
            ])))
            .expect(1)
            .mount(&server)
            .await;
        let mut config = fixture.config(&format!("{}/repos", server.uri()));
        config.version = current.to_owned();
        if current.contains("-exp.") {
            config.build_type = "experimental".to_owned();
        }
        let service = ProcessSystemOperations::new(CancellationToken::new(), config);

        let detail = service
            .update_detail(true, None)
            .await
            .expect("current release");
        assert_eq!(detail.latest_version, current);
        assert!(!detail.has_update);
        assert!(detail.update_supported);
        assert_eq!(
            detail.notes.as_deref(),
            Some("## 当前版本\n\n- 修复更新状态")
        );
        assert_eq!(detail.release_url.as_deref(), Some(release_url.as_str()));
        let cached = service
            .update_detail(false, None)
            .await
            .expect("cached release");
        assert!(cached.cached);
        assert_eq!(cached.notes, detail.notes);
        assert_eq!(cached.release_url, detail.release_url);
        assert!(!service.version().await.expect("version").has_update);

        let error = service
            .perform_test_update(Some(current.to_owned()))
            .await
            .expect_err("current release must not be installed again");
        assert_eq!(error.kind(), SystemOperationErrorKind::Conflict);
        assert!(!fixture.state().exists());
        assert_eq!(
            fs::read(fixture.executable()).expect("binary"),
            b"old-binary"
        );
    }
}

#[tokio::test]
async fn update_detail_should_prefer_available_update_notes_over_current_release() {
    for current_first in [true, false] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let mut releases = vec![
            serde_json::json!({
                "tag_name": "v1.0.0", "prerelease": false, "body": "current notes"
            }),
            serde_json::json!({
                "tag_name": "v1.9.9", "prerelease": false, "body": "update notes",
                "html_url": "https://github.com/owner/repository/releases/tag/v1.9.9"
            }),
        ];
        if !current_first {
            releases.reverse();
        }
        Mock::given(method("GET"))
            .and(path("/repos/owner/repository/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_json(releases))
            .mount(&server)
            .await;

        let detail = fixture
            .service(&server)
            .update_detail(true, None)
            .await
            .expect("detail");
        assert!(detail.has_update);
        assert_eq!(detail.latest_version, TARGET_VERSION);
        assert_eq!(detail.notes.as_deref(), Some("update notes"));
        assert_eq!(
            detail.release_url.as_deref(),
            Some("https://github.com/owner/repository/releases/tag/v1.9.9")
        );
    }
}

#[tokio::test]
async fn update_detail_should_find_current_release_notes_across_pages() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(vec![
            serde_json::json!({ "tag_name": "v2.0.0", "prerelease": false });
            100
        ]))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .and(query_param("page", "2"))
        .respond_with(release_response("1.0.0", Vec::new()))
        .expect(1)
        .mount(&server)
        .await;

    let detail = fixture
        .service(&server)
        .update_detail(true, None)
        .await
        .expect("detail");
    assert!(!detail.has_update);
    assert_eq!(detail.latest_version, "1.0.0");
    assert_eq!(detail.notes.as_deref(), Some("notes"));
    assert!(detail.release_url.is_some());
}

#[tokio::test]
async fn update_detail_should_not_use_draft_or_mislabeled_current_release_notes() {
    for (current, prerelease) in [("1.0.0", false), ("1.0.0-beta.1", true)] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        Mock::given(method("GET"))
            .and(path("/repos/owner/repository/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "tag_name": format!("v{current}"), "prerelease": prerelease, "draft": true, "body": "draft notes" },
                { "tag_name": format!("v{current}"), "prerelease": !prerelease, "body": "mislabeled notes" },
                { "tag_name": "v0.9.0", "prerelease": false, "body": "older notes" },
            ])))
            .mount(&server)
            .await;
        let mut config = fixture.config(&format!("{}/repos", server.uri()));
        config.version = current.to_owned();
        let service = ProcessSystemOperations::new(CancellationToken::new(), config);

        let detail = service.update_detail(true, None).await.expect("detail");
        assert!(!detail.has_update);
        assert_eq!(detail.latest_version, current);
        assert!(detail.notes.is_none());
        assert!(detail.release_url.is_none());
    }
}

#[tokio::test]
async fn update_checks_should_ignore_downgrades_unknown_stages_and_build_metadata() {
    for (current, target) in [
        ("3.12.0-beta.2", "3.12.0-beta.1"),
        ("3.12.0", "3.12.0+new-build"),
        ("3.12.0-beta.1+aaa", "3.12.0-beta.1+zzz"),
        ("3.12.0", "3.11.0"),
        ("3.12.0-alpha.1", "3.12.0-preview.2"),
        ("3.12.0-alpha.1", "3.12.0-beta"),
        ("3.12.0-alpha.1", "3.12.0-beta.0"),
        ("3.12.0-alpha.1", "3.12.0-beta.2.extra"),
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        fixture.mount_release_once(&server, target).await;
        let mut config = fixture.config(&format!("{}/repos", server.uri()));
        config.version = current.to_owned();
        let service = ProcessSystemOperations::new(CancellationToken::new(), config);
        let detail = service.update_detail(true, None).await.expect("detail");
        assert!(!detail.has_update, "{current} -> {target}");
        assert_eq!(detail.latest_version, current);
        assert!(detail.notes.is_none());
        assert!(detail.release_url.is_none());
    }
}

#[tokio::test]
async fn update_checks_should_fail_closed_for_unknown_current_channels() {
    for (current, build_type) in [
        ("3.12.0-preview.1", "release"),
        ("3.12.0-alpha", "release"),
        ("3.12.0-exp.0", "experimental"),
        ("invalid", "release"),
        ("3.12.0", "experimental"),
        ("3.12.0-beta.1", "experimental"),
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let mut config = fixture.config(&format!("{}/repos", server.uri()));
        config.version = current.to_owned();
        config.build_type = build_type.to_owned();
        let service = ProcessSystemOperations::new(CancellationToken::new(), config);
        let detail = service.update_detail(true, None).await.expect("detail");
        assert!(!detail.has_update);
        assert!(!detail.update_supported);
        assert!(detail.unsupported_reason.is_some());
        assert!(
            server
                .received_requests()
                .await
                .expect("requests")
                .is_empty()
        );
    }
}

#[tokio::test]
async fn stable_update_should_select_allowed_version_below_newer_major_releases() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "tag_name": "v4.0.0", "prerelease": false },
            { "tag_name": "v3.13.0-beta.1", "prerelease": true },
            { "tag_name": "v3.12.1", "prerelease": true },
            { "tag_name": "v3.12.0", "prerelease": false, "draft": true },
            { "tag_name": "v3.11.2", "prerelease": false },
            { "tag_name": "v3.11.10", "prerelease": false, "body": "stable notes" }
        ])))
        .mount(&server)
        .await;
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = "3.11.0".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);
    let detail = service.update_detail(true, None).await.expect("detail");
    assert!(detail.has_update);
    assert_eq!(detail.latest_version, "3.11.10");
    assert_eq!(detail.notes.as_deref(), Some("stable notes"));
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
async fn update_detail_should_use_cached_release_when_not_refreshed() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture.mount_release_once(&server, TARGET_VERSION).await;
    let service = fixture.service(&server);
    service
        .update_detail(true, None)
        .await
        .expect("prime cache");

    assert!(service.update_detail(false, None).await.is_ok());
}

#[tokio::test]
async fn update_detail_should_withhold_cross_major_release() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release_once(&server, CROSS_MAJOR_VERSION)
        .await;

    let detail = fixture
        .service(&server)
        .update_detail(true, None)
        .await
        .expect("detail");
    assert_eq!(detail.latest_version, "1.0.0");
    assert!(!detail.has_update);
    assert!(detail.warning.is_none());
    assert!(detail.notes.is_none());
    assert!(detail.release_url.is_none());
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
async fn update_events_should_close_after_terminal_update_log() {
    let fixture = Fixture::new();
    let mut config = fixture.config("https://api.github.com/repos");
    config.build_type = "source".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);
    let mut events = service.update_events();
    let _ = service
        .perform_test_update(Some(TARGET_VERSION.to_owned()))
        .await;
    let first = events.next().await.expect("terminal event");

    assert_eq!(first.level, SystemUpdateEventLevel::Error);
    assert!(events.next().await.is_none());
}

#[tokio::test]
async fn update_events_should_open_authenticated_sse_stream() {
    let fixture = Fixture::new();
    let mut config = fixture.config("https://api.github.com/repos");
    config.build_type = "source".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);
    let mut stream = service.update_events();
    let _ = service
        .perform_test_update(Some(TARGET_VERSION.to_owned()))
        .await;

    assert!(stream.next().await.is_some());
}

#[tokio::test]
async fn update_events_should_preserve_complete_release_stage_sequence() {
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
    let events = service.update_events();
    assert_eq!(
        complete_update(&service, TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Succeeded
    );
    let steps = events
        .map(|event| event.step.expect("release update step"))
        .collect::<Vec<_>>()
        .await;

    assert_eq!(
        steps,
        [
            "release",
            "prepare",
            "asset",
            "asset",
            "verify",
            "prepare",
            "download",
            "download",
            "download",
            "checksum",
            "checksum",
            "extract",
            "extract",
            "preflight",
            "preflight",
            "replace",
            "replace",
            "done",
        ]
    );
}

#[tokio::test]
async fn update_events_should_report_download_bytes_and_percent() {
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
    let events = service.update_events();
    assert_eq!(
        complete_update(&service, TARGET_VERSION)
            .await
            .operation
            .status,
        SystemOperationStatus::Succeeded
    );
    let progress = events
        .filter_map(
            |event| async move { event.progress_percent.map(|value| (value, event.message)) },
        )
        .collect::<Vec<_>>()
        .await;

    assert!(progress.iter().any(|(percent, message)| {
        *percent == 100 && message.starts_with("已下载 ") && message.ends_with(" (100%)")
    }));
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

#[test]
fn update_should_trust_github_release_asset_redirect_host() {
    assert!(
        validate_download_url(
            "https://release-assets.githubusercontent.com/github-production-release-asset/archive",
            "https://api.github.com/repos",
        )
        .is_ok()
    );
}

#[tokio::test]
async fn update_should_reject_release_archive_with_unsafe_path() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release(
            &server,
            TARGET_VERSION,
            ArchiveKind::UnsafePath,
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
    fs::create_dir(fixture.executable().with_extension("backup")).expect("blocking backup dir");

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
async fn update_status_should_reconcile_manual_deployment_without_reusing_legacy_success() {
    let fixture = Fixture::new();
    fs::write(fixture.state(), r#"{"previousVersion":"3.14.0","currentVersion":"3.14.1","operation":{"operationId":"legacy","kind":"update","status":"succeeded","targetVersion":"3.14.1"}}"#).expect("legacy state");
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

#[tokio::test]
async fn terminal_event_should_only_be_visible_after_status_is_persisted() {
    let upstream = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release(
            &upstream,
            TARGET_VERSION,
            ArchiveKind::Safe,
            ChecksumKind::Mismatch,
        )
        .await;
    let service = fixture.service(&upstream);
    let mut events = service.update_events();
    let SystemOperationAccepted::Update { operation_id, .. } = service
        .perform_test_update(Some(TARGET_VERSION.to_owned()))
        .await
        .expect("accepted")
    else {
        panic!("update")
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.next().await {
            assert_eq!(event.operation_id.as_deref(), Some(operation_id.as_str()));
            if event.terminal {
                let status = service.update_status().await.expect("terminal status");
                assert_eq!(status.operation.status, SystemOperationStatus::Failed);
                assert!(status.operation.finished_at.is_some());
                return;
            }
        }
        panic!("missing terminal event");
    })
    .await
    .expect("terminal event");
}

async fn wait_for_update(service: &ProcessSystemOperations) -> SystemUpdateStatus {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = service.update_status().await.expect("update status");
            if status.operation.status != SystemOperationStatus::Running {
                assert!(status.operation.finished_at.is_some(), "terminal timestamp");
                return status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("update reaches terminal state")
}

async fn complete_update(service: &ProcessSystemOperations, target: &str) -> SystemUpdateStatus {
    let SystemOperationAccepted::Update { operation_id, .. } = service
        .perform_test_update(Some(target.to_owned()))
        .await
        .expect("update accepted")
    else {
        panic!("expected update operation")
    };
    let status = wait_for_update(service).await;
    assert_eq!(
        status.operation.operation_id.as_deref(),
        Some(operation_id.as_str())
    );
    status
}

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self::new_in(std::env::temp_dir())
    }

    fn new_in(parent: impl AsRef<std::path::Path>) -> Self {
        let root = tempfile::tempdir_in(parent).expect("system update root");
        let fixture = Self { root };
        fixture.write_executable("old-binary");
        fs::create_dir_all(fixture.web()).expect("web dir");
        fs::write(fixture.web().join("index.html"), "old-web").expect("web");
        fs::create_dir_all(fixture.official()).expect("official plugin dir");
        fs::write(
            fixture.official().join("plugin-release-manifest.json"),
            OLD_RELEASE_MANIFEST,
        )
        .expect("manifest");
        fs::write(fixture.official().join("old-plugin.tar.gz"), "old-plugin").expect("plugin");
        fixture
    }

    fn executable(&self) -> PathBuf {
        self.root.path().join("codex-proxy-rs")
    }

    fn web(&self) -> PathBuf {
        self.root.path().join("web/dist")
    }

    fn official(&self) -> PathBuf {
        self.root.path().join("plugins/official")
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("update-state.json")
    }

    fn lock(&self) -> PathBuf {
        self.root.path().join("update.lock")
    }

    fn write_executable(&self, content: &str) {
        fs::write(self.executable(), content).expect("binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(self.executable(), fs::Permissions::from_mode(0o755))
                .expect("permissions");
        }
    }

    fn config(&self, api_base: &str) -> SystemUpdateConfig {
        SystemUpdateConfig {
            version: "1.0.0".to_owned(),
            git_sha: "test-sha".to_owned(),
            build_time: "2026-07-19T00:00:00Z".to_owned(),
            deployment_mode: "binary".to_owned(),
            build_type: "release".to_owned(),
            update_repository: Some("owner/repository".to_owned()),
            github_api_base: api_base.to_owned(),
            executable_path: Some(self.executable()),
            web_dist_dir: Some(self.web()),
            update_state_file: self.state(),
            update_lock_file: self.lock(),
            update_temp_dir: self.root.path().join("tmp"),
            self_restart_enabled: false,
        }
    }

    fn service(&self, server: &MockServer) -> ProcessSystemOperations {
        self.service_for_url(&format!("{}/repos", server.uri()))
    }

    fn service_for_url(&self, api_base: &str) -> ProcessSystemOperations {
        ProcessSystemOperations::new(CancellationToken::new(), self.config(api_base))
    }

    async fn mount_release(
        &self,
        server: &MockServer,
        version: &str,
        archive_kind: ArchiveKind,
        checksum_kind: ChecksumKind,
    ) {
        let archive = release_archive(archive_kind);
        let name = archive_name(version);
        let checksum = match checksum_kind {
            ChecksumKind::Valid => format!("{}  {name}\n", hex::encode(Sha256::digest(&archive))),
            ChecksumKind::Mismatch => format!("{}  {name}\n", "0".repeat(64)),
            ChecksumKind::Missing => String::new(),
        };
        let checksum_asset = (!matches!(checksum_kind, ChecksumKind::Missing)).then(|| {
            serde_json::json!({
                "name": "checksums.txt",
                "browser_download_url": format!("{}/checksums", server.uri()),
                "size": checksum.len(),
            })
        });
        let mut assets = vec![serde_json::json!({
            "name": name,
            "browser_download_url": format!("{}/archive", server.uri()),
            "size": archive.len(),
        })];
        assets.extend(checksum_asset);
        mount_release_json(server, version, assets, None).await;
        Mock::given(method("GET"))
            .and(path("/archive"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
            .mount(server)
            .await;
        if !checksum.is_empty() {
            Mock::given(method("GET"))
                .and(path("/checksums"))
                .respond_with(ResponseTemplate::new(200).set_body_string(checksum))
                .mount(server)
                .await;
        }
    }

    async fn mount_release_once(&self, server: &MockServer, version: &str) {
        let assets = Vec::new();
        Mock::given(method("GET"))
            .and(path("/repos/owner/repository/releases"))
            .respond_with(release_response(version, assets))
            .up_to_n_times(1)
            .mount(server)
            .await;
    }

    async fn mount_custom_release(
        &self,
        server: &MockServer,
        archive_url: &str,
        checksum_url: Option<&str>,
    ) {
        let archive = release_archive(ArchiveKind::Safe);
        let name = archive_name(TARGET_VERSION);
        let mut assets = vec![serde_json::json!({
            "name": name,
            "browser_download_url": archive_url,
            "size": archive.len(),
        })];
        if let Some(checksum_url) = checksum_url {
            assets.push(serde_json::json!({
                "name": "checksums.txt",
                "browser_download_url": checksum_url,
                "size": 80,
            }));
        } else {
            assets.push(serde_json::json!({
                "name": "checksums.txt",
                "browser_download_url": format!("{}/checksums", server.uri()),
                "size": 80,
            }));
        }
        mount_release_json(server, TARGET_VERSION, assets, None).await;
    }
}

#[derive(Clone, Copy)]
enum ArchiveKind {
    Safe,
    UnsafePath,
    MissingOfficialManifest,
    NestedOfficialAsset,
    OfficialSymlink,
}

#[derive(Clone, Copy)]
enum ChecksumKind {
    Valid,
    Mismatch,
    Missing,
}

fn release_archive(kind: ArchiveKind) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = Builder::new(encoder);
    append_file(&mut tar, "codex-proxy-rs", b"new-binary", false);
    append_file(&mut tar, "web/dist/index.html", b"new-web", false);
    if !matches!(kind, ArchiveKind::MissingOfficialManifest) {
        append_file(
            &mut tar,
            "plugins/official/plugin-release-manifest.json",
            b"new-manifest",
            false,
        );
    }
    append_file(
        &mut tar,
        "plugins/official/new-plugin.tar.gz",
        b"new-plugin",
        false,
    );
    if matches!(kind, ArchiveKind::UnsafePath) {
        append_file(&mut tar, "safe", b"escape", true);
    }
    if matches!(kind, ArchiveKind::NestedOfficialAsset) {
        append_file(
            &mut tar,
            "plugins/official/nested/plugin.tar.gz",
            b"nested",
            false,
        );
    }
    if matches!(kind, ArchiveKind::OfficialSymlink) {
        append_symlink(
            &mut tar,
            "plugins/official/linked.tar.gz",
            "../outside.tar.gz",
        );
    }
    let encoder = tar.into_inner().expect("tar");
    encoder.finish().expect("gzip")
}

fn append_symlink(tar: &mut Builder<GzEncoder<Vec<u8>>>, name: &str, target: &str) {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Symlink);
    header.set_mode(0o777);
    header.set_size(0);
    header.set_path(name).expect("path");
    header.set_link_name(target).expect("link target");
    header.set_cksum();
    tar.append(&header, std::io::empty()).expect("append link");
}

fn append_file(tar: &mut Builder<GzEncoder<Vec<u8>>>, name: &str, data: &[u8], unsafe_path: bool) {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_mode(0o755);
    header.set_size(u64::try_from(data.len()).expect("size"));
    header.set_path(name).expect("path");
    if unsafe_path {
        let bytes = header.as_mut_bytes();
        bytes[..100].fill(0);
        bytes[..9].copy_from_slice(b"../escape");
    }
    header.set_cksum();
    tar.append(&header, data).expect("append");
}

fn archive_name(version: &str) -> String {
    format!(
        "codex-proxy-rs-{version}-{}-{}.tar.gz",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

async fn mount_release_json(
    server: &MockServer,
    version: &str,
    assets: Vec<serde_json::Value>,
    times: Option<u64>,
) {
    let mock = Mock::given(method("GET"))
        .and(path("/repos/owner/repository/releases"))
        .respond_with(release_response(version, assets));
    match times {
        Some(times) => mock.up_to_n_times(times).mount(server).await,
        None => mock.mount(server).await,
    }
}

fn release_response(version: &str, assets: Vec<serde_json::Value>) -> ResponseTemplate {
    let prerelease = !semver::Version::parse(version)
        .expect("version")
        .pre
        .is_empty();
    let release = serde_json::json!({
        "tag_name": format!("v{version}"),
        "name": format!("Release {version}"),
        "body": "notes",
        "html_url": "https://github.com/owner/repository/releases/latest",
        "prerelease": prerelease,
        "published_at": "2026-07-19T00:00:00Z",
        "assets": assets,
    });
    ResponseTemplate::new(200).set_body_json(serde_json::json!([release]))
}

struct RejectingRestartPreflight;
#[async_trait::async_trait]
impl gateway_admin::ports::system::SystemRestartPreflight for RejectingRestartPreflight {
    async fn prepare(
        &self,
        candidate: Option<SystemUpdateCandidate>,
    ) -> Result<(), SystemOperationError> {
        assert_eq!(candidate.unwrap().target_version, "1.0.0");
        Err(SystemOperationError::new(
            SystemOperationErrorKind::Conflict,
            "plugins need confirmation",
        ))
    }
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
