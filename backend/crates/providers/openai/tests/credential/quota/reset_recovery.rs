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
            let account = store.account(&account_id).expect("account");
            let now = Utc::now().timestamp();
            let observed_at =
                SystemTime::UNIX_EPOCH + Duration::from_secs((now + failure_offset) as u64);
            let reset_at = resets
                .iter()
                .filter_map(|reset| *reset)
                .min()
                .map(|offset| SystemTime::UNIX_EPOCH + Duration::from_secs((now + offset) as u64));
            let old_usage = usage(now, &resets, Some(false));
            store
                .compare_and_swap_quota(QuotaObservation {
                    plan_type: None,
                    account_id: account.id().clone(),
                    expected_revision: account.revision(),
                    quota: OpaqueProviderData::new(old_usage.as_object().expect("usage").clone()),
                    observed_at,
                    state: QuotaState::exhausted(
                        QuotaEvidence::UsageLimitReached,
                        observed_at,
                        reset_at,
                    ),
                })
                .await
                .expect("persist confirmed exhaustion before refresh");
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
