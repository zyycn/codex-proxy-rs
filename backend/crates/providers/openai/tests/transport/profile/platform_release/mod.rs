use futures::future::BoxFuture;
use gateway_core::provider_ports::{
    ProviderArtifactProfile, ProviderArtifactProfileCachePort, ProviderStoreError,
};
use gateway_core::routing::ProviderKind;
use provider_openai::transport::profile::CodexWireProfileState;
use provider_openai::transport::profile::platform_release::{
    DesktopArtifactTransport, DesktopTarget, PlatformDesktopReleaseService, TARGETS,
    VerifiedArtifact,
};
use provider_openai::transport::profile::selection::{ClientKind, ClientProfileSelection};
use serde_json::json;
use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod artifact;
mod range;
mod xz;

#[derive(Default)]
struct Cache {
    profiles: Mutex<BTreeMap<String, ProviderArtifactProfile>>,
    reject: bool,
}
impl ProviderArtifactProfileCachePort for Cache {
    fn read<'a>(
        &'a self,
        _provider: &'a ProviderKind,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Option<ProviderArtifactProfile>, ProviderStoreError>> {
        Box::pin(async move { Ok(self.profiles.lock().unwrap().get(key).cloned()) })
    }
    fn replace_if_newer(
        &self,
        profile: ProviderArtifactProfile,
        _ttl: Duration,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async move {
            if self.reject {
                return Ok(false);
            }
            self.profiles
                .lock()
                .unwrap()
                .insert(profile.artifact_key().to_owned(), profile);
            Ok(true)
        })
    }
}
#[derive(Default)]
struct Transport {
    round: Mutex<u32>,
    seen_previous: Mutex<usize>,
}
impl DesktopArtifactTransport for Transport {
    fn fetch(
        &self,
        target: DesktopTarget,
        previous: Option<VerifiedArtifact>,
    ) -> BoxFuture<'_, io::Result<VerifiedArtifact>> {
        Box::pin(async move {
            if previous.is_some() {
                *self.seen_previous.lock().unwrap() += 1;
            }
            let round = *self.round.lock().unwrap();
            if round == 1 && target == TARGETS[0] {
                return Err(io::Error::other("fixture outage"));
            }
            let build = if round == 2 { 1 } else { 20000 + round };
            Ok(serde_json::from_value(json!({"identity":{"etag":format!("\"{build}\""),"size":1000}, "release":{"codexVersion":format!("0.200.{round}"), "desktopVersion":format!("30.1.{round}"), "desktopBuild":build.to_string(), "verifiedAt":null}})).unwrap())
        })
    }
}
fn service(
    state: CodexWireProfileState,
    cache: Arc<Cache>,
    transport: Arc<Transport>,
) -> PlatformDesktopReleaseService {
    PlatformDesktopReleaseService::new(
        ProviderKind::new("openai").unwrap(),
        state,
        cache,
        transport,
    )
}
#[tokio::test]
async fn releases_are_independent_atomic_cached_and_preserved_on_failures_or_rollback() {
    let state = CodexWireProfileState::new(super::wire_profile());
    let cache = Arc::new(Cache::default());
    let transport = Arc::new(Transport::default());
    let service = service(state.clone(), cache.clone(), transport.clone());
    service.refresh().await;
    let selection = ClientProfileSelection {
        platform: TARGETS[0].platform,
        ..Default::default()
    };
    let frozen = selection.resolve(&state).unwrap();
    *transport.round.lock().unwrap() = 1;
    service.refresh().await;
    assert_eq!(selection.resolve(&state).unwrap(), frozen);
    assert!(
        state
            .client_release_status(ClientKind::Desktop, TARGETS[0].platform, TARGETS[0].arch)
            .1
            .is_some()
    );
    assert_eq!(*transport.seen_previous.lock().unwrap(), 8);
    for target in TARGETS.into_iter().skip(1) {
        assert_eq!(
            state
                .client_release(ClientKind::Desktop, target.platform, target.arch)
                .unwrap()
                .codex_version,
            "0.200.1"
        );
    }
    *transport.round.lock().unwrap() = 2;
    service.refresh().await;
    assert_eq!(selection.resolve(&state).unwrap(), frozen);
    let restored = CodexWireProfileState::new(super::wire_profile());
    self::service(restored.clone(), cache, transport)
        .restore()
        .await;
    assert_eq!(selection.resolve(&restored).unwrap(), frozen);
}
#[tokio::test]
async fn cache_rejection_keeps_the_previous_release() {
    let state = CodexWireProfileState::new(super::wire_profile());
    let before = state.client_release(ClientKind::Desktop, TARGETS[0].platform, TARGETS[0].arch);
    service(
        state.clone(),
        Arc::new(Cache {
            reject: true,
            ..Default::default()
        }),
        Arc::new(Transport::default()),
    )
    .refresh()
    .await;
    assert_eq!(
        state.client_release(ClientKind::Desktop, TARGETS[0].platform, TARGETS[0].arch),
        before
    );
}
