//! 缓存恢复保留事实时间，制品、目标或缓存变化不能借用旧核验结果

use crate::{
    profile::{ReleaseService, Selection, artifact::Release, verification::VerificationCache},
    support::profile::cache,
};
use gateway_core::{provider_ports::ProviderArtifactProfile, routing::ProviderKind};
use std::sync::atomic::Ordering;

fn provider() -> ProviderKind {
    ProviderKind::new("antigravity").unwrap()
}
fn release(server: &wiremock::MockServer, version: &str) -> Release {
    Release {
        version: version.into(),
        build: Some("123".into()),
        url: format!("{}/Antigravity.dmg", server.uri()),
        sha512: None,
    }
}

#[tokio::test]
async fn cache_restore_and_renewal_preserve_verification_without_downloading_again() {
    let server = super::archive::serve(super::artifact::desktop_zip("2.35.1"), "").await;
    let cache = cache();
    let service = VerificationCache::new(provider(), cache.clone());
    let client = reqwest::Client::new();
    let target = Selection::default().target();
    let release = release(&server, "2.35.1");
    let first = service
        .verify(&client, target, &release, None)
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap().len();
    let same = service
        .verify(&client, target, &release, Some(&first))
        .await
        .unwrap();
    assert_eq!(same.verified_at, first.verified_at);
    assert_eq!(cache.writes.load(Ordering::SeqCst), 1);
    let restarted = VerificationCache::new(provider(), cache.clone());
    let restored = restarted
        .verify(&client, target, &release, None)
        .await
        .unwrap();
    assert_eq!(restored.verified_at, first.verified_at);
    assert_eq!(cache.writes.load(Ordering::SeqCst), 2);
    assert_eq!(server.received_requests().await.unwrap().len(), requests);

    let releases = ReleaseService::new(cache).unwrap();
    releases.publish(target, Some(release.clone()));
    releases.update().await.unwrap();
    assert_eq!(
        crate::profile::dashboard(&Selection::default(), &releases).verified_at,
        Some(first.verified_at)
    );
    let new_release = Release {
        version: "2.36.0".into(),
        ..release
    };
    releases.publish(target, Some(new_release));
    assert!(releases.update().await.is_err());
    assert!(
        crate::profile::dashboard(&Selection::default(), &releases)
            .verified_at
            .is_none()
    );
}

#[tokio::test]
async fn changed_artifact_or_target_and_failed_cache_do_not_publish_verification() {
    let server = super::archive::serve(super::artifact::desktop_zip("2.35.1"), "").await;
    let cache = cache();
    let service = VerificationCache::new(provider(), cache.clone());
    let client = reqwest::Client::new();
    let target = Selection::default().target();
    let release = release(&server, "2.35.1");
    service
        .verify(&client, target, &release, None)
        .await
        .unwrap();
    let original = cache
        .profiles
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    let changed = Release {
        build: Some("124".into()),
        ..release.clone()
    };
    assert!(
        service
            .verify(&client, target, &changed, None)
            .await
            .is_err()
    );
    cache.fail.store(true, Ordering::SeqCst);
    assert!(
        service
            .verify(&client, target, &release, None)
            .await
            .is_err()
    );
    cache.fail.store(false, Ordering::SeqCst);
    let corrupted = ProviderArtifactProfile::new(
        ProviderKind::new("openai").unwrap(),
        original.artifact_key().into(),
        original.artifact_sequence(),
        original.verified_at(),
        original.profile().clone(),
    );
    cache
        .profiles
        .lock()
        .unwrap()
        .insert(original.artifact_key().into(), corrupted);
    assert!(
        service
            .verify(&client, target, &release, None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn version_disagreement_leaves_the_previous_cache_intact() {
    let server = super::archive::serve(super::artifact::desktop_zip("2.35.1"), "").await;
    let cache = cache();
    let service = VerificationCache::new(provider(), cache.clone());
    let client = reqwest::Client::new();
    let target = Selection::default().target();
    let release = release(&server, "2.35.1");
    let first = service
        .verify(&client, target, &release, None)
        .await
        .unwrap();
    let newer = Release {
        version: "2.36.0".into(),
        ..release
    };
    assert!(
        service
            .verify(&client, target, &newer, Some(&first))
            .await
            .is_err()
    );
    assert_eq!(cache.writes.load(Ordering::SeqCst), 1);
    let old = cache
        .profiles
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(
        chrono::DateTime::<chrono::Utc>::from(old.verified_at()),
        first.verified_at
    );
}
