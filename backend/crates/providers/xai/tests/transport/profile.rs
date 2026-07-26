use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use futures::future::BoxFuture;
use provider_xai::{
    GrokCliReleaseError, GrokCliReleaseService, GrokCliReleaseTransport, XaiWireProfileConfig,
    XaiWireProfileState,
};

struct ReleaseTransport {
    outcomes: Mutex<VecDeque<Result<String, GrokCliReleaseError>>>,
}

impl ReleaseTransport {
    fn new(outcomes: impl IntoIterator<Item = Result<String, GrokCliReleaseError>>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
        }
    }
}

impl GrokCliReleaseTransport for ReleaseTransport {
    fn fetch(&self) -> BoxFuture<'_, Result<String, GrokCliReleaseError>> {
        let outcome = self
            .outcomes
            .lock()
            .expect("release outcomes lock")
            .pop_front()
            .expect("release outcome");
        Box::pin(async move { outcome })
    }
}

#[tokio::test]
async fn release_refresh_should_update_profile_and_observation() {
    let profile = wire_profile();
    let service = GrokCliReleaseService::new(
        profile.clone(),
        Arc::new(ReleaseTransport::new([Ok("0.2.112".to_owned())])),
    );
    let status = service.status();

    assert_eq!(service.refresh().await.expect("release"), "0.2.112");
    assert_eq!(profile.client_version(), "0.2.112");
    let snapshot = status.snapshot();
    assert!(snapshot.checked_at.is_some());
    assert_eq!(snapshot.latest_version.as_deref(), Some("0.2.112"));
    assert!(snapshot.last_error.is_none());
}

#[tokio::test]
async fn failed_release_refresh_should_keep_last_successful_version() {
    let profile = wire_profile();
    let service = GrokCliReleaseService::new(
        profile.clone(),
        Arc::new(ReleaseTransport::new([
            Ok("0.2.112".to_owned()),
            Err(GrokCliReleaseError::InvalidDocument),
        ])),
    );
    let status = service.status();

    service.refresh().await.expect("first release");
    assert!(matches!(
        service.refresh().await,
        Err(GrokCliReleaseError::InvalidDocument)
    ));
    assert_eq!(profile.client_version(), "0.2.112");
    let snapshot = status.snapshot();
    assert!(snapshot.checked_at.is_some());
    assert_eq!(snapshot.latest_version.as_deref(), Some("0.2.112"));
    assert_eq!(
        snapshot.last_error.as_deref(),
        Some("Grok CLI release document is invalid")
    );
}

fn wire_profile() -> XaiWireProfileState {
    XaiWireProfileState::new(XaiWireProfileConfig {
        client_identifier: "grok-shell".to_owned(),
        client_version: "0.2.106".to_owned(),
        client_mode: "headless".to_owned(),
        target_os: "linux".to_owned(),
        target_arch: "x86_64".to_owned(),
        verified_at: Utc::now(),
    })
}
