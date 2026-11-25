//! 对应生产职责的行为测试

use super::*;

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
