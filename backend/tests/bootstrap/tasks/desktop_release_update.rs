use super::*;

#[tokio::test]
async fn desktop_release_update_task_should_start_background_checker() {
    let status = DesktopReleaseStatus::default();
    let handle =
        codex_proxy_rs::bootstrap::tasks::desktop_release_update::start_desktop_release_update_task(
            status,
            "https://example.invalid/appcast.xml".to_string(),
        );

    handle.shutdown().await;
}

#[tokio::test]
async fn desktop_release_update_task_should_record_initial_appcast_observation() {
    let server = MockServer::start().await;
    mount_appcast(
        &server,
        r#"
        <rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
          <channel>
            <item>
              <pubDate>Tue, 14 Jul 2026 07:56:40 +0000</pubDate>
              <sparkle:shortVersionString>26.900.1</sparkle:shortVersionString>
              <sparkle:version>7001</sparkle:version>
              <sparkle:minimumSystemVersion>12.0</sparkle:minimumSystemVersion>
              <sparkle:hardwareRequirements>arm64</sparkle:hardwareRequirements>
              <enclosure url="https://example.invalid/download" length="1234"
                sparkle:edSignature="signature" />
            </item>
          </channel>
        </rss>
        "#,
    )
    .await;

    let status = DesktopReleaseStatus::default();
    let handle =
        codex_proxy_rs::bootstrap::tasks::desktop_release_update::start_desktop_release_update_task(
            status.clone(),
            format!("{}/appcast.xml", server.uri()),
        );

    let snapshot = wait_for_latest_desktop_release(&status, "26.900.1").await;
    handle.shutdown().await;

    let release = snapshot.latest.expect("latest release");
    assert_eq!(release.build, "7001");
    assert_eq!(release.minimum_system_version.as_deref(), Some("12.0"));
    assert_eq!(release.hardware_requirements.as_deref(), Some("arm64"));
    assert_eq!(release.download_size, Some(1234));
    assert!(release.signature_present);
    assert!(snapshot.checked_at.is_some());
    assert!(snapshot.last_error.is_none());
}

#[tokio::test]
async fn desktop_release_update_task_should_record_fetch_failure_without_mutating_wire_profile() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/appcast.xml"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let profile = crate::support::wire_profile::test_wire_profile();
    let original_user_agent = profile.user_agent();
    let status = DesktopReleaseStatus::default();
    let handle =
        codex_proxy_rs::bootstrap::tasks::desktop_release_update::start_desktop_release_update_task(
            status.clone(),
            format!("{}/appcast.xml", server.uri()),
        );

    wait_for_appcast_requests(&server, 1).await;
    let deadline = tokio::time::Instant::now() + StdDuration::from_secs(2);
    while status.snapshot().last_error.is_none() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(StdDuration::from_millis(25)).await;
    }
    handle.shutdown().await;

    assert_eq!(profile.user_agent(), original_user_agent);
    assert!(status.snapshot().latest.is_none());
}
