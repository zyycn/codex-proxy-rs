//! 系统更新测试入口与发行包、前置校验和本地安装夹具

mod channels;
mod discovery;
mod download;
mod events;
mod installation;
mod operations;
mod release;
mod state;
mod swap_paths;

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
        SystemOperationAccepted, SystemOperationKind, SystemOperationStatus, SystemUpdateCheck,
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
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BINARY_NAME: &str = if cfg!(windows) {
    "codex-proxy-rs.exe"
} else {
    "codex-proxy-rs"
};

const OLD_RELEASE_MANIFEST: &str = r#"{"gateway_version":"1.0.0"}"#;

const TARGET_VERSION: &str = "1.9.9";
const CROSS_MAJOR_VERSION: &str = "2.0.0";

#[derive(Default)]
struct RecordingDiagnostics {
    failures: std::sync::Mutex<Vec<gateway_core::diagnostics::OperationalFailure>>,
    reject: bool,
}

#[async_trait::async_trait]
impl gateway_core::diagnostics::OperationalDiagnostics for RecordingDiagnostics {
    async fn record_failure(
        &self,
        failure: gateway_core::diagnostics::OperationalFailure,
    ) -> Result<(), gateway_core::error::StoreError> {
        self.failures
            .lock()
            .expect("diagnostic failures")
            .push(failure);
        if self.reject {
            Err(gateway_core::error::StoreError::new(
                gateway_core::error::StoreErrorKind::Unavailable,
            ))
        } else {
            Ok(())
        }
    }
}

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
        self.root.path().join(BINARY_NAME)
    }

    fn backup(&self) -> PathBuf {
        self.root.path().join(format!("{BINARY_NAME}.backup"))
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
        mount_release_json(server, version, available_assets(version), Some(1)).await;
    }

    async fn mount_custom_release(
        &self,
        server: &MockServer,
        archive_url: &str,
        checksum_url: Option<&str>,
    ) {
        let archive = release_archive(ArchiveKind::Safe);
        let name = archive_name(TARGET_VERSION);
        let checksums = format!("{}  {name}\n", hex::encode(Sha256::digest(&archive)));
        let local_url = format!("{}/checksums", server.uri());
        let assets = vec![
            serde_json::json!({"name": name, "browser_download_url": archive_url, "size": archive.len()}),
            serde_json::json!({"name": "checksums.txt", "browser_download_url": checksum_url.unwrap_or(&local_url), "size": checksums.len()}),
        ];
        if checksum_url.is_none() {
            Mock::given(path("/checksums"))
                .respond_with(ResponseTemplate::new(200).set_body_string(checksums))
                .mount(server)
                .await;
        }
        mount_release_json(server, TARGET_VERSION, assets, None).await;
    }
}

#[derive(Clone, Copy)]
enum ArchiveKind {
    Safe,
    UnsafePath(&'static str),
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
    append_file(&mut tar, BINARY_NAME, b"new-binary", 0o755, false);
    append_file(&mut tar, "web/dist/index.html", b"new-web", 0o755, false);
    if !matches!(kind, ArchiveKind::MissingOfficialManifest) {
        append_file(
            &mut tar,
            "plugins/official/plugin-release-manifest.json",
            b"new-manifest",
            0o444,
            false,
        );
    }
    append_file(
        &mut tar,
        "plugins/official/new-plugin.tar.gz",
        b"new-plugin",
        0o644,
        false,
    );
    if let ArchiveKind::UnsafePath(name) = kind {
        append_file(&mut tar, name, b"escape", 0o755, true);
    }
    if matches!(kind, ArchiveKind::NestedOfficialAsset) {
        append_file(
            &mut tar,
            "plugins/official/nested/plugin.tar.gz",
            b"nested",
            0o444,
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

fn append_file(
    tar: &mut Builder<GzEncoder<Vec<u8>>>,
    name: &str,
    data: &[u8],
    mode: u32,
    unsafe_path: bool,
) {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_mode(mode);
    header.set_size(u64::try_from(data.len()).expect("size"));
    header
        .set_path(if unsafe_path { "safe" } else { name })
        .expect("path");
    if unsafe_path {
        let bytes = header.as_mut_bytes();
        bytes[..100].fill(0);
        bytes[..name.len()].copy_from_slice(name.as_bytes());
    }
    header.set_cksum();
    tar.append(&header, data).expect("append");
}

fn archive_name(version: &str) -> String {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    };
    format!("codex-proxy-rs_{version}_{os}_{arch}.tar.gz")
}

async fn mount_release_json(
    server: &MockServer,
    version: &str,
    assets: Vec<serde_json::Value>,
    times: Option<u64>,
) {
    mount_index(server, fixture_index(version)).await;
    let mock = Mock::given(method("GET"))
        .and(path(format!(
            "/repos/owner/repository/releases/tags/v{version}"
        )))
        .respond_with(release_response(version, assets));
    match times {
        Some(n) => mock.up_to_n_times(n).mount(server).await,
        None => mock.mount(server).await,
    }
}

const BUILD_VERSION: &str = "4.0.0-beta.1";
const INDEX_PATH: &str = "/owner/repository/refs/heads/main/release/version.yaml";

fn available_assets(version: &str) -> Vec<serde_json::Value> {
    [archive_name(version), "checksums.txt".to_owned()].into_iter()
        .map(|name| serde_json::json!({"name":name,"size":128,"browser_download_url":"https://github.com/owner/repository/releases/download/test/file"})).collect()
}

fn fixture_index(version: &str) -> serde_json::Value {
    let major = version.split('.').next().unwrap();
    let base = version.split('-').next().unwrap();
    serde_json::json!({"version":BUILD_VERSION, "published": {major: {"stable": version,"beta":version,"rc":version,"alpha":version,"exp":{base: version}}}})
}

async fn mount_index(server: &MockServer, index: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(INDEX_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(index))
        .mount(server)
        .await;
}

fn release_response(version: &str, assets: Vec<serde_json::Value>) -> ResponseTemplate {
    let prerelease = !semver::Version::parse(version)
        .expect("version")
        .pre
        .is_empty();
    let release = serde_json::json!({
        "draft": false, "tag_name": format!("v{version}"),
        "name": format!("Release {version}"),
        "body": "notes",
        "html_url": "https://github.com/owner/repository/releases/latest",
        "prerelease": prerelease,
        "published_at": "2026-07-19T00:00:00Z",
        "assets": assets,
    });
    ResponseTemplate::new(200).set_body_json(release)
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
