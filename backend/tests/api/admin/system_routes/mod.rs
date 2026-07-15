use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::CONTENT_TYPE},
};
use codex_proxy_rs::{
    api::{AppState, router::ProcessControl},
    bootstrap::services::Services,
    update::service::{SystemUpdateConfig, SystemUpdateService},
};
use flate2::{Compression, write::GzEncoder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tar::{Builder, EntryType, Header};
use tokio::sync::broadcast;
use tower::util::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use crate::support::{
    admin::seed_admin_session,
    config::test_config,
    http::response_json,
    wire_profile::{test_wire_profile_value, wire_profile},
};

const VERSION_OLD: &str = "0.1.0";
const VERSION_NEW: &str = "0.2.0";

struct TestProcessControl {
    shutdown: broadcast::Sender<()>,
}

impl TestProcessControl {
    fn new() -> Self {
        let (shutdown, _receiver) = broadcast::channel(16);
        Self { shutdown }
    }

    fn signal_shutdown(&self) {
        let _ = self.shutdown.send(());
    }

    fn subscribe_shutdown(&self) -> broadcast::Receiver<()> {
        self.shutdown.subscribe()
    }
}

impl ProcessControl for TestProcessControl {
    fn request_shutdown(&self) {
        self.signal_shutdown();
    }

    fn request_restart(&self, _executable_path: PathBuf) {
        self.signal_shutdown();
    }
}

#[tokio::test]
async fn version_should_return_backend_build_metadata() {
    let repository = "zyycn/codex-proxy-rs-version-route";
    let github = MockServer::start().await;
    mount_latest_release(&github, repository, "0.2.0").await;
    let mut update_config = system_update_config(repository, &github.uri());
    update_config.git_sha = "version-test-sha".to_string();
    update_config.build_time = "2026-07-01T00:00:00Z".to_string();
    let (app, _dir) = admin_system_test_app("system-version", update_config).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/system/version")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;

    assert!(
        body["data"]["version"] == VERSION_OLD
            && body["data"]["gitSha"] == "version-test-sha"
            && body["data"]["buildTime"] == "2026-07-01T00:00:00Z"
            && body["data"]["deploymentMode"] == "docker"
            && body["data"]["deploymentModeLabel"] == "Docker"
            && body["data"]["updateChannel"] == "stable"
            && body["data"]["latestVersion"] == VERSION_NEW
            && body["data"]["hasUpdate"] == true
            && body["data"]["updateCached"] == false
            && body["data"]["updateWarning"].is_null()
    );
}

#[tokio::test]
async fn update_detail_should_use_cached_release_when_not_refreshed() {
    let repository = "zyycn/codex-proxy-rs-cache-route";
    let github = MockServer::start().await;
    mount_latest_release(&github, repository, "0.2.0").await;
    let update_config = system_update_config(repository, &github.uri());
    let (app, _dir) = admin_system_test_app("system-cache", update_config).await;
    let initial = get_update_detail(&app, true, "req_system_cache_initial").await;

    github.reset().await;
    let cached = get_update_detail(&app, false, "req_system_cache_cached").await;

    assert!(
        initial["hasUpdate"] == true
            && cached["cached"] == true
            && cached["latestVersion"] == "0.2.0"
    );
}

#[tokio::test]
async fn update_detail_should_fallback_to_cache_when_refresh_fetch_fails() {
    let repository = "zyycn/codex-proxy-rs-cache-forced-route";
    let github = MockServer::start().await;
    mount_latest_release(&github, repository, "0.3.0").await;
    let update_config = system_update_config(repository, &github.uri());
    let (app, _dir) = admin_system_test_app("system-cache-forced", update_config).await;
    let initial = get_update_detail(&app, true, "req_system_cache_forced_initial").await;

    github.reset().await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/{repository}/releases/latest")))
        .respond_with(ResponseTemplate::new(500))
        .mount(&github)
        .await;
    let fallback = get_update_detail(&app, true, "req_system_cache_forced_fallback").await;

    assert!(
        initial["hasUpdate"] == true
            && fallback["cached"] == true
            && fallback["latestVersion"] == "0.3.0"
            && fallback["warning"].is_null()
    );
}

#[tokio::test]
async fn update_detail_should_report_source_build_as_unsupported() {
    let repository = "zyycn/codex-proxy-rs-source-route";
    let github = MockServer::start().await;
    mount_latest_release(&github, repository, "0.2.0").await;
    let mut update_config = system_update_config(repository, &github.uri());
    update_config.build_type = "source".to_string();
    let (app, _dir) = admin_system_test_app("system-source-build", update_config).await;

    let info = get_update_detail(&app, true, "req_system_source_build").await;

    assert!(
        info["currentVersion"] == VERSION_OLD
            && info["latestVersion"] == VERSION_NEW
            && info["hasUpdate"] == true
            && info["buildType"] == "source"
            && info["buildTypeLabel"] == "源码构建"
            && info["updateSupported"] == false
            && info["unsupportedReason"] == "一键更新需要正式构建包"
    );
}

#[tokio::test]
async fn update_detail_should_reject_untrusted_github_api_base() {
    let update_config = system_update_config(
        "zyycn/codex-proxy-rs-untrusted-api-base-route",
        "https://mirror.example",
    );
    let (app, _dir) = admin_system_test_app("system-untrusted-api-base", update_config).await;

    let info = get_update_detail(&app, true, "req_system_untrusted_api_base").await;

    assert!(
        info["hasUpdate"] == false
            && info["updateSupported"] == false
            && info["unsupportedReason"] == "GitHub API base must be https://api.github.com/repos"
    );
}

#[tokio::test]
async fn restart_should_request_process_restart_inside_docker() {
    let mut update_config =
        system_update_config("zyycn/codex-proxy-rs-restart-route", "http://127.0.0.1:9");
    update_config.self_restart_enabled = true;
    let (app, _dir, process_control) =
        admin_system_test_app_with_process_control("system-restart", update_config).await;
    let mut shutdown = process_control.subscribe_shutdown();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/system/restart")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_restart")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let shutdown = tokio::time::timeout(Duration::from_secs(2), shutdown.recv()).await;

    assert!(
        body["data"]["operationId"]
            .as_str()
            .is_some_and(|id| id.starts_with("sysop-restart-"))
            && body["data"]["message"] == "已安排进程内重启"
            && shutdown.is_ok()
    );
}

#[tokio::test]
async fn restart_should_spawn_replacement_before_shutdown_outside_docker() {
    let replacement = Path::new("/bin/true");
    if !replacement.exists() {
        return;
    }
    let mut update_config = system_update_config(
        "zyycn/codex-proxy-rs-binary-restart-route",
        "http://127.0.0.1:9",
    );
    update_config.deployment_mode = "binary".to_string();
    update_config.self_restart_enabled = true;
    update_config.executable_path = Some(replacement.to_path_buf());
    let (app, _dir, process_control) =
        admin_system_test_app_with_process_control("system-binary-restart", update_config).await;
    let mut shutdown = process_control.subscribe_shutdown();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/system/restart")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_binary_restart")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let shutdown = tokio::time::timeout(Duration::from_secs(2), shutdown.recv()).await;

    assert!(body["data"]["message"] == "已安排自重启" && shutdown.is_ok());
}

#[tokio::test]
async fn restart_should_not_shutdown_when_replacement_spawn_fails() {
    let mut update_config = system_update_config(
        "zyycn/codex-proxy-rs-restart-spawn-failure-route",
        "http://127.0.0.1:9",
    );
    update_config.deployment_mode = "binary".to_string();
    update_config.self_restart_enabled = true;
    update_config.executable_path =
        Some(PathBuf::from("/tmp/codex-proxy-rs-missing-restart-binary"));
    let (app, _dir, process_control) =
        admin_system_test_app_with_process_control("system-restart-spawn-failure", update_config)
            .await;
    let mut shutdown = process_control.subscribe_shutdown();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/system/restart")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_restart_spawn_failure")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;
    let shutdown = tokio::time::timeout(Duration::from_millis(100), shutdown.recv()).await;
    let message = body["message"].as_str().unwrap_or_default();

    assert!(
        status == StatusCode::INTERNAL_SERVER_ERROR
            && message.starts_with("Failed to schedule replacement process:")
            && shutdown.is_err()
    );
}

#[tokio::test]
async fn update_should_replace_local_release_files_with_latest_asset() {
    let repository = "zyycn/codex-proxy-rs-update-route";
    let github = MockServer::start().await;
    let deploy = tempfile::tempdir().unwrap();
    let exe_path = deploy.path().join("codex-proxy-rs");
    let web_dist = deploy.path().join("web/dist");
    std::fs::create_dir_all(&web_dist).unwrap();
    std::fs::write(&exe_path, "old-binary").unwrap();
    std::fs::write(web_dist.join("index.html"), "old-web").unwrap();
    mount_latest_release_with_archive(&github, repository, "0.4.0", "new-binary", "new-web").await;
    let mut update_config = system_update_config(repository, &github.uri());
    configure_system_update_paths(&mut update_config, deploy.path(), &exe_path, &web_dist);
    let (app, _dir) = admin_system_test_app("system-update", update_config).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/system/update")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_update_latest")
                .header(CONTENT_TYPE, "application/json")
                .body(confirmed_update_body("0.4.0"))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let status = wait_for_operation_status(&app, "succeeded").await;

    assert!(
        body["data"]["operationId"]
            .as_str()
            .is_some_and(|id| id.starts_with("sysop-update-"))
            && body["data"]["targetVersion"] == "0.4.0"
            && body["data"]["needRestart"] == true
            && status["operation"]["status"] == "succeeded"
            && status["currentVersion"] == "0.4.0"
            && std::fs::read_to_string(&exe_path).unwrap() == "new-binary"
            && std::fs::read_to_string(web_dist.join("index.html")).unwrap() == "new-web"
            && std::fs::read_to_string(deploy.path().join("codex-proxy-rs.backup")).unwrap()
                == "old-binary"
            && std::fs::read_to_string(deploy.path().join("web/dist.backup/index.html")).unwrap()
                == "old-web"
    );
}

#[tokio::test]
async fn update_should_reject_when_confirmed_target_differs_from_remote_latest() {
    let repository = "zyycn/codex-proxy-rs-update-target-changed-route";
    let github = MockServer::start().await;
    let deploy = tempfile::tempdir().unwrap();
    let exe_path = deploy.path().join("codex-proxy-rs");
    let web_dist = deploy.path().join("web/dist");
    std::fs::create_dir_all(&web_dist).unwrap();
    std::fs::write(&exe_path, "old-binary").unwrap();
    std::fs::write(web_dist.join("index.html"), "old-web").unwrap();
    mount_latest_release(&github, repository, "0.4.0").await;
    let mut update_config = system_update_config(repository, &github.uri());
    configure_system_update_paths(&mut update_config, deploy.path(), &exe_path, &web_dist);
    let (app, _dir) = admin_system_test_app("system-update-target-changed", update_config).await;
    let initial = get_update_detail(&app, true, "req_system_update_target_initial").await;
    github.reset().await;
    mount_latest_release(&github, repository, "0.5.0").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/system/update")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_update_target_changed")
                .header(CONTENT_TYPE, "application/json")
                .body(confirmed_update_body("0.4.0"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert!(
        initial["latestVersion"] == "0.4.0"
            && status == StatusCode::CONFLICT
            && body["message"] == "远端最新版本已变更为 v0.5.0，请重新检查并确认"
            && std::fs::read_to_string(&exe_path).unwrap() == "old-binary"
            && std::fs::read_to_string(web_dist.join("index.html")).unwrap() == "old-web"
    );
}

#[tokio::test]
async fn update_should_reject_untrusted_github_api_base() {
    let deploy = tempfile::tempdir().unwrap();
    let exe_path = deploy.path().join("codex-proxy-rs");
    let web_dist = deploy.path().join("web/dist");
    std::fs::create_dir_all(&web_dist).unwrap();
    std::fs::write(&exe_path, "old-binary").unwrap();
    std::fs::write(web_dist.join("index.html"), "old-web").unwrap();
    let mut update_config = system_update_config(
        "zyycn/codex-proxy-rs-update-untrusted-api-base-route",
        "https://mirror.example",
    );
    configure_system_update_paths(&mut update_config, deploy.path(), &exe_path, &web_dist);
    let (app, _dir) =
        admin_system_test_app("system-update-untrusted-api-base", update_config).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/system/update")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_update_untrusted_api_base")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response_json(response).await;

    assert!(
        status == StatusCode::CONFLICT
            && body["message"] == "GitHub API base must be https://api.github.com/repos"
            && std::fs::read_to_string(&exe_path).unwrap() == "old-binary"
            && std::fs::read_to_string(web_dist.join("index.html")).unwrap() == "old-web"
    );
}

#[tokio::test]
async fn update_should_fail_when_release_checksum_is_missing() {
    let repository = "zyycn/codex-proxy-rs-missing-checksum-route";
    let github = MockServer::start().await;
    let deploy = tempfile::tempdir().unwrap();
    let exe_path = deploy.path().join("codex-proxy-rs");
    let web_dist = deploy.path().join("web/dist");
    std::fs::create_dir_all(&web_dist).unwrap();
    std::fs::write(&exe_path, "old-binary").unwrap();
    std::fs::write(web_dist.join("index.html"), "old-web").unwrap();
    mount_latest_release_with_archive_without_checksum(
        &github,
        repository,
        "0.4.0",
        "new-binary",
        "new-web",
    )
    .await;
    let mut update_config = system_update_config(repository, &github.uri());
    configure_system_update_paths(&mut update_config, deploy.path(), &exe_path, &web_dist);
    let (app, _dir) = admin_system_test_app("system-update-missing-checksum", update_config).await;

    assert_update_failure_preserves_files(
        &app,
        "req_system_update_missing_checksum",
        StatusCode::BAD_GATEWAY,
        "Release checksums.txt is required",
        &exe_path,
        &web_dist,
    )
    .await;
}

#[tokio::test]
async fn update_should_fail_when_release_checksum_mismatches() {
    let repository = "zyycn/codex-proxy-rs-bad-checksum-route";
    let github = MockServer::start().await;
    let deploy = tempfile::tempdir().unwrap();
    let exe_path = deploy.path().join("codex-proxy-rs");
    let web_dist = deploy.path().join("web/dist");
    std::fs::create_dir_all(&web_dist).unwrap();
    std::fs::write(&exe_path, "old-binary").unwrap();
    std::fs::write(web_dist.join("index.html"), "old-web").unwrap();
    mount_latest_release_with_archive_bad_checksum(
        &github,
        repository,
        "0.4.0",
        "new-binary",
        "new-web",
    )
    .await;
    let mut update_config = system_update_config(repository, &github.uri());
    configure_system_update_paths(&mut update_config, deploy.path(), &exe_path, &web_dist);
    let (app, _dir) = admin_system_test_app("system-update-bad-checksum", update_config).await;

    assert_update_failure_preserves_files(
        &app,
        "req_system_update_bad_checksum",
        StatusCode::BAD_GATEWAY,
        "Checksum mismatch",
        &exe_path,
        &web_dist,
    )
    .await;
}

#[tokio::test]
async fn update_should_reject_release_archive_from_untrusted_host() {
    let repository = "zyycn/codex-proxy-rs-untrusted-download-route";
    let github = MockServer::start().await;
    let deploy = tempfile::tempdir().unwrap();
    let exe_path = deploy.path().join("codex-proxy-rs");
    let web_dist = deploy.path().join("web/dist");
    std::fs::create_dir_all(&web_dist).unwrap();
    std::fs::write(&exe_path, "old-binary").unwrap();
    std::fs::write(web_dist.join("index.html"), "old-web").unwrap();
    mount_latest_release_with_archive_urls(
        &github,
        repository,
        "0.4.0",
        "https://evil.example/download/codex-proxy-rs.tar.gz",
        "https://github.com/zyycn/codex-proxy-rs/releases/download/v0.4.0/checksums.txt",
    )
    .await;
    let mut update_config = system_update_config(repository, &github.uri());
    configure_system_update_paths(&mut update_config, deploy.path(), &exe_path, &web_dist);
    let (app, _dir) =
        admin_system_test_app("system-update-untrusted-download", update_config).await;

    assert_update_failure_preserves_files(
        &app,
        "req_system_update_untrusted_download",
        StatusCode::BAD_REQUEST,
        "Download host is not allowed: evil.example",
        &exe_path,
        &web_dist,
    )
    .await;
}

#[tokio::test]
async fn update_should_reject_insecure_release_archive() {
    let repository = "zyycn/codex-proxy-rs-insecure-download-route";
    let github = MockServer::start().await;
    let deploy = tempfile::tempdir().unwrap();
    let exe_path = deploy.path().join("codex-proxy-rs");
    let web_dist = deploy.path().join("web/dist");
    std::fs::create_dir_all(&web_dist).unwrap();
    std::fs::write(&exe_path, "old-binary").unwrap();
    std::fs::write(web_dist.join("index.html"), "old-web").unwrap();
    mount_latest_release_with_archive_urls(
        &github,
        repository,
        "0.4.0",
        "http://github.com/zyycn/codex-proxy-rs/releases/download/v0.4.0/codex-proxy-rs.tar.gz",
        "https://github.com/zyycn/codex-proxy-rs/releases/download/v0.4.0/checksums.txt",
    )
    .await;
    let mut update_config = system_update_config(repository, &github.uri());
    configure_system_update_paths(&mut update_config, deploy.path(), &exe_path, &web_dist);
    let (app, _dir) = admin_system_test_app("system-update-insecure-download", update_config).await;

    assert_update_failure_preserves_files(
        &app,
        "req_system_update_insecure_download",
        StatusCode::BAD_REQUEST,
        "Only HTTPS release downloads are allowed",
        &exe_path,
        &web_dist,
    )
    .await;
}

#[tokio::test]
async fn update_should_reject_release_archive_with_unsafe_path() {
    let repository = "zyycn/codex-proxy-rs-unsafe-archive-route";
    let github = MockServer::start().await;
    let deploy = tempfile::tempdir().unwrap();
    let exe_path = deploy.path().join("codex-proxy-rs");
    let web_dist = deploy.path().join("web/dist");
    std::fs::create_dir_all(&web_dist).unwrap();
    std::fs::write(&exe_path, "old-binary").unwrap();
    std::fs::write(web_dist.join("index.html"), "old-web").unwrap();
    mount_latest_release_with_unsafe_archive(&github, repository, "0.4.0", "new-binary", "new-web")
        .await;
    let mut update_config = system_update_config(repository, &github.uri());
    configure_system_update_paths(&mut update_config, deploy.path(), &exe_path, &web_dist);
    let (app, _dir) = admin_system_test_app("system-update-unsafe-archive", update_config).await;

    assert_update_failure_preserves_files(
        &app,
        "req_system_update_unsafe_archive",
        StatusCode::BAD_REQUEST,
        "Unsafe archive path",
        &exe_path,
        &web_dist,
    )
    .await;
}

#[tokio::test]
async fn update_should_restore_web_assets_when_binary_backup_fails() {
    let repository = "zyycn/codex-proxy-rs-binary-backup-failure-route";
    let github = MockServer::start().await;
    let deploy = tempfile::tempdir().unwrap();
    let exe_path = deploy.path().join("codex-proxy-rs");
    let web_dist = deploy.path().join("web/dist");
    std::fs::create_dir_all(&web_dist).unwrap();
    std::fs::write(&exe_path, "old-binary").unwrap();
    std::fs::write(web_dist.join("index.html"), "old-web").unwrap();
    std::fs::create_dir(deploy.path().join("codex-proxy-rs.backup")).unwrap();
    mount_latest_release_with_archive(&github, repository, "0.4.0", "new-binary", "new-web").await;
    let mut update_config = system_update_config(repository, &github.uri());
    configure_system_update_paths(&mut update_config, deploy.path(), &exe_path, &web_dist);
    let (app, _dir) =
        admin_system_test_app("system-update-binary-backup-failure", update_config).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/system/update")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_update_binary_backup_failure")
                .header(CONTENT_TYPE, "application/json")
                .body(confirmed_update_body("0.4.0"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status_code = response.status();
    let body = response_json(response).await;
    let status = wait_for_operation_status(&app, "failed").await;
    let message = body["message"].as_str().unwrap_or_default();
    let state_error = status["operation"]["error"].as_str().unwrap_or_default();

    assert!(
        status_code == StatusCode::INTERNAL_SERVER_ERROR
            && message.starts_with("Failed to remove old binary backup:")
            && state_error.starts_with("Failed to remove old binary backup:")
            && std::fs::read_to_string(&exe_path).unwrap() == "old-binary"
            && std::fs::read_to_string(web_dist.join("index.html")).unwrap() == "old-web"
            && !deploy.path().join("web/dist.backup").exists(),
        "status={status_code}, message={message}, state_error={state_error}, exe={}, web={}, web_backup_exists={}",
        std::fs::read_to_string(&exe_path).unwrap_or_default(),
        std::fs::read_to_string(web_dist.join("index.html")).unwrap_or_default(),
        deploy.path().join("web/dist.backup").exists()
    );
}

#[tokio::test]
async fn update_should_remove_stale_file_lock_and_continue() {
    let repository = "zyycn/codex-proxy-rs-stale-lock-route";
    let github = MockServer::start().await;
    let deploy = tempfile::tempdir().unwrap();
    let exe_path = deploy.path().join("codex-proxy-rs");
    let web_dist = deploy.path().join("web/dist");
    let lock_path = deploy.path().join(".runtime/update.lock");
    std::fs::create_dir_all(&web_dist).unwrap();
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    std::fs::write(&exe_path, "old-binary").unwrap();
    std::fs::write(web_dist.join("index.html"), "old-web").unwrap();
    std::fs::write(&lock_path, "pid=1\ncreated_at=stale\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&lock_path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(31 * 60))
        .unwrap();
    mount_latest_release_with_archive(&github, repository, "0.4.0", "new-binary", "new-web").await;
    let mut update_config = system_update_config(repository, &github.uri());
    configure_system_update_paths(&mut update_config, deploy.path(), &exe_path, &web_dist);
    let (app, _dir) = admin_system_test_app("system-update-stale-lock", update_config).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/system/update")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_update_stale_lock")
                .header(CONTENT_TYPE, "application/json")
                .body(confirmed_update_body("0.4.0"))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let status = wait_for_operation_status(&app, "succeeded").await;

    assert!(
        body["data"]["targetVersion"] == "0.4.0"
            && status["operation"]["status"] == "succeeded"
            && std::fs::read_to_string(&exe_path).unwrap() == "new-binary"
            && std::fs::read_to_string(web_dist.join("index.html")).unwrap() == "new-web"
            && !lock_path.exists()
    );
}

#[tokio::test]
async fn update_should_replace_web_assets_across_filesystems() {
    let Ok(web_root) = tempfile::tempdir_in("/dev/shm") else {
        return;
    };
    let repository = "zyycn/codex-proxy-rs-cross-device-update-route";
    let github = MockServer::start().await;
    let deploy = tempfile::tempdir().unwrap();
    let exe_path = deploy.path().join("codex-proxy-rs");
    let web_dist = web_root.path().join("web/dist");
    std::fs::create_dir_all(&web_dist).unwrap();
    std::fs::write(&exe_path, "old-binary").unwrap();
    std::fs::write(web_dist.join("index.html"), "old-web").unwrap();
    mount_latest_release_with_archive(&github, repository, "0.4.0", "new-binary", "new-web").await;
    let mut update_config = system_update_config(repository, &github.uri());
    configure_system_update_paths(&mut update_config, deploy.path(), &exe_path, &web_dist);
    let (app, _dir) = admin_system_test_app("system-cross-device-update", update_config).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/system/update")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_cross_device_update")
                .header(CONTENT_TYPE, "application/json")
                .body(confirmed_update_body("0.4.0"))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;
    let status = wait_for_operation_status(&app, "succeeded").await;

    assert!(
        body["data"]["targetVersion"] == "0.4.0"
            && status["operation"]["status"] == "succeeded"
            && std::fs::read_to_string(&exe_path).unwrap() == "new-binary"
            && std::fs::read_to_string(web_dist.join("index.html")).unwrap() == "new-web"
            && std::fs::read_to_string(web_root.path().join("web/dist.backup/index.html")).unwrap()
                == "old-web"
    );
}

#[tokio::test]
async fn update_status_should_read_local_update_state() {
    let deploy = tempfile::tempdir().unwrap();
    let mut update_config =
        system_update_config("zyycn/codex-proxy-rs-status-route", "http://127.0.0.1:9");
    configure_system_update_paths(
        &mut update_config,
        deploy.path(),
        &deploy.path().join("codex-proxy-rs"),
        &deploy.path().join("web/dist"),
    );
    std::fs::create_dir_all(deploy.path().join(".runtime")).unwrap();
    std::fs::write(
        deploy.path().join(".runtime/update-state.json"),
        json!({
            "previousVersion": VERSION_OLD,
            "currentVersion": VERSION_NEW,
            "operation": {
                "operationId": "system-update-status",
                "kind": "update",
                "status": "running",
                "targetVersion": VERSION_NEW,
                "message": "operation running",
                "error": null,
                "startedAt": "2026-07-01T00:00:00Z",
                "finishedAt": null
            }
        })
        .to_string(),
    )
    .unwrap();
    let (app, _dir) = admin_system_test_app("system-status", update_config).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/system/update-status")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_update_status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = response_json(response).await;

    assert!(
        body["data"]["operation"]["operationId"] == "system-update-status"
            && body["data"]["operation"]["status"] == "running"
            && body["data"]["currentVersion"] == VERSION_NEW
    );
}

#[tokio::test]
async fn update_events_should_open_authenticated_sse_stream() {
    let deploy = tempfile::tempdir().unwrap();
    let mut update_config =
        system_update_config("zyycn/codex-proxy-rs-events-route", "http://127.0.0.1:9");
    configure_system_update_paths(
        &mut update_config,
        deploy.path(),
        &deploy.path().join("codex-proxy-rs"),
        &deploy.path().join("web/dist"),
    );
    let (app, _dir) = admin_system_test_app("system-events", update_config).await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/system/update-events")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_update_events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    assert!(response.status() == StatusCode::OK && content_type.starts_with("text/event-stream"));
}

#[tokio::test]
async fn update_events_should_close_after_terminal_update_log() {
    let repository = "zyycn/codex-proxy-rs-events-terminal-route";
    let github = MockServer::start().await;
    let deploy = tempfile::tempdir().unwrap();
    let exe_path = deploy.path().join("codex-proxy-rs");
    let web_dist = deploy.path().join("web/dist");
    std::fs::create_dir_all(&web_dist).unwrap();
    std::fs::write(&exe_path, "old-binary").unwrap();
    std::fs::write(web_dist.join("index.html"), "old-web").unwrap();
    mount_latest_release_with_archive(&github, repository, "0.4.0", "new-binary", "new-web").await;
    let mut update_config = system_update_config(repository, &github.uri());
    configure_system_update_paths(&mut update_config, deploy.path(), &exe_path, &web_dist);
    let (app, _dir) = admin_system_test_app("system-events-terminal", update_config).await;

    let sse_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/system/update-events")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_update_events_terminal")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let update_app = app.clone();
    let update_task = tokio::spawn(async move {
        update_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/system/update")
                    .header("cookie", "cpr_admin_session=session_1")
                    .header("x-request-id", "req_system_update_events_terminal_update")
                    .header(CONTENT_TYPE, "application/json")
                    .body(confirmed_update_body("0.4.0"))
                    .unwrap(),
            )
            .await
            .unwrap()
    });

    let body = tokio::time::timeout(
        Duration::from_secs(2),
        to_bytes(sse_response.into_body(), 128 * 1024),
    )
    .await
    .expect("update event stream should end after terminal update log")
    .unwrap();
    let update_response = update_task.await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();

    assert!(
        update_response.status() == StatusCode::OK
            && text.contains("\"step\":\"done\"")
            && text.contains("\"terminal\":true")
            && text.contains("更新文件已替换，等待服务重启生效")
    );
}

async fn get_update_detail(app: &axum::Router, refresh: bool, request_id: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/admin/system/update-detail?refresh={refresh}"))
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", request_id)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response_json(response).await["data"].clone()
}

fn confirmed_update_body(version: &str) -> Body {
    Body::from(json!({ "targetVersion": version }).to_string())
}

async fn assert_update_failure_preserves_files(
    app: &axum::Router,
    request_id: &str,
    expected_status: StatusCode,
    expected_message: &str,
    exe_path: &Path,
    web_dist: &Path,
) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/system/update")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", request_id)
                .header(CONTENT_TYPE, "application/json")
                .body(confirmed_update_body("0.4.0"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status_code = response.status();
    let body = response_json(response).await;
    let status = wait_for_operation_status(app, "failed").await;

    assert!(
        status_code == expected_status
            && body["message"] == expected_message
            && status["operation"]["error"] == expected_message
            && std::fs::read_to_string(exe_path).unwrap() == "old-binary"
            && std::fs::read_to_string(web_dist.join("index.html")).unwrap() == "old-web"
    );
}

async fn mount_latest_release(server: &MockServer, repository: &str, version: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/repos/{repository}/releases/latest")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": format!("v{version}"),
            "name": format!("v{version}"),
            "body": "## Changes\n- Update package",
            "html_url": format!("https://github.com/{repository}/releases/tag/v{version}"),
            "prerelease": false,
            "assets": []
        })))
        .mount(server)
        .await;
}

async fn mount_latest_release_with_archive(
    server: &MockServer,
    repository: &str,
    version: &str,
    binary: &str,
    index: &str,
) {
    let archive_name = format!(
        "codex-proxy-rs_{version}_{}_{}.tar.gz",
        std::env::consts::OS,
        release_arch()
    );
    let archive = release_archive(binary, index);
    let checksum = format!(
        "{}  {archive_name}\n",
        hex::encode(Sha256::digest(&archive))
    );
    mount_latest_release_archive(
        server,
        repository,
        version,
        archive_name,
        archive,
        Some(checksum),
    )
    .await;
}

async fn mount_latest_release_with_archive_without_checksum(
    server: &MockServer,
    repository: &str,
    version: &str,
    binary: &str,
    index: &str,
) {
    let archive_name = format!(
        "codex-proxy-rs_{version}_{}_{}.tar.gz",
        std::env::consts::OS,
        release_arch()
    );
    let archive = release_archive(binary, index);
    mount_latest_release_archive(server, repository, version, archive_name, archive, None).await;
}

async fn mount_latest_release_with_archive_bad_checksum(
    server: &MockServer,
    repository: &str,
    version: &str,
    binary: &str,
    index: &str,
) {
    let archive_name = format!(
        "codex-proxy-rs_{version}_{}_{}.tar.gz",
        std::env::consts::OS,
        release_arch()
    );
    let archive = release_archive(binary, index);
    let checksum = format!(
        "{}  {archive_name}\n",
        "0".repeat(Sha256::output_size() * 2)
    );
    mount_latest_release_archive(
        server,
        repository,
        version,
        archive_name,
        archive,
        Some(checksum),
    )
    .await;
}

async fn mount_latest_release_with_archive_urls(
    server: &MockServer,
    repository: &str,
    version: &str,
    archive_url: &str,
    checksum_url: &str,
) {
    let archive_name = format!(
        "codex-proxy-rs_{version}_{}_{}.tar.gz",
        std::env::consts::OS,
        release_arch()
    );
    Mock::given(method("GET"))
        .and(path(format!("/repos/{repository}/releases/latest")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": format!("v{version}"),
            "name": format!("v{version}"),
            "body": "## Changes\n- Update package",
            "html_url": format!("https://github.com/{repository}/releases/tag/v{version}"),
            "prerelease": false,
            "assets": [
                {
                    "name": archive_name,
                    "browser_download_url": archive_url,
                    "size": 128
                },
                {
                    "name": "checksums.txt",
                    "browser_download_url": checksum_url,
                    "size": 128
                }
            ]
        })))
        .mount(server)
        .await;
}

async fn mount_latest_release_with_unsafe_archive(
    server: &MockServer,
    repository: &str,
    version: &str,
    binary: &str,
    index: &str,
) {
    let archive_name = format!(
        "codex-proxy-rs_{version}_{}_{}.tar.gz",
        std::env::consts::OS,
        release_arch()
    );
    let archive = release_archive_with_extra_file(binary, index, "../escape", b"unsafe");
    let checksum = format!(
        "{}  {archive_name}\n",
        hex::encode(Sha256::digest(&archive))
    );
    mount_latest_release_archive(
        server,
        repository,
        version,
        archive_name,
        archive,
        Some(checksum),
    )
    .await;
}

async fn mount_latest_release_archive(
    server: &MockServer,
    repository: &str,
    version: &str,
    archive_name: String,
    archive: Vec<u8>,
    checksum: Option<String>,
) {
    let mut assets = vec![json!({
        "name": archive_name,
        "browser_download_url": format!("{}/download/{archive_name}", server.uri()),
        "size": archive.len()
    })];
    if let Some(checksum) = checksum.as_ref() {
        assets.push(json!({
            "name": "checksums.txt",
            "browser_download_url": format!("{}/download/checksums.txt", server.uri()),
            "size": checksum.len()
        }));
    }

    Mock::given(method("GET"))
        .and(path(format!("/repos/{repository}/releases/latest")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": format!("v{version}"),
            "name": format!("v{version}"),
            "body": "## Changes\n- Update package",
            "html_url": format!("https://github.com/{repository}/releases/tag/v{version}"),
            "prerelease": false,
            "assets": assets
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/download/{archive_name}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
        .mount(server)
        .await;
    if let Some(checksum) = checksum {
        Mock::given(method("GET"))
            .and(path("/download/checksums.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_string(checksum))
            .mount(server)
            .await;
    }
}

fn release_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        arch => arch,
    }
}

fn release_archive(binary: &str, index: &str) -> Vec<u8> {
    release_archive_with_extra_file(binary, index, "", &[])
}

fn release_archive_with_extra_file(
    binary: &str,
    index: &str,
    extra_path: &str,
    extra_bytes: &[u8],
) -> Vec<u8> {
    let mut data = Vec::new();
    {
        let encoder = GzEncoder::new(&mut data, Compression::default());
        let mut builder = Builder::new(encoder);
        append_tar_file(&mut builder, "codex-proxy-rs", binary.as_bytes());
        append_tar_file(&mut builder, "web/dist/index.html", index.as_bytes());
        if !extra_path.is_empty() {
            append_raw_tar_file(&mut builder, extra_path, extra_bytes);
        }
        builder.finish().unwrap();
    }
    data
}

fn append_tar_file<W: Write>(builder: &mut Builder<W>, path: &str, bytes: &[u8]) {
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    builder.append_data(&mut header, path, bytes).unwrap();
}

fn append_raw_tar_file<W: Write>(builder: &mut Builder<W>, path: &str, bytes: &[u8]) {
    let mut header = Header::new_old();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(EntryType::Regular);
    let name = path.as_bytes();
    header.as_old_mut().name[..name.len()].copy_from_slice(name);
    header.set_cksum();
    builder.append(&header, bytes).unwrap();
}

fn system_update_config(repository: &str, github_api_base: &str) -> SystemUpdateConfig {
    SystemUpdateConfig {
        version: VERSION_OLD.to_string(),
        git_sha: "test-git-sha".to_string(),
        build_time: "2026-07-01T00:00:00Z".to_string(),
        deployment_mode: "docker".to_string(),
        build_type: "release".to_string(),
        update_channel: "stable".to_string(),
        update_repository: Some(repository.to_string()),
        github_api_base: format!("{github_api_base}/repos"),
        executable_path: Some(PathBuf::from("/app/bin/codex-proxy-rs")),
        web_dist_dir: PathBuf::from("/app/web/dist"),
        update_state_file: PathBuf::from("/app/.runtime/data/update-state.json"),
        update_lock_file: PathBuf::from("/app/.runtime/data/update-state.lock"),
        update_temp_dir: PathBuf::from("/app/.runtime/data/update-tmp"),
        self_restart_enabled: false,
    }
}

fn configure_system_update_paths(
    config: &mut SystemUpdateConfig,
    root: &Path,
    exe_path: &Path,
    web_dist: &Path,
) {
    config.executable_path = Some(exe_path.to_path_buf());
    config.web_dist_dir = web_dist.to_path_buf();
    config.update_state_file = root.join(".runtime/update-state.json");
    config.update_lock_file = root.join(".runtime/update.lock");
    config.update_temp_dir = root.join(".runtime/update-tmp");
}

async fn wait_for_operation_status(app: &axum::Router, expected: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/admin/system/update-status")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_update_status_poll")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status_code = response.status();
    let body = response_json(response).await;
    assert_eq!(status_code, StatusCode::OK, "body={body}");
    let data = body["data"].clone();
    assert_eq!(
        data["operation"]["status"], expected,
        "update status response={body}"
    );
    data
}

async fn admin_system_test_app(
    db_name: &str,
    update_config: SystemUpdateConfig,
) -> (axum::Router, crate::support::storage::TestDatabaseGuard) {
    let (app, dir, _process_control) =
        admin_system_test_app_with_process_control(db_name, update_config).await;
    (app, dir)
}

async fn admin_system_test_app_with_process_control(
    db_name: &str,
    update_config: SystemUpdateConfig,
) -> (
    axum::Router,
    crate::support::storage::TestDatabaseGuard,
    Arc<TestProcessControl>,
) {
    let (pool, dir) = crate::support::storage::init_test_db(db_name).await;
    let redis = crate::support::storage::create_test_redis(db_name).await;
    seed_admin_session(&pool, &redis, "session_1").await;
    let config = test_config(crate::support::storage::test_database_url());
    let stores = crate::support::storage::background_task_stores(pool, redis);
    let services = Arc::new(Services::new(
        &config,
        stores,
        wire_profile(test_wire_profile_value()),
    ));
    let mut state = AppState::from(services.as_ref());
    state.services.system_update = Arc::new(SystemUpdateService::new(update_config));
    let process_control = Arc::new(TestProcessControl::new());
    state.services.process_control = process_control.clone();
    let app = codex_proxy_rs::api::router::router().with_state(state);
    (app, dir, process_control)
}
