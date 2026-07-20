use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use filetime::FileTime;
use flate2::{Compression, write::GzEncoder};
use futures::StreamExt as _;
use gateway_admin::model::system::{
    SystemOperationKind, SystemOperationStatus, SystemUpdateEventLevel,
};
use gateway_admin::ports::system::{SystemOperationErrorKind, SystemOperations};
use gateway_core::engine::CancellationToken;
use gateway_host::system_update::{ProcessSystemOperations, SystemUpdateConfig};
use sha2::{Digest as _, Sha256};
use tar::{Builder, EntryType, Header};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TARGET_VERSION: &str = "9.9.9";

#[tokio::test]
async fn restart_should_not_shutdown_when_replacement_spawn_fails() {
    let fixture = Fixture::new();
    let mut config = fixture.config("http://127.0.0.1:1/repos");
    config.self_restart_enabled = true;
    config.executable_path = Some(fixture.root.path().join("missing"));
    let shutdown = CancellationToken::new();
    let service = ProcessSystemOperations::new(shutdown.clone(), config);

    assert!(service.restart().await.is_err());
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
    service.restart().await.expect("restart accepted");

    tokio::time::timeout(Duration::from_secs(2), shutdown.cancelled())
        .await
        .expect("shutdown requested");
}

#[tokio::test]
async fn restart_should_spawn_replacement_before_shutdown_outside_docker() {
    let fixture = Fixture::new();
    fixture.write_executable("#!/bin/sh\nexit 0\n");
    let mut config = fixture.config("http://127.0.0.1:1/repos");
    config.self_restart_enabled = true;
    let shutdown = CancellationToken::new();
    let service = ProcessSystemOperations::new(shutdown.clone(), config);

    assert_eq!(
        service
            .restart()
            .await
            .expect("replacement scheduled")
            .kind(),
        SystemOperationKind::Restart
    );
    tokio::time::timeout(Duration::from_secs(2), shutdown.cancelled())
        .await
        .expect("shutdown requested after spawn");
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
    service
        .perform_update(Some(TARGET_VERSION.to_owned()))
        .await
        .expect("update");
    service.rollback().await.expect("rollback");

    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"old-binary"
    );
    assert_eq!(
        fs::read(fixture.web().join("index.html")).expect("web"),
        b"old-web"
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
async fn update_detail_should_fallback_to_cache_when_refresh_fetch_fails() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture.mount_release_once(&server, TARGET_VERSION).await;
    let service = fixture.service(&server);
    service.update_detail(true).await.expect("prime cache");

    assert_eq!(
        service
            .update_detail(true)
            .await
            .expect("cached fallback")
            .latest_version,
        TARGET_VERSION
    );
}

#[tokio::test]
async fn update_detail_should_reject_untrusted_github_api_base() {
    let fixture = Fixture::new();
    let service = ProcessSystemOperations::new(
        CancellationToken::new(),
        fixture.config("https://api.github.example/repos"),
    );

    let detail = service.update_detail(true).await.expect("safe rejection");
    assert!(!detail.update_supported);
    assert!(detail.warning.is_some() || detail.unsupported_reason.is_some());
}

#[tokio::test]
async fn update_detail_should_report_source_build_as_unsupported() {
    let fixture = Fixture::new();
    let mut config = fixture.config("https://api.github.com/repos");
    config.build_type = "source".to_owned();
    config.update_repository = None;
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);

    let detail = service.update_detail(false).await.expect("detail");
    assert_eq!(detail.latest_version, "1.0.0");
    assert!(!detail.update_supported);
}

#[tokio::test]
async fn update_detail_should_use_cached_release_when_not_refreshed() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture.mount_release_once(&server, TARGET_VERSION).await;
    let service = fixture.service(&server);
    service.update_detail(true).await.expect("prime cache");

    assert!(service.update_detail(false).await.is_ok());
}

#[tokio::test]
async fn update_events_should_close_after_terminal_update_log() {
    let fixture = Fixture::new();
    let mut config = fixture.config("https://api.github.com/repos");
    config.build_type = "source".to_owned();
    let service = ProcessSystemOperations::new(CancellationToken::new(), config);
    let mut events = service.update_events();
    let _ = service
        .perform_update(Some(TARGET_VERSION.to_owned()))
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
        .perform_update(Some(TARGET_VERSION.to_owned()))
        .await;

    assert!(stream.next().await.is_some());
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

    assert!(
        fixture
            .service(&server)
            .perform_update(Some(TARGET_VERSION.to_owned()))
            .await
            .is_err()
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

    assert!(
        fixture
            .service(&server)
            .perform_update(Some(TARGET_VERSION.to_owned()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn update_should_reject_insecure_release_archive() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_custom_release(&server, "http://github.com/archive", None)
        .await;

    assert!(
        fixture
            .service(&server)
            .perform_update(Some(TARGET_VERSION.to_owned()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn update_should_reject_release_archive_from_untrusted_host() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_custom_release(&server, "https://evil.example/archive", None)
        .await;

    assert!(
        fixture
            .service(&server)
            .perform_update(Some(TARGET_VERSION.to_owned()))
            .await
            .is_err()
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

    assert!(
        fixture
            .service(&server)
            .perform_update(Some(TARGET_VERSION.to_owned()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn update_should_reject_untrusted_github_api_base() {
    let fixture = Fixture::new();
    let service = ProcessSystemOperations::new(
        CancellationToken::new(),
        fixture.config("https://api.github.example/repos"),
    );
    let error = service
        .perform_update(Some(TARGET_VERSION.to_owned()))
        .await
        .expect_err("untrusted API rejected");

    assert_eq!(error.kind(), SystemOperationErrorKind::Conflict);
}

#[tokio::test]
async fn update_should_reject_when_confirmed_target_differs_from_remote_latest() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_release(&server, "9.9.8", ArchiveKind::Safe, ChecksumKind::Valid)
        .await;

    assert!(
        fixture
            .service(&server)
            .perform_update(Some(TARGET_VERSION.to_owned()))
            .await
            .is_err()
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

    assert!(
        fixture
            .service(&server)
            .perform_update(Some(TARGET_VERSION.to_owned()))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn update_should_replace_local_release_files_with_latest_asset() {
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
    fixture
        .service(&server)
        .perform_update(Some(TARGET_VERSION.to_owned()))
        .await
        .expect("update");

    assert_eq!(
        fs::read(fixture.executable()).expect("binary"),
        b"new-binary"
    );
    assert_eq!(
        fs::read(fixture.web().join("index.html")).expect("web"),
        b"new-web"
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
    config.web_dist_dir = external.path().join("dist");
    fs::create_dir_all(&config.web_dist_dir).expect("web dir");
    fs::write(config.web_dist_dir.join("index.html"), "old-web").expect("web");
    let service = ProcessSystemOperations::new(CancellationToken::new(), config.clone());
    service
        .perform_update(Some(TARGET_VERSION.to_owned()))
        .await
        .expect("update");

    assert_eq!(
        fs::read(config.web_dist_dir.join("index.html")).expect("web"),
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

    assert!(
        fixture
            .service(&server)
            .perform_update(Some(TARGET_VERSION.to_owned()))
            .await
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.web().join("index.html")).expect("web"),
        b"old-web"
    );
}

#[tokio::test]
async fn update_status_should_read_local_update_state() {
    let fixture = Fixture::new();
    fs::write(
        fixture.state(),
        r#"{"previousVersion":"1.0.0","currentVersion":"2.0.0","operation":{"operationId":"x","kind":"update","status":"succeeded","targetVersion":"2.0.0","message":"done","error":null,"startedAt":"2026-07-19T00:00:00Z","finishedAt":"2026-07-19T00:01:00Z"}}"#,
    )
    .expect("state");
    let service = ProcessSystemOperations::new(
        CancellationToken::new(),
        fixture.config("https://api.github.com/repos"),
    );

    assert_eq!(
        service
            .update_status()
            .await
            .expect("status")
            .previous_version
            .as_deref(),
        Some("1.0.0")
    );
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

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("system update root");
        let fixture = Self { root };
        fixture.write_executable("old-binary");
        fs::create_dir_all(fixture.web()).expect("web dir");
        fs::write(fixture.web().join("index.html"), "old-web").expect("web");
        fixture
    }

    fn executable(&self) -> PathBuf {
        self.root.path().join("codex-proxy-rs")
    }

    fn web(&self) -> PathBuf {
        self.root.path().join("web/dist")
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
            update_channel: "stable".to_owned(),
            update_repository: Some("owner/repository".to_owned()),
            github_api_base: api_base.to_owned(),
            executable_path: Some(self.executable()),
            web_dist_dir: self.web(),
            update_state_file: self.state(),
            update_lock_file: self.lock(),
            update_temp_dir: self.root.path().join("tmp"),
            self_restart_enabled: false,
        }
    }

    fn service(&self, server: &MockServer) -> ProcessSystemOperations {
        ProcessSystemOperations::new(
            CancellationToken::new(),
            self.config(&format!("{}/repos", server.uri())),
        )
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
            .and(path("/repos/owner/repository/releases/latest"))
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
    if matches!(kind, ArchiveKind::UnsafePath) {
        append_file(&mut tar, "safe", b"escape", true);
    }
    let encoder = tar.into_inner().expect("tar");
    encoder.finish().expect("gzip")
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
        .and(path("/repos/owner/repository/releases/latest"))
        .respond_with(release_response(version, assets));
    match times {
        Some(times) => mock.up_to_n_times(times).mount(server).await,
        None => mock.mount(server).await,
    }
}

fn release_response(version: &str, assets: Vec<serde_json::Value>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "tag_name": format!("v{version}"),
        "name": format!("Release {version}"),
        "body": "notes",
        "html_url": "https://github.com/owner/repository/releases/latest",
        "prerelease": false,
        "published_at": "2026-07-19T00:00:00Z",
        "assets": assets,
    }))
}
