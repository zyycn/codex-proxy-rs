use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use futures::future::BoxFuture;
use provider_openai::transport::profile::{
    CodexDesktopRelease, CodexDesktopReleaseError, CodexDesktopReleaseService,
    CodexDesktopReleaseTransport, CodexWireProfile, CodexWireProfileState, parse_desktop_release,
};

struct ReleaseTransport {
    outcomes: Mutex<VecDeque<Result<CodexDesktopRelease, CodexDesktopReleaseError>>>,
}

impl ReleaseTransport {
    fn new(
        outcomes: impl IntoIterator<Item = Result<CodexDesktopRelease, CodexDesktopReleaseError>>,
    ) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
        }
    }
}

impl CodexDesktopReleaseTransport for ReleaseTransport {
    fn fetch(&self) -> BoxFuture<'_, Result<CodexDesktopRelease, CodexDesktopReleaseError>> {
        Box::pin(async move {
            self.outcomes
                .lock()
                .expect("release outcomes")
                .pop_front()
                .expect("release outcome")
        })
    }
}

#[test]
fn wire_profile_should_generate_codex_core_user_agent() {
    let profile = CodexWireProfile {
        originator: "Codex Desktop".to_owned(),
        codex_version: "0.144.2".to_owned(),
        desktop_version: "26.707.72221".to_owned(),
        desktop_build: "72221".to_owned(),
        os_type: "Mac OS".to_owned(),
        os_version: "15.7.1".to_owned(),
        arch: "arm64".to_owned(),
        terminal: "unknown".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("valid fixture time"),
    };

    assert_eq!(
        profile.user_agent(),
        "Codex Desktop/0.144.2 (Mac OS 15.7.1; arm64) unknown (Codex Desktop; 26.707.72221)"
    );
}

#[test]
fn desktop_appcast_should_publish_the_first_complete_release() {
    let release = parse_desktop_release(
        r#"<?xml version="1.0" encoding="utf-8"?>
        <rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
          <channel>
            <item>
              <title>Incomplete release without artifact identity</title>
            </item>
            <item>
              <pubDate>Sun, 19 Jul 2026 08:00:00 +0000</pubDate>
              <sparkle:minimumSystemVersion>14.0</sparkle:minimumSystemVersion>
              <sparkle:hardwareRequirements>arm64</sparkle:hardwareRequirements>
              <enclosure url="https://persistent.oaistatic.com/codex.dmg"
                length="123456" sparkle:shortVersionString="26.720.1"
                sparkle:version="72001" sparkle:edSignature="signature" />
            </item>
            <item>
              <enclosure sparkle:shortVersionString="26.719.1" sparkle:version="71901" />
            </item>
          </channel>
        </rss>"#,
    )
    .expect("valid appcast");

    assert_eq!(release.version, "26.720.1");
    assert_eq!(release.build, "72001");
    assert_eq!(release.download_size, Some(123_456));
    assert!(release.signature_present);
}

#[test]
fn desktop_appcast_should_reject_invalid_version_and_build() {
    for (version, build, expected) in [
        ("latest", "72001", CodexDesktopReleaseError::InvalidVersion),
        (
            "26.720.1",
            "build-72001",
            CodexDesktopReleaseError::InvalidBuild,
        ),
    ] {
        let error = parse_desktop_release(&format!(
            r#"<rss xmlns:sparkle="https://sparkle-project.org/xml-namespaces/sparkle"><channel><item><enclosure sparkle:shortVersionString="{version}" sparkle:version="{build}" /></item></channel></rss>"#,
        ))
        .expect_err("invalid release identity");
        assert_eq!(
            std::mem::discriminant(&error),
            std::mem::discriminant(&expected)
        );
    }
}

#[tokio::test]
async fn wire_profile_release_update_should_change_only_desktop_identity() {
    let original = wire_profile();
    let state = CodexWireProfileState::new(original.clone());
    let service = CodexDesktopReleaseService::new(
        state.clone(),
        Arc::new(ReleaseTransport::new([Ok(release("26.720.1", "72001"))])),
    );

    service.refresh().await.expect("release refresh");

    let updated = state.snapshot();
    assert_eq!(updated.desktop_version, "26.720.1");
    assert_eq!(updated.desktop_build, "72001");
    assert_eq!(updated.codex_version, original.codex_version);
    assert_eq!(updated.originator, original.originator);
    assert_eq!(updated.os_type, original.os_type);
    assert_eq!(updated.os_version, original.os_version);
    assert_eq!(updated.arch, original.arch);
    assert_eq!(updated.terminal, original.terminal);
    assert_eq!(updated.verified_at, original.verified_at);
    let status = service.status().snapshot();
    assert_eq!(status.latest, Some(release("26.720.1", "72001")));
    assert!(status.checked_at.is_some());
    assert!(status.last_error.is_none());
}

#[tokio::test]
async fn failed_release_refresh_should_preserve_the_last_successful_profile_and_release() {
    let state = CodexWireProfileState::new(wire_profile());
    let latest = release("26.720.1", "72001");
    let service = CodexDesktopReleaseService::new(
        state.clone(),
        Arc::new(ReleaseTransport::new([
            Ok(latest.clone()),
            Err(CodexDesktopReleaseError::InvalidDocument),
        ])),
    );
    service.refresh().await.expect("first release refresh");
    let successful_profile = state.snapshot();

    service
        .refresh()
        .await
        .expect_err("second release refresh should fail");

    assert_eq!(state.snapshot(), successful_profile);
    let status = service.status().snapshot();
    assert_eq!(status.latest, Some(latest));
    assert_eq!(
        status.last_error.as_deref(),
        Some("Codex Desktop appcast document is invalid")
    );
    assert!(status.checked_at.is_some());
}

fn wire_profile() -> CodexWireProfile {
    CodexWireProfile {
        originator: "Codex Desktop".to_owned(),
        codex_version: "0.144.2".to_owned(),
        desktop_version: "26.707.72221".to_owned(),
        desktop_build: "72221".to_owned(),
        os_type: "Mac OS".to_owned(),
        os_version: "15.7.1".to_owned(),
        arch: "arm64".to_owned(),
        terminal: "unknown".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 7, 18, 0, 0, 0)
            .single()
            .expect("valid fixture time"),
    }
}

fn release(version: &str, build: &str) -> CodexDesktopRelease {
    CodexDesktopRelease {
        version: version.to_owned(),
        build: build.to_owned(),
        published_at: None,
        minimum_system_version: Some("14.0".to_owned()),
        hardware_requirements: Some("arm64".to_owned()),
        download_url: Some("https://persistent.oaistatic.com/codex.dmg".to_owned()),
        download_size: Some(123_456),
        signature_present: true,
    }
}
