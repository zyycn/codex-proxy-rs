use chrono::{Duration, TimeZone, Utc};
use codex_proxy_rs::accounts::cookies::CloudflareChallengeCooldownTracker;

#[tokio::test]
async fn challenge_cooldown_should_escalate_and_cap_delay() {
    let tracker = CloudflareChallengeCooldownTracker::new();
    let now = Utc.with_ymd_and_hms(2026, 6, 20, 10, 0, 0).unwrap();

    let first = tracker.record_challenge("acct_a", now).await;
    let second = tracker
        .record_challenge("acct_a", now + Duration::seconds(1))
        .await;
    let third = tracker
        .record_challenge("acct_a", now + Duration::seconds(2))
        .await;
    let fourth = tracker
        .record_challenge("acct_a", now + Duration::seconds(3))
        .await;
    let fifth = tracker
        .record_challenge("acct_a", now + Duration::seconds(4))
        .await;

    assert_eq!(
        (
            first.delay_seconds,
            second.delay_seconds,
            third.delay_seconds,
            fourth.delay_seconds,
            fifth.delay_seconds,
        ),
        (10, 30, 90, 120, 120)
    );
}

#[tokio::test]
async fn challenge_cooldown_should_reset_after_stale_window() {
    let tracker = CloudflareChallengeCooldownTracker::new();
    let now = Utc.with_ymd_and_hms(2026, 6, 20, 10, 0, 0).unwrap();

    let first = tracker.record_challenge("acct_a", now).await;
    let after_stale = tracker
        .record_challenge("acct_a", now + Duration::hours(2))
        .await;

    assert_eq!(
        (
            first.challenge_count,
            first.delay_seconds,
            after_stale.challenge_count,
            after_stale.delay_seconds,
        ),
        (1, 10, 1, 10)
    );
}
