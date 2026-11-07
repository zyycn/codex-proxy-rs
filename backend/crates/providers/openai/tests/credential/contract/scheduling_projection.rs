//! 验证会话候选投影的容量口径、实时准入、隔离与有界失效

use std::sync::atomic::{AtomicBool, Ordering};

use futures::FutureExt;
use gateway_core::account::{AccountAffinity, AccountRuntimeSignals};
use gateway_core::provider_ports::ProviderCooldown;
use tokio::sync::Notify;

use super::*;

fn projection_attempt(
    client_key: &str,
    scope: Arc<FrozenAccountScope>,
    policy: AccountSelectionPolicy,
) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_projection").unwrap(),
            ClientApiKeyId::new(client_key).unwrap(),
        )
        .with_trace(gateway_core::diagnostics::TraceContext::new(
            "req_projection",
        )),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(30),
        policy,
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope),
        None,
        CancellationToken::new(),
    )
}

async fn select(
    selector: &CodexCredentialSelector,
    attempt: &AttemptContext,
    key: &ProviderSessionAffinityKey,
    model: &str,
) -> Result<provider_openai::credential::CodexCredentialLease, CredentialSelectionError> {
    selector
        .select(&SelectCodexCredential {
            upstream_model: model,
            request_url: &Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap(),
            attempt,
            session_affinity_key: Some(key),
        })
        .await
}

fn signal(in_flight: u32) -> AccountRuntimeSignals {
    AccountRuntimeSignals {
        in_flight,
        last_started_at: None,
        quota_reset_at: None,
        quota_remaining_rank: None,
        cooldown: None,
        failure_rate_basis_points: None,
        first_output_latency_ms: None,
    }
}

#[tokio::test]
async fn healthy_binding_reuses_complete_capacity_and_refreshes_selected_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "test-first");
    create_account(&store, "acct_second", "test-second");
    let leases = Arc::new(TestLeaseCoordinator::default());
    let first = ProviderAccountId::new("acct_first").unwrap();
    leases
        .signals
        .lock()
        .unwrap()
        .insert(ProviderAccountId::new("acct_second").unwrap(), signal(1));
    let selector = selector(&store, leases.clone());
    let attempt = attempt(BTreeSet::new());
    let key = ProviderSessionAffinityKey::try_new("projection-capacity").unwrap();
    drop(select(&selector, &attempt, &key, "gpt-5.4").await.unwrap());

    store.fail_provider_listing();
    leases
        .signals
        .lock()
        .unwrap()
        .insert(first.clone(), signal(1));
    let selected = select(&selector, &attempt, &key, "gpt-5.4").await.unwrap();
    let capacity = selected.capacity_snapshot().unwrap();
    assert_eq!(selected.account_id(), &first);
    assert_eq!((capacity.used_slots(), capacity.total_slots()), (3, 4));
    assert_eq!(store.provider_list_reads(), 1);
    assert_eq!(store.account_reads(), 1);
}

#[tokio::test]
async fn changed_owner_and_lease_contention_refresh_the_complete_pool() {
    for reason in ["disabled", "cooldown", "lease_busy"] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        let leases = Arc::new(TestLeaseCoordinator::default());
        let cooldowns = Arc::new(MemoryCooldownPort::new());
        let selector = selector_with_runtime(
            &store,
            leases.clone(),
            Arc::new(MemorySessionAffinity::default()),
            Arc::new(AccountFeedbackStats::default()),
            cooldowns.clone(),
        );
        let attempt = projection_attempt(
            "key_codex_contract",
            contract_account_scope(),
            account_policy().with_openai_account_affinity(AccountAffinity::Preferred),
        );
        let key = ProviderSessionAffinityKey::try_new("projection-refresh").unwrap();
        let selected = select(&selector, &attempt, &key, "gpt-5.4").await.unwrap();
        let first = selected.account_id().clone();
        drop(selected);
        match reason {
            "disabled" => store.set_enabled(&first, false).await.unwrap(),
            "cooldown" => {
                cooldowns
                    .put_if_later(ProviderCooldown::new(
                        first.clone(),
                        store.account(first.as_str()).unwrap().revision(),
                        SystemTime::now() + Duration::from_secs(60),
                    ))
                    .await
                    .unwrap();
            }
            "lease_busy" => {
                leases.busy_accounts.lock().unwrap().insert(first.clone());
            }
            _ => unreachable!(),
        }
        let selected = select(&selector, &attempt, &key, "gpt-5.4").await.unwrap();
        assert_ne!(selected.account_id(), &first, "{reason}");
        assert_eq!(store.provider_list_reads(), 2, "{reason}");
    }
}

#[tokio::test]
async fn projection_is_isolated_by_client_scope_model_and_account_policy() {
    for changed in ["client", "scope", "model", "concurrency"] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
        let key = ProviderSessionAffinityKey::try_new("projection-isolation").unwrap();
        let initial = attempt(BTreeSet::new());
        drop(select(&selector, &initial, &key, "gpt-5.4").await.unwrap());
        store.fail_provider_listing();
        let scope = if changed == "scope" {
            Arc::new(
                contract_account_scope()
                    .excluding_account(ProviderAccountId::new("acct_second").unwrap()),
            )
        } else {
            contract_account_scope()
        };
        let policy = if changed == "concurrency" {
            AccountSelectionPolicy::new(RotationStrategy::Smart, NonZeroU32::new(3).unwrap())
        } else {
            account_policy()
        };
        let attempt = projection_attempt(
            if changed == "client" {
                "key_another"
            } else {
                "key_codex_contract"
            },
            scope,
            policy,
        );
        let error = select(
            &selector,
            &attempt,
            &key,
            if changed == "model" {
                "gpt-5.5"
            } else {
                "gpt-5.4"
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, CredentialSelectionError::Store(_)),
            "{changed}: {error:?}"
        );
    }
}

#[tokio::test]
async fn expired_projection_requires_a_complete_read() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "test-first");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let attempt = attempt(BTreeSet::new());
    let key = ProviderSessionAffinityKey::try_new("projection-expiry").unwrap();
    drop(select(&selector, &attempt, &key, "gpt-5.4").await.unwrap());
    store.fail_provider_listing();
    tokio::time::sleep(Duration::from_millis(220)).await;
    assert!(matches!(
        select(&selector, &attempt, &key, "gpt-5.4").await,
        Err(CredentialSelectionError::Store(_)),
    ));
}

#[tokio::test]
async fn projection_entry_budget_evicts_older_models() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "test-first");
    let selector = selector(&store, Arc::new(TestLeaseCoordinator::default()));
    let attempt = attempt(BTreeSet::new());
    let key = ProviderSessionAffinityKey::try_new("projection-bound").unwrap();
    for index in 0..33 {
        drop(
            select(
                &selector,
                &attempt,
                &key,
                &format!("gpt-projection-{index}"),
            )
            .await
            .unwrap(),
        );
    }
    store.fail_provider_listing();
    drop(
        select(&selector, &attempt, &key, "gpt-projection-32")
            .await
            .unwrap(),
    );
    assert!(matches!(
        select(&selector, &attempt, &key, "gpt-projection-0").await,
        Err(CredentialSelectionError::Store(_)),
    ));
}

#[tokio::test]
async fn projection_fill_merges_equal_keys_and_releases_cancelled_owners() {
    for scenario in ["same_key", "different_key", "cancel_owner"] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        let affinity = Arc::new(MemorySessionAffinity::default());
        let key = ProviderSessionAffinityKey::try_new("projection-concurrent").unwrap();
        affinity
            .bind(
                &ProviderKind::new("openai").unwrap(),
                &key,
                &ProviderAccountId::new("acct_first").unwrap(),
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        let blocked = Arc::new(AtomicBool::new(true));
        let released = Arc::new(Notify::new());
        let hook_blocked = blocked.clone();
        let hook_released = released.clone();
        let cooldowns = Arc::new(MemoryCooldownPort::new().with_read_hook(Arc::new(move |_| {
            let blocked = hook_blocked.clone();
            let released = hook_released.clone();
            Box::pin(async move {
                if blocked.load(Ordering::SeqCst) {
                    released.notified().await;
                }
                Ok(())
            })
        })));
        let selector = selector_with_runtime(
            &store,
            Arc::new(TestLeaseCoordinator::default()),
            affinity,
            Arc::new(AccountFeedbackStats::default()),
            cooldowns,
        );
        let attempt = attempt(BTreeSet::new());
        let mut first = Box::pin(select(&selector, &attempt, &key, "gpt-5.4"));
        assert!(first.as_mut().now_or_never().is_none());
        let mut second = Box::pin(select(
            &selector,
            &attempt,
            &key,
            if scenario == "different_key" {
                "gpt-5.5"
            } else {
                "gpt-5.4"
            },
        ));
        assert!(second.as_mut().now_or_never().is_none());
        assert_eq!(
            store.provider_list_reads(),
            if scenario == "different_key" { 2 } else { 1 }
        );
        blocked.store(false, Ordering::SeqCst);
        released.notify_waiters();
        if scenario == "cancel_owner" {
            drop(first);
            drop(second.await.unwrap());
            assert_eq!(store.provider_list_reads(), 2);
        } else {
            let (first, second) = futures::join!(first, second);
            drop(first.unwrap());
            drop(second.unwrap());
        }
    }
}

#[tokio::test]
async fn strict_bound_waiters_recheck_only_their_account_until_capacity_opens() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "test-first");
    create_account(&store, "acct_second", "test-second");
    let leases = Arc::new(TestLeaseCoordinator::default());
    let selector = selector_with_runtime(
        &store,
        leases.clone(),
        Arc::new(MemorySessionAffinity::default()),
        Arc::new(AccountFeedbackStats::default()),
        Arc::new(MemoryCooldownPort::new()),
    );
    let key = ProviderSessionAffinityKey::try_new("queued-projection").unwrap();
    let queue_policy =
        account_policy().with_queue(gateway_core::concurrency::ConcurrencyQueuePolicy {
            max_waiting: 10,
            timeout: Duration::from_secs(3),
        });
    let first_attempt =
        projection_attempt("key_codex_contract", contract_account_scope(), queue_policy);
    let second_attempt =
        projection_attempt("key_codex_contract", contract_account_scope(), queue_policy);
    drop(
        select(&selector, &first_attempt, &key, "gpt-5.4")
            .await
            .unwrap(),
    );
    for id in ["acct_first", "acct_second"] {
        leases
            .signals
            .lock()
            .unwrap()
            .insert(ProviderAccountId::new(id).unwrap(), signal(100));
    }
    let mut first = Box::pin(select(&selector, &first_attempt, &key, "gpt-5.4"));
    assert!(first.as_mut().now_or_never().is_none());
    let mut second = Box::pin(select(&selector, &second_attempt, &key, "gpt-5.4"));
    assert!(second.as_mut().now_or_never().is_none());
    let initial_reads = store.provider_list_reads();
    let release = async {
        tokio::time::sleep(Duration::from_millis(650)).await;
        assert_eq!(store.provider_list_reads(), initial_reads);
        leases.signals.lock().unwrap().clear();
    };
    let (first, second, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        futures::join!(first, second, release)
    })
    .await
    .unwrap();
    for lease in [first.unwrap(), second.unwrap()] {
        assert_eq!(lease.capacity_snapshot().unwrap().total_slots(), 4);
    }
}

#[tokio::test]
async fn strict_queue_failure_reselects_once_and_only_rebinds_after_admission() {
    use gateway_core::concurrency::{ConcurrencyQueuePolicy, QueueRejection};
    for (full, spare, enabled) in [
        (false, true, true),
        (true, true, true),
        (false, false, true),
        (true, false, true),
        (false, true, false),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        let first = ProviderAccountId::new("acct_first").unwrap();
        let second = ProviderAccountId::new("acct_second").unwrap();
        let leases = Arc::new(TestLeaseCoordinator::default());
        let selector = selector_with_runtime(
            &store,
            leases.clone(),
            Arc::new(MemorySessionAffinity::default()),
            Arc::new(AccountFeedbackStats::default()),
            Arc::new(MemoryCooldownPort::new()),
        );
        let policy = account_policy()
            .with_queue(ConcurrencyQueuePolicy {
                max_waiting: 1,
                timeout: Duration::from_millis(150),
            })
            .with_switch_account_on_queue_failure(enabled);
        let context = || projection_attempt("key_codex_contract", contract_account_scope(), policy);
        let key = ProviderSessionAffinityKey::try_new("strict-queue-failure").unwrap();
        assert_eq!(
            select(&selector, &context(), &key, "gpt-5.4")
                .await
                .unwrap()
                .account_id(),
            &first
        );
        leases
            .signals
            .lock()
            .unwrap()
            .insert(first.clone(), signal(2));
        if !spare {
            leases
                .signals
                .lock()
                .unwrap()
                .insert(second.clone(), signal(2));
        }
        let head_ctx = context();
        let mut head = Box::pin(select(&selector, &head_ctx, &key, "gpt-5.4"));
        if full {
            assert!(head.as_mut().now_or_never().is_none());
        }
        let ctx = context();
        let mut pending = Box::pin(select(&selector, &ctx, &key, "gpt-5.4"));
        if !full {
            // B 即使空闲也不能在 A 的等待期内直接分流
            assert!(pending.as_mut().now_or_never().is_none());
        }
        let result = pending.await;
        let reason = if full {
            QueueRejection::Full
        } else {
            QueueRejection::Timeout
        };
        let snapshot = ctx.trace().snapshot().unwrap();
        let migrations = snapshot["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["stage"] == "account.binding.migrated")
            .map(|event| &event["data"])
            .collect::<Vec<_>>();
        if spare && enabled {
            assert_eq!(
                migrations,
                vec![&json!({
                    "reason": reason.to_string(),
                    "fromAccountId": first.as_str(),
                    "toAccountId": second.as_str(),
                })]
            );
        } else {
            assert!(migrations.is_empty());
        }
        let expected = if spare && enabled {
            assert_eq!(result.unwrap().account_id(), &second);
            &second
        } else {
            assert!(
                matches!(result, Err(CredentialSelectionError::QueueRejected(actual)) if actual == reason)
            );
            &first
        };
        drop(head);
        leases.signals.lock().unwrap().clear();
        assert_eq!(
            select(&selector, &context(), &key, "gpt-5.4")
                .await
                .unwrap()
                .account_id(),
            expected
        );
    }
}

#[tokio::test]
async fn account_queue_timeout_can_reselect_for_preferred_and_sessionless_requests() {
    for mode in [AccountAffinity::Preferred, AccountAffinity::Relaxed] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        let first = ProviderAccountId::new("acct_first").unwrap();
        let second = ProviderAccountId::new("acct_second").unwrap();
        let leases = Arc::new(TestLeaseCoordinator::default());
        let selector = selector(&store, leases.clone());
        let policy = account_policy()
            .with_openai_account_affinity(mode)
            .with_switch_account_on_queue_failure(true)
            .with_queue(gateway_core::concurrency::ConcurrencyQueuePolicy {
                max_waiting: 1,
                timeout: Duration::from_millis(30),
            });
        let ctx = projection_attempt("key_codex_contract", contract_account_scope(), policy);
        let key = ProviderSessionAffinityKey::try_new("queue-fallback").unwrap();
        let request = SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap(),
            attempt: &ctx,
            session_affinity_key: (mode == AccountAffinity::Preferred).then_some(&key),
        };
        assert_eq!(
            selector.select(&request).await.unwrap().account_id(),
            &first
        );
        for id in [&first, &second] {
            leases.signals.lock().unwrap().insert(id.clone(), signal(2));
        }
        let mut pending = Box::pin(selector.select(&request));
        assert!(pending.as_mut().now_or_never().is_none());
        // B 在排队后恢复，等待时限短于队首重查周期，只有最后一次立即选号能取到 B
        leases.signals.lock().unwrap().remove(&second);
        assert_eq!(pending.await.unwrap().account_id(), &second);
        if mode == AccountAffinity::Preferred {
            leases.signals.lock().unwrap().remove(&first);
            assert_eq!(
                selector.select(&request).await.unwrap().account_id(),
                &first
            );
        }
    }
}

#[tokio::test]
async fn movable_waiters_use_other_accounts_even_before_queue_promotion() {
    for (mode, has_session, bind_session) in [
        (AccountAffinity::Relaxed, false, false),
        (AccountAffinity::Strict, true, false),
        (AccountAffinity::Preferred, true, true),
    ] {
        for has_waiter_ahead in [false, true] {
            let store = Arc::new(MemoryAccountStore::default());
            create_account(&store, "acct_first", "test-first");
            create_account(&store, "acct_second", "test-second");
            let first = ProviderAccountId::new("acct_first").unwrap();
            let second = ProviderAccountId::new("acct_second").unwrap();
            let leases = Arc::new(TestLeaseCoordinator::default());
            let feedback = Arc::new(AccountFeedbackStats::default());
            for _ in 0..4 {
                feedback.report(
                    &ProviderKind::new("openai").unwrap(),
                    &second,
                    AccountAttemptFeedback::Failed {
                        first_output_ms: None,
                    },
                );
            }
            let selector = selector_with_runtime(
                &store,
                leases.clone(),
                Arc::new(MemorySessionAffinity::default()),
                feedback,
                Arc::new(MemoryCooldownPort::new()),
            );
            // 健康分保证请求排入 A，已有队首时也不因 B 的队列较短而提前排入 B
            let policy = account_policy()
                .with_queue(gateway_core::concurrency::ConcurrencyQueuePolicy {
                    max_waiting: 10,
                    timeout: Duration::from_secs(5),
                })
                .with_smart_scheduling(
                    gateway_core::account::SmartSchedulingConfig::new([
                        0.0, 0.0, 1.0, 0.0, 0.0, 0.2,
                    ])
                    .unwrap(),
                );
            let head_attempt =
                projection_attempt("key_codex_contract", contract_account_scope(), policy);
            let head_key = ProviderSessionAffinityKey::try_new("fixed-head").unwrap();
            assert_eq!(
                select(&selector, &head_attempt, &head_key, "gpt-5.4")
                    .await
                    .unwrap()
                    .account_id(),
                &first,
            );
            let attempt = projection_attempt(
                "key_codex_contract",
                contract_account_scope(),
                policy.with_openai_account_affinity(mode),
            );
            let key = ProviderSessionAffinityKey::try_new("movable-waiter").unwrap();
            if bind_session {
                assert_eq!(
                    select(&selector, &attempt, &key, "gpt-5.4")
                        .await
                        .unwrap()
                        .account_id(),
                    &first,
                );
            }
            for id in [&first, &second] {
                leases.signals.lock().unwrap().insert(id.clone(), signal(2));
            }
            let mut head = Box::pin(select(&selector, &head_attempt, &head_key, "gpt-5.4"));
            if has_waiter_ahead {
                assert!(head.as_mut().now_or_never().is_none());
            }
            let request = SelectCodexCredential {
                upstream_model: "gpt-5.4",
                request_url: &Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap(),
                attempt: &attempt,
                session_affinity_key: has_session.then_some(&key),
            };
            let mut pending = Box::pin(selector.select(&request));
            assert!(pending.as_mut().now_or_never().is_none());
            // A 及其队首始终占位，只释放 B，验证请求能在晋升或超时前使用新空位
            leases.signals.lock().unwrap().remove(&second);
            let selected = tokio::time::timeout(Duration::from_secs(2), pending)
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "missed free account: mode={mode:?}, has_waiter_ahead={has_waiter_ahead}"
                    )
                })
                .unwrap();
            assert_eq!(selected.account_id(), &second);
            let capacity = selected.capacity_snapshot().unwrap();
            assert_eq!((capacity.used_slots(), capacity.total_slots()), (3, 4));
            if has_waiter_ahead {
                assert!(head.as_mut().now_or_never().is_none());
            }
        }
    }
}
