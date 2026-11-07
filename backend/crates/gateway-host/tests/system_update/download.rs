//! 更新下载来源信任边界测试

use super::*;

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
async fn archive_is_the_last_remote_request_before_activation() {
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
    let installed = complete_update(&service, TARGET_VERSION).await;
    assert_eq!(installed.operation.status, SystemOperationStatus::Succeeded);
    assert!(installed.need_restart);
    // 从磁盘恢复和探测新版本同样不能在下载之后依赖 GitHub
    let mut config = fixture.config(&format!("{}/repos", server.uri()));
    config.version = TARGET_VERSION.to_owned();
    let activated = ProcessSystemOperations::new(CancellationToken::new(), config);
    assert_eq!(activated.version().await.unwrap().version, TARGET_VERSION);
    assert!(!activated.update_status().await.unwrap().need_restart);
    let requests = server.received_requests().await.unwrap();
    let paths: Vec<_> = requests.iter().map(|req| req.url.path()).collect();
    assert_eq!(
        paths,
        [
            INDEX_PATH,
            &format!("/repos/owner/repository/releases/tags/v{TARGET_VERSION}"),
            "/checksums",
            "/archive"
        ]
    );
}

#[tokio::test]
async fn rate_limited_checksums_leave_the_package_undownloaded_and_installation_untouched() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    fixture
        .mount_custom_release(
            &server,
            &format!("{}/archive", server.uri()),
            Some(&format!("{}/limited-checksums", server.uri())),
        )
        .await;
    Mock::given(path("/limited-checksums"))
        .respond_with(ResponseTemplate::new(429))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/archive"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let status = complete_update(&fixture.service(&server), TARGET_VERSION).await;
    assert_eq!(status.operation.status, SystemOperationStatus::Failed);
    assert!(status.operation.error.unwrap().contains("HTTP 429"));
    assert_eq!(fs::read(fixture.executable()).unwrap(), b"old-binary");
    assert!(!status.need_restart);
}
