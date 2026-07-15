use codex_proxy_rs::upstream::openai::desktop_release::{
    APPCAST_POLL_INTERVAL, DesktopReleaseChecker, DesktopReleaseStatus,
    parse_latest_desktop_release,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const APPCAST: &str = r#"
<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <item>
      <title>26.707.72221</title>
      <pubDate>Tue, 14 Jul 2026 07:56:40 +0000</pubDate>
      <sparkle:version>5307</sparkle:version>
      <sparkle:shortVersionString>26.707.72221</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>12.0</sparkle:minimumSystemVersion>
      <sparkle:hardwareRequirements>arm64</sparkle:hardwareRequirements>
      <enclosure url="https://example.invalid/Codex.zip" length="565564803"
        sparkle:edSignature="signature" />
      <sparkle:deltas>
        <enclosure url="https://example.invalid/Codex.delta" length="12" />
      </sparkle:deltas>
    </item>
    <item>
      <sparkle:version>5263</sparkle:version>
      <sparkle:shortVersionString>26.707.71524</sparkle:shortVersionString>
    </item>
  </channel>
</rss>
"#;

#[test]
fn desktop_release_poll_interval_should_be_one_day() {
    assert_eq!(APPCAST_POLL_INTERVAL, std::time::Duration::from_hours(24));
}

#[test]
fn appcast_parser_should_return_first_full_release_and_ignore_deltas() {
    let release = parse_latest_desktop_release(APPCAST).expect("parse appcast");

    assert_eq!(release.version, "26.707.72221");
    assert_eq!(release.build, "5307");
    assert_eq!(
        release.published_at,
        Some(crate::support::storage::timestamp("2026-07-14T07:56:40Z"))
    );
    assert_eq!(release.minimum_system_version.as_deref(), Some("12.0"));
    assert_eq!(release.hardware_requirements.as_deref(), Some("arm64"));
    assert_eq!(
        release.download_url.as_deref(),
        Some("https://example.invalid/Codex.zip")
    );
    assert_eq!(release.download_size, Some(565_564_803));
    assert!(release.signature_present);
}

#[test]
fn appcast_parser_should_reject_release_without_codex_desktop_version() {
    let error = parse_latest_desktop_release("<rss><channel><item /></channel></rss>")
        .expect_err("missing version should fail");

    assert!(error.to_string().contains("shortVersionString"));
}

#[tokio::test]
async fn desktop_release_checker_should_update_observation_only() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/appcast.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(APPCAST, "application/xml"))
        .mount(&server)
        .await;
    let status = DesktopReleaseStatus::default();
    let checker = DesktopReleaseChecker::with_client(
        reqwest::Client::new(),
        format!("{}/appcast.xml", server.uri()),
        status.clone(),
    );

    checker.check_and_record().await.expect("check appcast");

    let snapshot = status.snapshot();
    assert_eq!(
        snapshot
            .latest
            .as_ref()
            .map(|release| release.version.as_str()),
        Some("26.707.72221")
    );
    assert!(snapshot.checked_at.is_some());
    assert!(snapshot.last_error.is_none());
}
