//! 官方发行解析、刷新与原子更新测试

use super::*;

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
