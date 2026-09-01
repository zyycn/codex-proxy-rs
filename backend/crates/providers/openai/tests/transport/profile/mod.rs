use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use futures::future::BoxFuture;
use gateway_core::provider_ports::{
    ProviderArtifactProfile, ProviderArtifactProfileCachePort, ProviderStoreError,
    ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;
use provider_openai::transport::profile::{
    CodexArtifactProfileCache, CodexBundledReleaseProfile, CodexDesktopRelease,
    CodexDesktopReleaseError, CodexDesktopReleaseService, CodexDesktopReleaseTransport,
    CodexWireProfile, CodexWireProfileState, OfficialCodexDesktopReleaseTransport,
    parse_desktop_release,
};

mod desktop_artifact;

struct ReleaseTransport {
    releases: Mutex<VecDeque<Result<CodexDesktopRelease, CodexDesktopReleaseError>>>,
    core_versions: Mutex<VecDeque<Result<String, CodexDesktopReleaseError>>>,
    core_probes: Mutex<usize>,
}

impl ReleaseTransport {
    fn new(
        releases: impl IntoIterator<Item = Result<CodexDesktopRelease, CodexDesktopReleaseError>>,
        core_versions: impl IntoIterator<Item = Result<String, CodexDesktopReleaseError>>,
    ) -> Self {
        Self {
            releases: Mutex::new(releases.into_iter().collect()),
            core_versions: Mutex::new(core_versions.into_iter().collect()),
            core_probes: Mutex::new(0),
        }
    }

    fn core_probes(&self) -> usize {
        *self.core_probes.lock().expect("core probe count")
    }
}

impl CodexDesktopReleaseTransport for ReleaseTransport {
    fn fetch(&self) -> BoxFuture<'_, Result<CodexDesktopRelease, CodexDesktopReleaseError>> {
        Box::pin(async move {
            self.releases
                .lock()
                .expect("release outcomes")
                .pop_front()
                .expect("release outcome")
        })
    }

    fn fetch_bundled_core_version<'a>(
        &'a self,
        _release: &'a CodexDesktopRelease,
    ) -> BoxFuture<'a, Result<String, CodexDesktopReleaseError>> {
        Box::pin(async move {
            *self.core_probes.lock().expect("core probe count") += 1;
            self.core_versions
                .lock()
                .expect("Core version outcomes")
                .pop_front()
                .expect("Core version outcome")
        })
    }
}

#[derive(Default)]
struct ArtifactProfiles {
    profile: Mutex<Option<ProviderArtifactProfile>>,
    fail_writes: bool,
}

impl ArtifactProfiles {
    fn failing() -> Self {
        Self {
            profile: Mutex::new(None),
            fail_writes: true,
        }
    }
}

impl ProviderArtifactProfileCachePort for ArtifactProfiles {
    fn replace_if_newer(
        &self,
        profile: ProviderArtifactProfile,
        _ttl: Duration,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            if self.fail_writes {
                return Err(ProviderStoreError::new(
                    ProviderStoreErrorKind::Unavailable,
                    "replace test artifact profile",
                ));
            }
            let mut current = self.profile.lock().expect("artifact profile");
            if let Some(current) = current.as_ref() {
                if current.artifact_sequence() > profile.artifact_sequence() {
                    return Ok(false);
                }
                if current.artifact_sequence() == profile.artifact_sequence()
                    && current.profile() != profile.profile()
                {
                    return Err(ProviderStoreError::new(
                        ProviderStoreErrorKind::Conflict,
                        "replace test artifact profile",
                    ));
                }
            }
            *current = Some(profile);
            Ok(true)
        })
    }

    fn read<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
    ) -> BoxFuture<'a, Result<Option<ProviderArtifactProfile>, ProviderStoreError>> {
        Box::pin(async move {
            Ok(self
                .profile
                .lock()
                .expect("artifact profile")
                .as_ref()
                .filter(|profile| profile.provider_kind() == provider_kind)
                .cloned())
        })
    }
}

#[test]
fn wire_profile_should_generate_bundled_core_app_server_user_agent() {
    let profile = CodexWireProfile {
        originator: "Codex Desktop".to_owned(),
        codex_version: "0.147.0-alpha.6.6".to_owned(),
        desktop_version: "26.803.81509".to_owned(),
        desktop_build: "6415".to_owned(),
        os_type: "Mac OS".to_owned(),
        os_version: "15.7.1".to_owned(),
        arch: "arm64".to_owned(),
        terminal: "unknown".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 8, 3, 0, 0, 0)
            .single()
            .expect("valid fixture time"),
    };

    assert_eq!(
        profile.user_agent(),
        "Codex Desktop/0.147.0-alpha.6.6 (Mac OS 15.7.1; arm64) unknown (Codex Desktop; 26.803.81509)"
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
              <enclosure url="https://persistent.oaistatic.com/codex.zip"
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
async fn bundled_release_update_should_change_core_and_desktop_identity_atomically() {
    let original = wire_profile();
    let original_user_agent = original.user_agent();
    let state = CodexWireProfileState::new(original.clone());
    let transport = Arc::new(ReleaseTransport::new(
        [Ok(release("26.810.41047", "6570"))],
        [Ok("0.148.0-alpha.9".to_owned())],
    ));
    let cache = Arc::new(ArtifactProfiles::default());
    let service = service(state.clone(), transport.clone(), cache.clone(), None);

    service.refresh().await.expect("release refresh");

    let updated = state.snapshot();
    assert_eq!(updated.desktop_version, "26.810.41047");
    assert_eq!(updated.desktop_build, "6570");
    assert_eq!(updated.codex_version, "0.148.0-alpha.9");
    assert_eq!(updated.originator, original.originator);
    assert_eq!(updated.os_type, original.os_type);
    assert_eq!(updated.os_version, original.os_version);
    assert_eq!(updated.arch, original.arch);
    assert_eq!(updated.terminal, original.terminal);
    assert!(updated.verified_at > original.verified_at);
    assert_ne!(updated.user_agent(), original_user_agent);
    assert_eq!(
        updated.user_agent(),
        "Codex Desktop/0.148.0-alpha.9 (Mac OS 15.7.1; arm64) unknown (Codex Desktop; 26.810.41047)"
    );
    assert_eq!(transport.core_probes(), 1);
    assert!(cache.profile.lock().expect("artifact profile").is_some());
    let status = service.status().snapshot();
    assert_eq!(status.latest, Some(release("26.810.41047", "6570")));
    assert!(status.checked_at.is_some());
    assert!(status.last_error.is_none());
}

#[tokio::test]
async fn unchanged_verified_release_should_refresh_cache_without_downloading_core_again() {
    let state = CodexWireProfileState::new(wire_profile());
    let latest = release("26.810.41047", "6570");
    let transport = Arc::new(ReleaseTransport::new(
        [Ok(latest.clone()), Ok(latest)],
        [Ok("0.148.0-alpha.9".to_owned())],
    ));
    let cache = Arc::new(ArtifactProfiles::default());
    let service = service(state, transport.clone(), cache, None);

    service.refresh().await.expect("first release refresh");
    service.refresh().await.expect("cached release refresh");

    assert_eq!(transport.core_probes(), 1);
}

#[tokio::test]
async fn failed_release_refresh_should_preserve_the_last_successful_profile_and_release() {
    let state = CodexWireProfileState::new(wire_profile());
    let latest = release("26.810.41047", "6570");
    let transport = Arc::new(ReleaseTransport::new(
        [
            Ok(latest.clone()),
            Err(CodexDesktopReleaseError::InvalidDocument),
        ],
        [Ok("0.148.0-alpha.9".to_owned())],
    ));
    let service = service(
        state.clone(),
        transport,
        Arc::new(ArtifactProfiles::default()),
        None,
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

#[tokio::test]
async fn cache_failure_should_not_publish_a_partially_verified_profile() {
    let original = wire_profile();
    let state = CodexWireProfileState::new(original.clone());
    let transport = Arc::new(ReleaseTransport::new(
        [Ok(release("26.810.41047", "6570"))],
        [Ok("0.148.0-alpha.9".to_owned())],
    ));
    let service = service(
        state.clone(),
        transport,
        Arc::new(ArtifactProfiles::failing()),
        None,
    );

    service
        .refresh()
        .await
        .expect_err("cache failure must fail the refresh");

    assert_eq!(state.snapshot(), original);
    assert!(service.status().snapshot().latest.is_none());
}

#[tokio::test]
#[ignore = "downloads bounded ranges from the current official macOS artifact"]
async fn official_desktop_artifact_should_expose_its_bundled_core_version() {
    let transport = OfficialCodexDesktopReleaseTransport::new().expect("official transport");
    let release = transport.fetch().await.expect("official appcast release");
    let core_version = transport
        .fetch_bundled_core_version(&release)
        .await
        .expect("bundled Core version");

    semver::Version::parse(&core_version).expect("semantic bundled Core version");
    eprintln!(
        "official Desktop {} (build {}) bundles Core {core_version}",
        release.version, release.build
    );
}

fn service(
    state: CodexWireProfileState,
    transport: Arc<ReleaseTransport>,
    cache: Arc<ArtifactProfiles>,
    verified: Option<CodexBundledReleaseProfile>,
) -> CodexDesktopReleaseService {
    let provider = ProviderKind::new("openai").expect("provider kind");
    CodexDesktopReleaseService::new(
        state,
        transport,
        CodexArtifactProfileCache::new(provider, cache),
        verified,
    )
}

fn wire_profile() -> CodexWireProfile {
    CodexWireProfile {
        originator: "Codex Desktop".to_owned(),
        codex_version: "0.147.0-alpha.6.6".to_owned(),
        desktop_version: "26.803.81509".to_owned(),
        desktop_build: "6415".to_owned(),
        os_type: "Mac OS".to_owned(),
        os_version: "15.7.1".to_owned(),
        arch: "arm64".to_owned(),
        terminal: "unknown".to_owned(),
        verified_at: Utc
            .with_ymd_and_hms(2026, 8, 3, 0, 0, 0)
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
        download_url: Some("https://persistent.oaistatic.com/codex.zip".to_owned()),
        download_size: Some(589_926_710),
        signature_present: true,
    }
}
