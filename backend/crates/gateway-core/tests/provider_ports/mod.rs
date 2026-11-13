//! 验证 Provider 协调端口的敏感值保护、有效期与刷新边界

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::time::{Duration, SystemTime};

use gateway_core::account::{
    AccountRuntimeSignals, CredentialRevision, OpaqueProviderData, ProviderAccountId,
};
use gateway_core::provider_ports::{
    NewOAuthPendingFlow, OAuthPendingBinding, ProviderRefreshPolicy, ProviderSchedulingState,
    ProviderSessionAffinityKey, ProviderStoreErrorKind,
};
use gateway_core::routing::ProviderKind;

#[test]
fn oauth_pending_binding_debug_redacts_raw_value() {
    let binding = OAuthPendingBinding::try_new("must-not-appear").expect("valid binding");

    assert_eq!(format!("{binding:?}"), "OAuthPendingBinding([REDACTED])");
}

#[test]
fn provider_session_affinity_key_debug_is_opaque() {
    let key = ProviderSessionAffinityKey::try_new("opaque-session-key").expect("valid key");

    assert_eq!(format!("{key:?}"), "ProviderSessionAffinityKey([OPAQUE])");
}

#[test]
fn oauth_pending_ttl_rejects_zero_and_more_than_thirty_minutes() {
    let provider = ProviderKind::new("fixture").expect("valid provider");
    let flow = OAuthPendingBinding::try_new("flow").expect("valid flow");
    let owner = OAuthPendingBinding::try_new("owner").expect("valid owner");
    let payload = OpaqueProviderData::new(serde_json::Map::new());

    for ttl in [Duration::ZERO, Duration::from_secs(30 * 60 + 1)] {
        let error = NewOAuthPendingFlow::try_new(
            provider.clone(),
            flow.clone(),
            owner.clone(),
            ttl,
            payload.clone(),
        )
        .expect_err("invalid TTL must fail");
        assert_eq!(error.kind(), ProviderStoreErrorKind::InvalidData);
    }
}

#[test]
fn refresh_policy_requires_a_positive_margin() {
    let error = ProviderRefreshPolicy::try_new(
        Duration::ZERO,
        NonZeroU32::new(1).expect("positive concurrency"),
    )
    .expect_err("zero margin must fail");

    assert_eq!(error.kind(), ProviderStoreErrorKind::InvalidData);
}

#[test]
fn refresh_policy_should_mark_tokens_due_at_the_exact_configured_margin() {
    let policy = ProviderRefreshPolicy::try_new(
        Duration::from_secs(3_600),
        NonZeroU32::new(2).expect("positive concurrency"),
    )
    .expect("valid policy");
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
    let expires_at = observed_at + Duration::from_secs(7_200);

    assert!(!policy.is_refresh_due(expires_at, observed_at));
    assert!(policy.is_refresh_due(observed_at + Duration::from_secs(3_600), observed_at));
}

#[test]
fn refresh_policy_should_mark_expired_tokens_due() {
    let policy = ProviderRefreshPolicy::try_new(
        Duration::from_secs(3_600),
        NonZeroU32::new(1).expect("positive concurrency"),
    )
    .expect("valid policy");
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);

    assert!(policy.is_refresh_due(observed_at - Duration::from_secs(1), observed_at));
}

#[test]
fn refresh_stagger_is_stable_and_bounded_by_the_margin() {
    let policy = ProviderRefreshPolicy::try_new(
        Duration::from_secs(300),
        NonZeroU32::new(1).expect("positive concurrency"),
    )
    .expect("valid policy");
    let account = ProviderAccountId::new("acct_stagger").expect("valid account");

    let first = policy.refresh_stagger(&account);
    for _ in 0..10 {
        assert_eq!(policy.refresh_stagger(&account), first);
    }
    assert!(first <= policy.margin());
    // 偏移随 margin 缩放，不超出新窗口。
    let widened = ProviderRefreshPolicy::try_new(
        Duration::from_secs(3_600),
        NonZeroU32::new(1).expect("positive concurrency"),
    )
    .expect("valid policy");
    assert!(widened.refresh_stagger(&account) <= widened.margin());
}

#[test]
fn refresh_stagger_should_spread_accounts_across_the_margin() {
    let policy = ProviderRefreshPolicy::try_new(
        Duration::from_secs(300),
        NonZeroU32::new(1).expect("positive concurrency"),
    )
    .expect("valid policy");
    let staggers = (0..64)
        .map(|index| {
            let account =
                ProviderAccountId::new(format!("acct_spread_{index}")).expect("valid account");
            let stagger = policy.refresh_stagger(&account);
            assert!(stagger <= policy.margin());
            stagger
        })
        .collect::<Vec<_>>();

    // 确定性哈希下的分散性：64 个账号在 301 个可能取值中覆盖足够多的档位。
    let distinct = staggers.iter().collect::<BTreeSet<_>>().len();
    assert!(
        distinct >= 24,
        "stagger values are too concentrated: {staggers:?}"
    );
}

#[test]
fn refresh_policy_staggered_due_boundaries() {
    let policy = ProviderRefreshPolicy::try_new(
        Duration::from_secs(300),
        NonZeroU32::new(2).expect("positive concurrency"),
    )
    .expect("valid policy");
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);

    for index in 0..8 {
        let account =
            ProviderAccountId::new(format!("acct_boundary_{index}")).expect("valid account");
        // 恰好 margin：任何偏移下都到期；2×margin+1 秒：任何偏移下都未到期。
        assert!(policy.is_refresh_due_staggered(
            &account,
            observed_at + Duration::from_secs(300),
            observed_at,
        ));
        assert!(!policy.is_refresh_due_staggered(
            &account,
            observed_at + Duration::from_secs(601),
            observed_at,
        ));
        // 已过期账号不受偏移影响，恒到期。
        assert!(policy.is_refresh_due_staggered(
            &account,
            observed_at - Duration::from_secs(1),
            observed_at,
        ));
    }
}

#[test]
fn refresh_policy_staggered_due_matches_margin_plus_stagger() {
    let policy = ProviderRefreshPolicy::try_new(
        Duration::from_secs(300),
        NonZeroU32::new(1).expect("positive concurrency"),
    )
    .expect("valid policy");
    let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);

    for index in 0..4 {
        let account =
            ProviderAccountId::new(format!("acct_compose_{index}")).expect("valid account");
        let staggered_margin = policy.margin() + policy.refresh_stagger(&account);
        for remaining_secs in [299, 300, 301, 400, 450, 500, 599, 600, 601] {
            assert_eq!(
                policy.is_refresh_due_staggered(
                    &account,
                    observed_at + Duration::from_secs(remaining_secs),
                    observed_at,
                ),
                Duration::from_secs(remaining_secs) <= staggered_margin,
                "remaining {remaining_secs}s"
            );
        }
    }
}

#[test]
fn scheduling_state_preserves_provider_neutral_signals() {
    let account = ProviderAccountId::new("acct_fixture").expect("valid account");
    let signals = BTreeMap::from([(
        account.clone(),
        AccountRuntimeSignals {
            in_flight: 2,
            last_started_at: None,
            quota_reset_at: None,
            quota_reset_windows: None,
            quota_remaining_rank: Some(7),
            cooldown: None,
            failure_rate_basis_points: Some(125),
            first_output_latency_ms: Some(250),
        },
    )]);
    let state = ProviderSchedulingState::new(signals, 9);

    assert_eq!(state.signals()[&account].in_flight, 2);
    assert_eq!(
        state.signals()[&account].failure_rate_basis_points,
        Some(125)
    );
    assert_eq!(state.round_robin_cursor(), 9);
    assert_eq!(
        CredentialRevision::new(1).expect("positive revision").get(),
        1
    );
}

#[test]
fn warmup_cron_validation_and_calendar_matching() {
    use chrono::NaiveDate;
    use gateway_core::provider_ports::{ProviderWarmupPolicy, valid_warmup_cron};

    for value in [
        "0 8 * * *",
        "*/15 8-18 * * MON-FRI",
        "0 8,13 * * MON-FRI",
        "0 0 29 FEB *",
    ] {
        assert!(valid_warmup_cron(value), "{value}");
    }
    for value in [
        "",
        "08:00",
        "08:00,13:00",
        "60 8 * * *",
        "0 24 * * *",
        "0 8 * *",
        "0 0 8 * * *",
        "0 8 * * * *",
        "@daily",
        "0 8 * * *\n30 13 * * *",
        "0 8 * * *\r\n30 13 * * *",
        "0 8 * * *\t",
        " 0 8 * * *",
        "0 8 * * *\n",
    ] {
        assert!(!valid_warmup_cron(value), "{value:?}");
    }
    let policy =
        ProviderWarmupPolicy::try_new(true, "0 8,13 * * MON-FRI", Some("test-model".to_owned()))
            .unwrap();
    assert!(policy.enabled());
    assert_eq!(policy.model(), Some("test-model"));
    for (day, hour, minute, expected) in [
        (9, 8, 0, true),
        (9, 13, 0, true),
        (9, 8, 30, false),
        (9, 13, 30, false),
        (14, 8, 0, false),
        (14, 13, 30, false),
    ] {
        let slot = NaiveDate::from_ymd_opt(2026, 11, day)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap();
        assert_eq!(policy.matches_slot(slot), expected, "{slot}");
    }
    assert!(ProviderWarmupPolicy::try_new(false, "", None).is_ok());
    assert!(ProviderWarmupPolicy::try_new(true, "", Some("test-model".to_owned())).is_err());
    assert!(ProviderWarmupPolicy::try_new(false, "08:00", None).is_err());
    assert!(!valid_warmup_cron(&format!("0{}8 * * *", " ".repeat(256))));
    let leap = ProviderWarmupPolicy::try_new(false, "0 0 29 FEB *", None).unwrap();
    let slot = NaiveDate::from_ymd_opt(2028, 2, 29)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    assert!(leap.matches_slot(slot));
    assert!(!leap.matches_slot(slot + chrono::Duration::days(1)));
    assert!(ProviderWarmupPolicy::try_new(true, "invalid", None).is_err());
    assert!(ProviderWarmupPolicy::try_new(true, "0 8 * * *", None).is_err());
}

#[test]
fn warmup_matches_only_the_scheduled_hour_and_minute_pairs() {
    use chrono::NaiveDate;
    use gateway_core::provider_ports::ProviderWarmupPolicy;

    let policy = ProviderWarmupPolicy::try_new(
        true,
        "58 5 * * *;59 10 * * *;0 16 * * *;1 21 * * *;0 16 * * MON-FRI",
        Some("test-model".to_owned()),
    )
    .unwrap();
    let date = NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
    for minute in 0..1440 {
        let slot = date.and_hms_opt(minute / 60, minute % 60, 0).unwrap();
        assert_eq!(
            policy.matches_slot(slot),
            [358, 659, 960, 1261].contains(&minute),
            "{slot}"
        );
    }
}
