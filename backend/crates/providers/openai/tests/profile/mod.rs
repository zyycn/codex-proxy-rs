//! 测试模块入口与共享夹具

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{TimeZone, Utc};
use futures::future::BoxFuture;
use gateway_core::provider_ports::{
    ProviderArtifactProfile, ProviderArtifactProfileCachePort, ProviderStoreError,
    ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;
use provider_openai::profile::{
    CodexArtifactProfileCache, CodexBundledReleaseProfile, CodexDesktopRelease,
    CodexDesktopReleaseError, CodexDesktopReleaseService, CodexDesktopReleaseTransport,
    CodexWireProfile, CodexWireProfileState, parse_desktop_release,
};

mod cli_release;
mod desktop_artifact;
mod identity;
mod platform_release;
mod selection;

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
    fail_writes: AtomicBool,
    writes: AtomicUsize,
}

impl ArtifactProfiles {
    fn failing() -> Self {
        Self {
            profile: Mutex::new(None),
            fail_writes: AtomicBool::new(true),
            writes: AtomicUsize::new(0),
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
            self.writes.fetch_add(1, Ordering::SeqCst);
            if self.fail_writes.load(Ordering::SeqCst) {
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
        artifact_key: &'a str,
    ) -> BoxFuture<'a, Result<Option<ProviderArtifactProfile>, ProviderStoreError>> {
        Box::pin(async move {
            Ok(self
                .profile
                .lock()
                .expect("artifact profile")
                .as_ref()
                .filter(|profile| {
                    profile.provider_kind() == provider_kind
                        && profile.artifact_key() == artifact_key
                })
                .cloned())
        })
    }
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
        client_kind: provider_openai::profile::selection::ClientKind::Desktop,
        originator: "Codex Desktop".to_owned(),
        codex_version: "0.147.0-alpha.6.6".to_owned(),
        desktop_version: "26.803.81509".to_owned(),
        desktop_build: "6415".to_owned(),
        os_type: "Mac OS".to_owned(),
        os_version: "15.7.1".to_owned(),
        arch: "arm64".to_owned(),
        terminal: "unknown".to_owned(),
        exact_user_agent: None,
        installation_seed: None,
        residency: None,
        account_routing_override: None,
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

mod state;

mod release;
