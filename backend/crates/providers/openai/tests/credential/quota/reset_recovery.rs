//! 到期后的明确放行恢复，保留到期前、多窗口与再次拒绝的保护

use gateway_core::account::{OpaqueProviderData, QuotaObservation};

use super::*;

fn usage(now: i64, resets: &[Option<i64>], allowed: Option<bool>) -> serde_json::Value {
    let mut value = json!({"rate_limit": {}});
    if let Some(allowed) = allowed {
        value["rate_limit"]["allowed"] = json!(allowed);
    }
    for ((role, seconds), reset) in [("primary_window", 18_000), ("secondary_window", 604_800)]
        .into_iter()
        .zip(resets)
    {
        let mut window = json!({"used_percent": 100, "limit_window_seconds": seconds});
        if let Some(offset) = reset {
            window["reset_at"] = json!(now + offset);
        }
        value["rate_limit"][role] = window;
    }
    value
}

async fn seed_exhaustion(
    store: &MemoryAccountStore,
    id: &str,
    usage: serde_json::Value,
    observed_at: SystemTime,
    reset_at: Option<SystemTime>,
) -> ProviderAccount {
    let account = store.account(id).expect("account");
    store
        .compare_and_swap_quota(QuotaObservation {
            quota_pause: None,
            expected_connection: None,
            plan_type: None,
            account_id: account.id().clone(),
            expected_revision: account.revision(),
            quota: OpaqueProviderData::new(usage.as_object().unwrap().clone()),
            observed_at,
            state: QuotaState::exhausted(QuotaEvidence::UsageLimitReached, observed_at, reset_at),
        })
        .await
        .expect("persist confirmed exhaustion");
    account
}

#[tokio::test]
async fn allowed_refresh_recovers_full_windows_only_after_the_confirmed_resets() {
    for (name, resets, failure_offset, allowed, recovered) in [
        ("before_reset", vec![Some(3600)], -120, Some(true), false),
        ("after_reset", vec![Some(-60)], -120, Some(true), true),
        ("still_denied", vec![Some(-60)], -120, Some(false), false),
        ("unknown_access", vec![Some(-60)], -120, None, false),
        ("unknown_reset", vec![None], -120, Some(true), false),
        ("new_failure", vec![Some(-60)], -30, Some(true), false),
        (
            "weekly_not_due",
            vec![Some(-60), Some(3600)],
            -120,
            Some(true),
            false,
        ),
        (
            "both_due",
            vec![Some(-90), Some(-60)],
            -120,
            Some(true),
            true,
        ),
        (
            "weekly_unknown",
            vec![Some(-60), None],
            -120,
            Some(true),
            false,
        ),
    ] {
        for worker in [false, true] {
            let store = Arc::new(MemoryAccountStore::default());
            let account_id = format!("acct_reset_{name}_{worker}");
            create_account(&store, &account_id).await;
            let now = Utc::now().timestamp();
            let observed_at =
                SystemTime::UNIX_EPOCH + Duration::from_secs((now + failure_offset) as u64);
            let reset_at = resets
                .iter()
                .filter_map(|reset| *reset)
                .min()
                .map(|offset| SystemTime::UNIX_EPOCH + Duration::from_secs((now + offset) as u64));
            let old_usage = usage(now, &resets, Some(false));
            let account =
                seed_exhaustion(&store, &account_id, old_usage, observed_at, reset_at).await;
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/codex/usage"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(usage(now, &resets, allowed)),
                )
                .mount(&server)
                .await;
            let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
            if worker {
                let summary = service.synchronize().await.expect("worker refresh");
                assert_eq!(summary.updated, u64::from(recovered), "{name}");
            } else {
                service
                    .refresh_account(account.id())
                    .await
                    .expect("manual refresh");
            }
            let refreshed = service
                .read_account(account.id())
                .await
                .expect("read")
                .expect("snapshot");
            assert_eq!(
                refreshed.quota().is_exhausted(),
                !recovered,
                "{name}, worker={worker}"
            );
            assert!(
                refreshed
                    .windows()
                    .iter()
                    .all(|window| window.used_percent() == Some(100.0))
            );
            assert_eq!(
                store
                    .account(&account_id)
                    .expect("account")
                    .quota()
                    .is_exhausted(),
                !recovered
            );
            assert_eq!(
                store
                    .quota_json(&account_id)
                    .expect("quota")
                    .get("_quota_recovery")
                    .is_none(),
                recovered
            );

            if recovered {
                service
                    .refresh_account(account.id())
                    .await
                    .expect("allowed remains allowed");
                assert!(
                    !store
                        .account(&account_id)
                        .expect("account")
                        .quota()
                        .is_exhausted()
                );
                // 新拒绝不能沿用刚刚解除时的旧 reset，即使查询仍然 allowed=true
                persist_quota_state(&store, &account, exhausted_quota(reset_at)).await;
                for _ in 0..2 {
                    let snapshot = service
                        .refresh_account(account.id())
                        .await
                        .expect("refresh after new refusal");
                    assert!(snapshot.quota().is_exhausted(), "{name}");
                }
            }
        }
    }
}

#[tokio::test]
async fn unmatched_usage_limit_reset_requires_a_complete_allowed_observation() {
    let now = Utc::now().timestamp();
    let at = |seconds: i64| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds as u64);
    let week = Some(86_400);
    let weekly = |used: i64| {
        json!({
            "limit_window_seconds": 604_800,
            "reset_at": now + 86_400,
            "used_percent": used
        })
    };
    let missing_percent = json!({"limit_window_seconds": 604_800, "reset_at": now + 86_400});
    for (case, error_offset, allowed, weekly_window, recovers) in [
        ("due", -60, Some(true), weekly(45), true),
        ("hint_conflict_due", -60, Some(true), weekly(45), true),
        ("hint_conflict_future", 3600, Some(true), weekly(45), false),
        ("before_reset", 3600, Some(true), weekly(45), false),
        ("week_full", -60, Some(true), weekly(100), false),
        ("week_missing", -60, Some(true), json!(null), false),
        ("usage_missing", -60, Some(true), missing_percent, false),
        ("denied", -60, Some(false), weekly(45), false),
        ("unknown", -60, None, weekly(45), false),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        let id = format!("acct_unmatched_{case}");
        create_account(&store, &id).await;
        let error_reset = at(now + error_offset);
        let mut old = usage(now, &[Some(error_offset - 2), week], Some(true));
        old["rate_limit"]["primary_window"]["used_percent"] = json!(99);
        old["rate_limit"]["secondary_window"]["used_percent"] = json!(45);
        let observed_at = at(now - 120);
        let account = seed_exhaustion(&store, &id, old, observed_at, Some(error_reset)).await;
        if case.starts_with("hint_conflict") {
            persist_quota_state(
                &store,
                &account,
                QuotaState::exhausted(
                    QuotaEvidence::UsageLimitReached,
                    observed_at,
                    Some(error_reset),
                )
                .with_limit_window_minutes(Some(300)),
            )
            .await;
        }
        let mut fresh = usage(now, &[Some(18_000), week], allowed);
        fresh["rate_limit"]["primary_window"]["used_percent"] = json!(0);
        fresh["rate_limit"]["secondary_window"] = weekly_window;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fresh))
            .mount(&server)
            .await;
        let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
        let summary = service.synchronize().await.unwrap();
        assert_eq!(summary.updated, u64::from(recovers), "{case}");
        let quota = store.account(&id).unwrap().quota();
        assert_eq!(quota.is_exhausted(), !recovers, "{case}");
        if !recovers {
            assert_eq!(quota.reset_at(), Some(error_reset), "{case}");
            // 第二次手动复核从已落库的进度继续，缺失或拒绝的观察仍不可恢复
            let refreshed = service.refresh_account(account.id()).await.unwrap();
            assert!(refreshed.quota().is_exhausted(), "{case}");
            if case == "week_missing" {
                server.reset().await;
                let mut complete = usage(now, &[Some(18_000), week], Some(true));
                complete["rate_limit"]["primary_window"]["used_percent"] = json!(0);
                complete["rate_limit"]["secondary_window"] = weekly(45);
                Mock::given(method("GET"))
                    .and(path("/api/codex/usage"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(complete))
                    .mount(&server)
                    .await;
                let recovered = service.refresh_account(account.id()).await.unwrap();
                assert!(!recovered.quota().is_exhausted());
            }
        }
    }
}

#[tokio::test]
async fn duration_hint_keeps_each_confirmed_window_until_it_is_observed_recovered() {
    let store = Arc::new(MemoryAccountStore::default());
    let id = "acct_partial_hinted_reset";
    create_account(&store, id).await;
    let now = Utc::now().timestamp();
    let denied_at = SystemTime::now() - Duration::from_secs(120);
    let baseline = usage(now, &[Some(-60), Some(-60)], Some(false));
    let account = seed_exhaustion(&store, id, baseline, denied_at, None).await;
    persist_quota_state(
        &store,
        &account,
        QuotaState::exhausted(QuotaEvidence::UsageLimitReached, denied_at, None)
            .with_limit_window_minutes(Some(10080)),
    )
    .await;
    let server = MockServer::start().await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    for (allowed, role, seconds, recovered) in [
        (false, "secondary_window", 604800, false),
        (true, "primary_window", 18000, false),
        (true, "secondary_window", 604800, true),
    ] {
        server.reset().await;
        let fresh = json!({"rate_limit": {"allowed": allowed, (role): {
            "limit_window_seconds": seconds, "used_percent": 0, "reset_at": now + seconds
        }}});
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fresh))
            .mount(&server)
            .await;
        let snapshot = service.refresh_account(account.id()).await.unwrap();
        assert_eq!(
            snapshot.quota().is_exhausted(),
            !recovered,
            "{role} allowed={allowed}"
        );
    }
}

#[tokio::test]
async fn newer_denial_survives_an_unmatched_reset_refresh() {
    let now = SystemTime::now();
    let stamp = Utc::now().timestamp();
    let mut healthy = usage(stamp, &[Some(18_000), Some(86_400)], Some(true));
    healthy["rate_limit"]["primary_window"]["used_percent"] = json!(0);
    healthy["rate_limit"]["secondary_window"]["used_percent"] = json!(45);
    let reset = now - Duration::from_secs(60);
    let store = Arc::new(MemoryAccountStore::default());
    let id = "acct_concurrent_quota";
    create_account(&store, id).await;
    let account = seed_exhaustion(
        &store,
        id,
        healthy.clone(),
        now - Duration::from_secs(120),
        Some(reset),
    )
    .await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(healthy))
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(&store, reqwest::Client::new(), server.uri());
    let (read, resume) = store.hold_next_quota_read();
    let next_reset = now + Duration::from_secs(3600);
    // 刷新已收到放行回复并读出旧快照，在合并写入前记录新的推理拒绝
    let (result, ()) = tokio::join!(service.refresh_account(account.id()), async {
        read.await.unwrap();
        persist_quota_state(&store, &account, exhausted_quota(Some(next_reset))).await;
        resume.send(()).unwrap();
    });
    result.expect("refresh quota display");
    let state = store.account(id).unwrap().quota();
    assert!(state.is_exhausted());
    assert_eq!(state.reset_at(), Some(next_reset));
}
