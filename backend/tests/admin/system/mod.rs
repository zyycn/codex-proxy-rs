use std::{io::Write, path::Path, sync::Arc};

use axum::{
    body::Body,
    http::{header::CONTENT_TYPE, Request, StatusCode},
};
use codex_proxy_rs::{
    admin::auth::service::SqliteAdminSessionStore,
    admin::keys::service::SqliteClientKeyStore,
    admin::monitoring::usage_record_store::SqliteUsageRecordStore,
    infra::database::connect_sqlite,
    proxy::dispatch::session_affinity::SqliteSessionAffinityStore,
    runtime::services::{BackgroundTaskStores, Services},
    runtime::state::AppState,
    upstream::accounts::{
        cookies::SqliteCookieStore, store::SqliteAccountStore, token_refresh::RefreshLeaseStore,
    },
    upstream::fingerprint::FingerprintRepository,
};
use flate2::{write::GzEncoder, Compression};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tar::{Builder, Header};
use tokio::sync::Mutex;
use tower::util::ServiceExt;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

use crate::support::{
    admin::seed_admin_session, config::test_config, fingerprint::test_fingerprint,
    http::response_json,
};

static SYSTEM_ENV_LOCK: Mutex<()> = Mutex::const_new(());
const VERSION_OLD: &str = "0.1.0";
const VERSION_NEW: &str = "0.2.0";

#[tokio::test]
async fn version_should_return_backend_build_metadata() {
    let _guard = SYSTEM_ENV_LOCK.lock().await;
    set_system_update_env("zyycn/codex-proxy-rs-version-route", "http://127.0.0.1:9");
    std::env::set_var("CPR_GIT_SHA", "version-test-sha");
    std::env::set_var("CPR_BUILD_TIME", "2026-07-01T00:00:00Z");
    let (app, _dir) = admin_system_test_app("system-version.sqlite").await;

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
    );
}

#[tokio::test]
async fn check_updates_should_use_cached_release_when_not_forced() {
    let _guard = SYSTEM_ENV_LOCK.lock().await;
    let repository = "zyycn/codex-proxy-rs-cache-route";
    let success_server = MockServer::start().await;
    mount_latest_release(&success_server, repository, "0.2.0").await;
    set_system_update_env(repository, &success_server.uri());
    let (app, _dir) = admin_system_test_app("system-cache.sqlite").await;
    let initial = get_check_updates(&app, true, "req_system_cache_initial").await;

    let failing_server = MockServer::start().await;
    set_system_update_env(repository, &failing_server.uri());
    let cached = get_check_updates(&app, false, "req_system_cache_cached").await;

    assert!(
        initial["hasUpdate"] == true
            && cached["cached"] == true
            && cached["latestVersion"] == "0.2.0"
    );
}

#[tokio::test]
async fn check_updates_should_fallback_to_cache_when_forced_fetch_fails() {
    let _guard = SYSTEM_ENV_LOCK.lock().await;
    let repository = "zyycn/codex-proxy-rs-cache-forced-route";
    let success_server = MockServer::start().await;
    mount_latest_release(&success_server, repository, "0.3.0").await;
    set_system_update_env(repository, &success_server.uri());
    let (app, _dir) = admin_system_test_app("system-cache-forced.sqlite").await;
    let initial = get_check_updates(&app, true, "req_system_cache_forced_initial").await;

    let failing_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/{repository}/releases/latest")))
        .respond_with(ResponseTemplate::new(500))
        .mount(&failing_server)
        .await;
    set_system_update_env(repository, &failing_server.uri());
    let fallback = get_check_updates(&app, true, "req_system_cache_forced_fallback").await;

    assert!(
        initial["hasUpdate"] == true
            && fallback["cached"] == true
            && fallback["latestVersion"] == "0.3.0"
            && fallback["warning"].is_null()
    );
}

#[tokio::test]
async fn check_updates_should_report_source_build_as_unsupported() {
    let _guard = SYSTEM_ENV_LOCK.lock().await;
    let repository = "zyycn/codex-proxy-rs-source-route";
    let github = MockServer::start().await;
    mount_latest_release(&github, repository, "0.2.0").await;
    set_system_update_env(repository, &github.uri());
    std::env::set_var("CPR_BUILD_TYPE", "source");
    let (app, _dir) = admin_system_test_app("system-source-build.sqlite").await;

    let info = get_check_updates(&app, true, "req_system_source_build").await;

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
async fn update_should_replace_local_release_files_with_latest_asset() {
    let _guard = SYSTEM_ENV_LOCK.lock().await;
    let repository = "zyycn/codex-proxy-rs-update-route";
    let github = MockServer::start().await;
    let deploy = tempfile::tempdir().unwrap();
    let exe_path = deploy.path().join("codex-proxy-rs");
    let web_dist = deploy.path().join("web/dist");
    std::fs::create_dir_all(&web_dist).unwrap();
    std::fs::write(&exe_path, "old-binary").unwrap();
    std::fs::write(web_dist.join("index.html"), "old-web").unwrap();
    mount_latest_release_with_archive(&github, repository, "0.4.0", "new-binary", "new-web").await;
    set_system_update_env(repository, &github.uri());
    set_system_update_paths(deploy.path(), &exe_path, &web_dist);
    let (app, _dir) = admin_system_test_app("system-update.sqlite").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/system/update")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_update_latest")
                .body(Body::empty())
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
async fn update_should_replace_web_assets_across_filesystems() {
    let _guard = SYSTEM_ENV_LOCK.lock().await;
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
    set_system_update_env(repository, &github.uri());
    set_system_update_paths(deploy.path(), &exe_path, &web_dist);
    let (app, _dir) = admin_system_test_app("system-cross-device-update.sqlite").await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/system/update")
                .header("cookie", "cpr_admin_session=session_1")
                .header("x-request-id", "req_system_cross_device_update")
                .body(Body::empty())
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
    let _guard = SYSTEM_ENV_LOCK.lock().await;
    let deploy = tempfile::tempdir().unwrap();
    set_system_update_env("zyycn/codex-proxy-rs-status-route", "http://127.0.0.1:9");
    set_system_update_paths(
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
    let (app, _dir) = admin_system_test_app("system-status.sqlite").await;

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
    let _guard = SYSTEM_ENV_LOCK.lock().await;
    let deploy = tempfile::tempdir().unwrap();
    set_system_update_env("zyycn/codex-proxy-rs-events-route", "http://127.0.0.1:9");
    set_system_update_paths(
        deploy.path(),
        &deploy.path().join("codex-proxy-rs"),
        &deploy.path().join("web/dist"),
    );
    let (app, _dir) = admin_system_test_app("system-events.sqlite").await;

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

async fn get_check_updates(app: &axum::Router, force: bool, request_id: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/admin/system/check-updates?force={force}"))
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
                    "browser_download_url": format!("{}/download/{archive_name}", server.uri()),
                    "size": archive.len()
                },
                {
                    "name": "checksums.txt",
                    "browser_download_url": format!("{}/download/checksums.txt", server.uri()),
                    "size": checksum.len()
                }
            ]
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/download/{archive_name}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/checksums.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(checksum))
        .mount(server)
        .await;
}

fn release_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        arch => arch,
    }
}

fn release_archive(binary: &str, index: &str) -> Vec<u8> {
    let mut data = Vec::new();
    {
        let encoder = GzEncoder::new(&mut data, Compression::default());
        let mut builder = Builder::new(encoder);
        append_tar_file(&mut builder, "codex-proxy-rs", binary.as_bytes());
        append_tar_file(&mut builder, "web/dist/index.html", index.as_bytes());
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

fn set_system_update_env(repository: &str, github_api_base: &str) {
    std::env::set_var("CPR_VERSION", "0.1.0");
    std::env::set_var("CPR_DEPLOYMENT_MODE", "docker");
    std::env::set_var("CPR_BUILD_TYPE", "release");
    std::env::set_var("CPR_UPDATE_CHANNEL", "stable");
    std::env::set_var("CPR_UPDATE_REPOSITORY", repository);
    std::env::set_var("CPR_GITHUB_API_BASE", format!("{github_api_base}/repos"));
    std::env::set_var("CPR_UPDATE_ALLOW_INSECURE_DOWNLOADS", "true");
    std::env::remove_var("CPR_UPDATE_EXE_PATH");
    std::env::remove_var("CPR_WEB_DIST_DIR");
    std::env::remove_var("CPR_UPDATE_STATE_FILE");
    std::env::remove_var("CPR_UPDATE_LOCK_FILE");
}

fn set_system_update_paths(root: &Path, exe_path: &Path, web_dist: &Path) {
    std::env::set_var("CPR_UPDATE_EXE_PATH", exe_path);
    std::env::set_var("CPR_WEB_DIST_DIR", web_dist);
    std::env::set_var(
        "CPR_UPDATE_STATE_FILE",
        root.join(".runtime/update-state.json"),
    );
    std::env::set_var("CPR_UPDATE_LOCK_FILE", root.join(".runtime/update.lock"));
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
    let data = response_json(response).await["data"].clone();
    assert_eq!(data["operation"]["status"], expected);
    data
}

async fn admin_system_test_app(db_name: &str) -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(db_name);
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    seed_admin_session(&pool, "session_1").await;
    let config = test_config(url);
    let stores = stores(pool);
    let services = Arc::new(Services::new(&config, stores, test_fingerprint()));
    let state = AppState {
        config,
        services: (*services).clone(),
    };
    let app = codex_proxy_rs::http::router::router().with_state(state);
    (app, dir)
}

fn stores(pool: SqlitePool) -> BackgroundTaskStores {
    BackgroundTaskStores {
        accounts: SqliteAccountStore::new(pool.clone()),
        admin_sessions: SqliteAdminSessionStore::new(pool.clone()),
        cookies: SqliteCookieStore::new(pool.clone()),
        fingerprints: FingerprintRepository::new(pool.clone()),
        session_affinity: SqliteSessionAffinityStore::new(pool.clone()),
        refresh_leases: RefreshLeaseStore::new(pool.clone()),
        client_keys: SqliteClientKeyStore::new(pool.clone()),
        usage_records: SqliteUsageRecordStore::new(pool),
    }
}
