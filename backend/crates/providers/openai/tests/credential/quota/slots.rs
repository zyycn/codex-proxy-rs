//! 各额度槽独立恢复，跨刷新保留未解除的限制。

use super::*;

const SHORT_RESET: i64 = 1_700_000_000;
const WEEK_RESET: i64 = SHORT_RESET + 604_800;

fn usage(short: (u64, i64), weekly: (u64, i64)) -> serde_json::Value {
    json!({"rate_limit": {
        "primary_window": {"used_percent": short.0, "reset_at": short.1, "limit_window_seconds": 18_000},
        "secondary_window": {"used_percent": weekly.0, "reset_at": weekly.1, "limit_window_seconds": 604_800}
    }})
}

async fn mount_usage(server: &MockServer, value: serde_json::Value) {
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(value))
        .mount(server)
        .await;
}

#[tokio::test]
async fn recovery_checks_each_exhausted_slot_in_worker_and_manual_refresh() {
    for (name, old_short, old_week, short, week, allowed, recovered) in [
        (
            "short_only",
            100,
            45,
            (9, SHORT_RESET + 18_000),
            (45, WEEK_RESET),
            false,
            true,
        ),
        (
            "weekly_only",
            88,
            100,
            (88, SHORT_RESET),
            (9, WEEK_RESET + 604_800),
            false,
            true,
        ),
        (
            "short_cannot_unlock_week",
            100,
            100,
            (0, SHORT_RESET + 18_000),
            (100, WEEK_RESET),
            true,
            false,
        ),
        (
            "week_cannot_unlock_short",
            100,
            100,
            (100, SHORT_RESET),
            (0, WEEK_RESET + 604_800),
            true,
            false,
        ),
        (
            "both_reset",
            100,
            100,
            (0, SHORT_RESET + 18_000),
            (9, WEEK_RESET + 604_800),
            false,
            true,
        ),
        (
            "same_reset",
            100,
            45,
            (0, SHORT_RESET),
            (45, WEEK_RESET),
            true,
            false,
        ),
        (
            "backward_reset",
            100,
            45,
            (0, SHORT_RESET - 1),
            (45, WEEK_RESET),
            true,
            false,
        ),
        (
            "exactly_ten",
            100,
            45,
            (10, SHORT_RESET + 18_000),
            (45, WEEK_RESET),
            true,
            false,
        ),
        (
            "high_usage",
            100,
            45,
            (98, SHORT_RESET + 18_000),
            (45, WEEK_RESET),
            true,
            false,
        ),
        (
            "week_now_full",
            100,
            45,
            (0, SHORT_RESET + 18_000),
            (100, WEEK_RESET),
            true,
            false,
        ),
    ] {
        let name = format!("acct_{name}");
        for worker in [false, true] {
            let store = Arc::new(MemoryAccountStore::default());
            create_account(&store, &name).await;
            let account = store.account(&name).expect("account");
            let server = MockServer::start().await;
            let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
            let mut old = usage((old_short, SHORT_RESET), (old_week, WEEK_RESET));
            old["rate_limit"]["allowed"] = json!(false);
            mount_usage(&server, old).await;
            service
                .refresh_account(account.id())
                .await
                .expect("seed exhausted quota");
            let mut new = usage(short, week);
            new["rate_limit"]["allowed"] = json!(allowed);
            mount_usage(&server, new).await;
            if worker {
                let summary = service.synchronize().await.expect("worker refresh");
                assert_eq!(summary.updated, u64::from(recovered), "{name}");
                assert_eq!(summary.exhausted, u64::from(!recovered), "{name}");
            } else {
                service
                    .refresh_account(account.id())
                    .await
                    .expect("manual refresh");
            }
            assert_eq!(
                store
                    .account(&name)
                    .expect("updated account")
                    .quota()
                    .is_exhausted(),
                !recovered,
                "{name}, worker={worker}"
            );
            let snapshot = service
                .read_account(account.id())
                .await
                .expect("read quota")
                .expect("snapshot");
            for (seconds, expected) in [(18_000, short.0), (604_800, week.0)] {
                let slot = snapshot
                    .windows()
                    .iter()
                    .find(|w| w.window_seconds() == Some(seconds))
                    .expect("slot");
                assert_eq!(
                    slot.used_percent(),
                    Some(expected as f64),
                    "latest usage: {name}"
                );
            }
        }
    }
}

#[tokio::test]
async fn worker_detects_early_resets_without_unlocking_other_exhausted_windows() {
    let short_reset = Utc::now().timestamp() + 18_000;
    let week_reset = Utc::now().timestamp() + 604_800;
    for (name, old_short, old_week, new_short, new_week, recovered) in [
        ("short_only", Some(100), None, Some(0), None, true),
        ("weekly_only", None, Some(100), None, Some(0), true),
        (
            "short_with_weekly_available",
            Some(100),
            Some(45),
            Some(0),
            Some(45),
            true,
        ),
        (
            "weekly_still_exhausted",
            Some(100),
            Some(100),
            Some(0),
            Some(100),
            false,
        ),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        let account_id = format!("acct_early_{name}");
        create_account(&store, &account_id).await;
        let account = store.account(&account_id).expect("account");
        let server = MockServer::start().await;
        let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
        let window = |used: Option<u64>, reset, seconds| {
            used.map(|used| {
                json!({
                    "used_percent": used,
                    "reset_at": reset,
                    "limit_window_seconds": seconds,
                })
            })
        };
        mount_usage(
            &server,
            json!({"rate_limit": {
                "allowed": false,
                "primary_window": window(old_short, short_reset, 18_000),
                "secondary_window": window(old_week, week_reset, 604_800),
            }}),
        )
        .await;
        let baseline = service
            .refresh_account(account.id())
            .await
            .expect("seed exhaustion");
        assert!(baseline.quota().reset_at().expect("future reset") > SystemTime::now());

        mount_usage(
            &server,
            json!({"rate_limit": {
                "allowed": recovered,
                "primary_window": window(new_short, short_reset + 18_000, 18_000),
                "secondary_window": window(new_week, if new_week == Some(0) {
                    week_reset + 604_800
                } else {
                    week_reset
                }, 604_800),
            }}),
        )
        .await;
        let summary = service
            .synchronize()
            .await
            .expect("periodic early reset check");
        assert_eq!(summary.updated, u64::from(recovered), "{name}");
        assert_eq!(summary.exhausted, u64::from(!recovered), "{name}");
        assert_eq!(
            store
                .account(&account_id)
                .expect("updated account")
                .quota()
                .is_exhausted(),
            !recovered,
            "{name}"
        );

        // 未恢复的周窗口仍在未来，也必须保留复核节流；恢复后则退出定时复核。
        service.synchronize().await.expect("next worker cycle");
        assert_eq!(
            server.received_requests().await.expect("requests").len(),
            1,
            "{name}"
        );
    }
}

#[tokio::test]
async fn partial_recovery_survives_restart_missing_windows_and_changed_roles() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_partial").await;
    let account = store.account("acct_partial").expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    let mut old = usage((100, SHORT_RESET), (100, WEEK_RESET));
    old["rate_limit"]["allowed"] = json!(false);
    mount_usage(&server, old).await;
    service
        .refresh_account(account.id())
        .await
        .expect("seed exhausted quota");
    mount_usage(&server, usage((1, SHORT_RESET + 18_000), (100, WEEK_RESET))).await;
    let partial = service
        .refresh_account(account.id())
        .await
        .expect("recover 5h only");
    assert!(partial.quota().is_exhausted());
    assert_eq!(
        partial.quota().reset_at(),
        Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(WEEK_RESET as u64))
    );
    drop(service);
    let restarted = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    let mut missing = usage((50, SHORT_RESET + 18_000), (0, WEEK_RESET + 604_800));
    let weekly = missing["rate_limit"]
        .as_object_mut()
        .expect("limit")
        .remove("secondary_window")
        .expect("weekly");
    missing["rate_limit"]["allowed"] = json!(true);
    missing["additional_rate_limits"] =
        json!([{"limit_name": "separate_model", "rate_limit": {"primary_window": weekly}}]);
    mount_usage(&server, missing).await;
    assert!(
        restarted
            .refresh_account(account.id())
            .await
            .expect("missing weekly")
            .quota()
            .is_exhausted()
    );
    let mut new = usage((50, SHORT_RESET + 18_000), (9, WEEK_RESET + 604_800));
    let limit = new["rate_limit"].as_object_mut().expect("limit");
    let short = limit.remove("primary_window").expect("short");
    let weekly = limit.remove("secondary_window").expect("weekly");
    limit.insert("primary_window".into(), weekly);
    limit.insert("secondary_window".into(), short);
    mount_usage(&server, new).await;
    assert_eq!(
        restarted
            .refresh_account(account.id())
            .await
            .expect("weekly recovered")
            .quota()
            .access(),
        QuotaAccessState::Allowed
    );
}

#[tokio::test]
async fn weekly_error_reset_identifies_the_slot_before_display_reaches_one_hundred() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_weekly_error").await;
    let account = store.account("acct_weekly_error").expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    let mut old = usage((88, SHORT_RESET), (99, WEEK_RESET));
    old["rate_limit"]["allowed"] = json!(true);
    mount_usage(&server, old).await;
    service
        .refresh_account(account.id())
        .await
        .expect("seed allowed quota");
    persist_quota_state(
        &store,
        &account,
        exhausted_quota(Some(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs((WEEK_RESET - 1) as u64),
        )),
    )
    .await;
    mount_usage(&server, usage((88, SHORT_RESET), (0, WEEK_RESET + 604_800))).await;
    assert_eq!(
        service
            .refresh_account(account.id())
            .await
            .expect("weekly recovered")
            .quota()
            .access(),
        QuotaAccessState::Allowed
    );
}

#[tokio::test]
async fn recovery_retains_reset_baseline_until_usage_is_below_ten() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_baseline").await;
    let account = store.account("acct_baseline").expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    let mut old = usage((100, SHORT_RESET), (45, WEEK_RESET));
    old["rate_limit"]["allowed"] = json!(false);
    mount_usage(&server, old).await;
    service
        .refresh_account(account.id())
        .await
        .expect("seed exhausted quota");
    for used in [98, 10, 9] {
        mount_usage(
            &server,
            usage((used, SHORT_RESET + 18_000), (45, WEEK_RESET)),
        )
        .await;
        let snapshot = service
            .refresh_account(account.id())
            .await
            .expect("refresh quota");
        assert_eq!(snapshot.quota().is_exhausted(), used >= 10);
    }
}

#[tokio::test]
async fn secondary_window_limit_signal_does_not_mark_the_primary_window_exhausted() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_secondary_signal").await;
    let account = store.account("acct_secondary_signal").expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    let mut old = usage((88, SHORT_RESET), (99, WEEK_RESET));
    old["rate_limit"]["secondary_window"]["limit_reached"] = json!(true);
    mount_usage(&server, old).await;
    let snapshot = service
        .refresh_account(account.id())
        .await
        .expect("observe weekly exhaustion");
    assert!(snapshot.quota().is_exhausted());
    assert_eq!(
        snapshot.quota().reset_at(),
        Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(WEEK_RESET as u64))
    );
    assert!(
        !snapshot
            .windows()
            .iter()
            .find(|w| w.window_seconds() == Some(18_000))
            .expect("5h slot")
            .limit_reached()
    );
    mount_usage(&server, usage((88, SHORT_RESET), (0, WEEK_RESET + 604_800))).await;
    assert_eq!(
        service
            .refresh_account(account.id())
            .await
            .expect("weekly recovered")
            .quota()
            .access(),
        QuotaAccessState::Allowed
    );
}

#[tokio::test]
async fn a_new_exhaustion_cannot_reuse_partial_recovery_from_the_previous_failure() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_new_exhaustion").await;
    let account = store.account("acct_new_exhaustion").expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    let mut old = usage((100, SHORT_RESET), (100, WEEK_RESET));
    old["rate_limit"]["allowed"] = json!(false);
    mount_usage(&server, old).await;
    service
        .refresh_account(account.id())
        .await
        .expect("seed exhausted quota");
    mount_usage(&server, usage((0, SHORT_RESET + 18_000), (100, WEEK_RESET))).await;
    service
        .refresh_account(account.id())
        .await
        .expect("recover short slot");
    persist_quota_state(&store, &account, QuotaState::allowed(SystemTime::now())).await;
    persist_quota_state(
        &store,
        &account,
        exhausted_quota(Some(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs((SHORT_RESET + 18_000) as u64),
        )),
    )
    .await;
    mount_usage(
        &server,
        usage((0, SHORT_RESET + 18_000), (0, WEEK_RESET + 604_800)),
    )
    .await;
    assert!(
        service
            .refresh_account(account.id())
            .await
            .expect("same short reset cannot recover new failure")
            .quota()
            .is_exhausted()
    );
    mount_usage(
        &server,
        usage((0, SHORT_RESET + 36_000), (50, WEEK_RESET + 604_800)),
    )
    .await;
    assert_eq!(
        service
            .refresh_account(account.id())
            .await
            .expect("new short reset")
            .quota()
            .access(),
        QuotaAccessState::Allowed
    );
}

#[tokio::test]
async fn stored_weekly_exhaustion_ignores_the_old_primary_only_account_reset() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_old_weekly").await;
    let account = store.account("acct_old_weekly").expect("account");
    let observed_at = SystemTime::now();
    let mut old = usage((88, SHORT_RESET), (100, WEEK_RESET));
    old["rate_limit"]["allowed"] = json!(false);
    store
        .compare_and_swap_quota(gateway_core::account::QuotaObservation {
            plan_type: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: gateway_core::account::OpaqueProviderData::new(
                old.as_object().expect("document").clone(),
            ),
            observed_at,
            state: QuotaState::exhausted(
                QuotaEvidence::ProviderDenied,
                observed_at,
                Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(SHORT_RESET as u64)),
            ),
        })
        .await
        .expect("seed old persisted quota");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    mount_usage(&server, usage((88, SHORT_RESET), (0, WEEK_RESET + 604_800))).await;
    assert_eq!(
        service
            .refresh_account(account.id())
            .await
            .expect("recover stored weekly exhaustion")
            .quota()
            .access(),
        QuotaAccessState::Allowed
    );
}

#[tokio::test]
async fn an_unknown_slot_establishes_a_baseline_before_it_can_recover() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_unknown_slot").await;
    let account = store.account("acct_unknown_slot").expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    mount_usage(&server, json!({"rate_limit": {"allowed": false}})).await;
    service
        .refresh_account(account.id())
        .await
        .expect("exhaustion without windows");
    for (window, recovered) in [
        (json!({"used_percent": 0}), false),
        (json!({"used_percent": 0, "reset_at": SHORT_RESET}), false),
        (json!({"reset_at": SHORT_RESET + 18_000}), false),
        (
            json!({"used_percent": 9, "reset_at": SHORT_RESET + 18_000}),
            true,
        ),
    ] {
        mount_usage(
            &server,
            json!({"rate_limit": {"allowed": true, "primary_window": window}}),
        )
        .await;
        assert_eq!(
            service
                .refresh_account(account.id())
                .await
                .expect("observe window")
                .quota()
                .is_exhausted(),
            !recovered
        );
    }
}

#[tokio::test]
async fn weekly_usage_falling_back_within_the_same_window_recovers_after_two_observations() {
    // 滑动周窗口的用量可以在同一窗口内回落：reset 不滚动、用量从 100% 降到
    // 正常水平。此时"reset 前进 + 低用量"永远不会成立，解除依赖连续两次
    // 观测都未触顶；单次观测无法排除耗尽后立刻拉到的旧快照，保持锁定。
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_weekly_fallout").await;
    let account = store.account("acct_weekly_fallout").expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    let mut old = usage((100, SHORT_RESET), (100, WEEK_RESET));
    old["rate_limit"]["allowed"] = json!(false);
    old["rate_limit"]["limit_reached"] = json!(true);
    mount_usage(&server, old).await;
    service
        .refresh_account(account.id())
        .await
        .expect("seed exhausted quota");

    // 5h 窗口滚动前进，周窗口 reset 不变但用量回落到正常水平：只标记候选。
    let mut first_observation = usage((0, SHORT_RESET + 18_000), (18, WEEK_RESET));
    first_observation["rate_limit"]["allowed"] = json!(true);
    mount_usage(&server, first_observation).await;
    let first = service
        .refresh_account(account.id())
        .await
        .expect("first fallout observation");
    assert!(first.quota().is_exhausted());

    // 第二次连续观测仍未触顶：解除周窗口。
    let mut second_observation = usage((9, SHORT_RESET + 36_000), (18, WEEK_RESET));
    second_observation["rate_limit"]["allowed"] = json!(true);
    mount_usage(&server, second_observation).await;
    let second = service
        .refresh_account(account.id())
        .await
        .expect("second fallout observation");
    assert_eq!(second.quota().access(), QuotaAccessState::Allowed);
}

#[tokio::test]
async fn a_single_same_window_fallout_observation_cannot_break_the_reset_baseline() {
    // 连续性中断（观测到重新触顶）后必须重新积累两次未触顶证据。
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_weekly_fallout_gap").await;
    let account = store.account("acct_weekly_fallout_gap").expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    let mut old = usage((100, SHORT_RESET), (100, WEEK_RESET));
    old["rate_limit"]["allowed"] = json!(false);
    mount_usage(&server, old).await;
    service
        .refresh_account(account.id())
        .await
        .expect("seed exhausted quota");

    mount_usage(&server, usage((0, SHORT_RESET + 18_000), (18, WEEK_RESET))).await;
    assert!(
        service
            .refresh_account(account.id())
            .await
            .expect("first candidate")
            .quota()
            .is_exhausted()
    );
    // 观测到重新触顶，候选证据清零。
    mount_usage(&server, usage((0, SHORT_RESET + 36_000), (100, WEEK_RESET))).await;
    assert!(
        service
            .refresh_account(account.id())
            .await
            .expect("reached again")
            .quota()
            .is_exhausted()
    );
    mount_usage(&server, usage((0, SHORT_RESET + 54_000), (18, WEEK_RESET))).await;
    assert!(
        service
            .refresh_account(account.id())
            .await
            .expect("candidate restarts")
            .quota()
            .is_exhausted()
    );
    mount_usage(&server, usage((0, SHORT_RESET + 72_000), (18, WEEK_RESET))).await;
    assert_eq!(
        service
            .refresh_account(account.id())
            .await
            .expect("second consecutive observation")
            .quota()
            .access(),
        QuotaAccessState::Allowed
    );
}

#[tokio::test]
async fn monthly_and_arbitrary_periods_use_the_same_recovery_rule() {
    for seconds in [18_000, 86_400, 604_800, 864_000, 2_592_000, 7_776_000] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_generic_period").await;
        let account = store.account("acct_generic_period").expect("account");
        let server = MockServer::start().await;
        let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
        for (used, reset, recovered) in [
            (100, SHORT_RESET, false),
            (0, SHORT_RESET, false),
            (10, SHORT_RESET + seconds, false),
            (1, SHORT_RESET + seconds, true),
        ] {
            mount_usage(&server, json!({"rate_limit": {
                "allowed": false,
                "primary_window": {"used_percent": used, "reset_at": reset, "limit_window_seconds": seconds},
                "secondary_window": null
            }})).await;
            let snapshot = service
                .refresh_account(account.id())
                .await
                .expect("refresh quota");
            assert_eq!(
                snapshot.quota().is_exhausted(),
                !recovered,
                "period={seconds}, used={used}"
            );
        }
    }
}

#[tokio::test]
async fn arbitrary_periods_keep_their_identity_when_roles_change() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_generic_roles").await;
    let account = store.account("acct_generic_roles").expect("account");
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    let mut old = usage((100, SHORT_RESET), (50, WEEK_RESET));
    old["rate_limit"]["allowed"] = json!(false);
    old["rate_limit"]["primary_window"]["limit_window_seconds"] = json!(864_000);
    old["rate_limit"]["secondary_window"]["limit_window_seconds"] = json!(172_800);
    mount_usage(&server, old.clone()).await;
    service
        .refresh_account(account.id())
        .await
        .expect("seed 10-day exhaustion");
    let mut new = old.clone();
    new["rate_limit"]["primary_window"] = old["rate_limit"]["secondary_window"].clone();
    new["rate_limit"]["secondary_window"] = old["rate_limit"]["primary_window"].clone();
    new["rate_limit"]["secondary_window"]["used_percent"] = json!(9);
    new["rate_limit"]["secondary_window"]["reset_at"] = json!(SHORT_RESET + 864_000);
    mount_usage(&server, new).await;
    assert_eq!(
        service
            .refresh_account(account.id())
            .await
            .expect("recover 10-day slot")
            .quota()
            .access(),
        QuotaAccessState::Allowed
    );
}

#[tokio::test]
async fn windows_with_the_same_display_kind_or_duration_do_not_share_recovery() {
    for (second_period, name) in [
        (18_900, "acct_similar_periods"),
        (18_000, "acct_equal_periods"),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, name).await;
        let account = store.account(name).expect("account");
        let server = MockServer::start().await;
        let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
        let mut old = usage((100, SHORT_RESET), (100, SHORT_RESET + 100));
        old["rate_limit"]["allowed"] = json!(false);
        old["rate_limit"]["secondary_window"]["limit_window_seconds"] = json!(second_period);
        mount_usage(&server, old.clone()).await;
        let snapshot = service
            .refresh_account(account.id())
            .await
            .expect("seed independent exhausted slots");
        assert_ne!(
            snapshot.windows()[0].key(),
            snapshot.windows()[1].key(),
            "{name}"
        );
        old["rate_limit"]["secondary_window"]["used_percent"] = json!(0);
        old["rate_limit"]["secondary_window"]["reset_at"] =
            json!(SHORT_RESET + 100 + second_period);
        mount_usage(&server, old).await;
        assert!(
            service
                .refresh_account(account.id())
                .await
                .expect("recover secondary only")
                .quota()
                .is_exhausted(),
            "{name}"
        );
    }
}
