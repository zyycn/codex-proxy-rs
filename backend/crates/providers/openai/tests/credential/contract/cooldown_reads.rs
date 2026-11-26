//! 验证选号冷却读取的并发边界、取消与冻结和读取失败语义

use std::sync::atomic::{AtomicUsize, Ordering};

use gateway_core::provider_ports::{
    ProviderCooldown, ProviderCooldownKind, ProviderCooldownScope, ProviderScopedCooldown,
    ProviderStoreError, ProviderStoreErrorKind,
};
use gateway_core::routing::UpstreamModelId;
use tokio::sync::Notify;

use super::*;

#[derive(Default)]
struct ReadActivity {
    started: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    changed: Notify,
}

struct ActiveRead(Arc<ReadActivity>);

impl ActiveRead {
    fn begin(activity: Arc<ReadActivity>) -> Self {
        activity.started.fetch_add(1, Ordering::SeqCst);
        let active = activity.active.fetch_add(1, Ordering::SeqCst) + 1;
        activity.peak.fetch_max(active, Ordering::SeqCst);
        activity.changed.notify_one();
        Self(activity)
    }
}

impl Drop for ActiveRead {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn pool_attempt(store: &Arc<MemoryAccountStore>, count: usize) -> AttemptContext {
    let provider = ProviderKind::new("openai").unwrap();
    let accounts = (0..count)
        .map(|index| {
            let id = format!("acct_{index:04}");
            create_account(store, &id, "synthetic-access-token");
            (
                ProviderAccountId::new(id).unwrap(),
                RuntimeAccount::new(provider.clone(), BTreeSet::new()),
            )
        })
        .collect();
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_cooldown_reads").unwrap(),
            ClientApiKeyId::new("key_cooldown_reads").unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(Arc::new(
            FrozenAccountScope::new(
                Arc::new(RuntimeAccountDirectory::new(accounts)),
                ClientRoutingScope::all_accounts(),
            ),
        )),
        None,
        CancellationToken::new(),
    )
}

#[tokio::test(start_paused = true)]
async fn selector_overlaps_cooldown_reads_with_bounded_in_flight_work() {
    for count in [10_usize, 100, 1000, 3000] {
        let store = Arc::new(MemoryAccountStore::default());
        let attempt = pool_attempt(&store, count);
        let activity = Arc::new(ReadActivity::default());
        let observed = activity.clone();
        let read_hook = Arc::new(
            move |_| -> futures::future::BoxFuture<'static, Result<(), ProviderStoreError>> {
                let activity = observed.clone();
                Box::pin(async move {
                    let _read = ActiveRead::begin(activity);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Ok(())
                })
            },
        );
        let cooldowns = Arc::new(
            MemoryCooldownPort::new()
                .with_read_hook(read_hook.clone())
                .with_scoped_read_hook(read_hook),
        );
        let selector = selector_with_runtime(
            &store,
            Arc::new(TestLeaseCoordinator::default()),
            Arc::new(MemorySessionAffinity::default()),
            Arc::new(AccountFeedbackStats::default()),
            cooldowns,
        );
        let request_url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
        let started = tokio::time::Instant::now();
        let selected = selector
            .select(&SelectCodexCredential {
                protocol: gateway_protocol::openai::OPENAI_PROTOCOL,
                upstream_model: "gpt-5.4",
                request_url: &request_url,
                attempt: &attempt,
                session_affinity_key: None,
            })
            .await
            .expect("available account");
        let elapsed = started.elapsed();
        assert!(selected.account_id().as_str().starts_with("acct_"));
        assert_eq!(activity.started.load(Ordering::SeqCst), count * 2);
        assert_eq!(activity.active.load(Ordering::SeqCst), 0);
        assert_eq!(activity.peak.load(Ordering::SeqCst), count.min(128) * 2);
        // 使用虚拟时间证明等待被合并，不把 CI 的 CPU 调度抖动当成性能回归
        assert!(elapsed <= Duration::from_millis(20 * count.div_ceil(128) as u64));
        println!(
            "candidates={count} simulated_rtt_ms=10 elapsed={elapsed:?} peak_reads={}",
            activity.peak.load(Ordering::SeqCst)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn dropping_selection_cancels_pending_reads_without_starting_later_batches() {
    let store = Arc::new(MemoryAccountStore::default());
    let attempt = pool_attempt(&store, 1000);
    let activity = Arc::new(ReadActivity::default());
    let observed = activity.clone();
    let cooldowns = Arc::new(MemoryCooldownPort::new().with_read_hook(Arc::new(move |_| {
        let activity = observed.clone();
        Box::pin(async move {
            let _read = ActiveRead::begin(activity);
            std::future::pending().await
        })
    })));
    let leases = Arc::new(TestLeaseCoordinator::default());
    let selector = selector_with_runtime(
        &store,
        leases.clone(),
        Arc::new(MemorySessionAffinity::default()),
        Arc::new(AccountFeedbackStats::default()),
        cooldowns,
    );
    let request_url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
    let request = SelectCodexCredential {
        protocol: gateway_protocol::openai::OPENAI_PROTOCOL,
        upstream_model: "gpt-5.4",
        request_url: &request_url,
        attempt: &attempt,
        session_affinity_key: None,
    };
    let mut selection = Box::pin(selector.select(&request));
    let pending_batch = async {
        while activity.started.load(Ordering::SeqCst) < 128 {
            activity.changed.notified().await;
        }
    };
    tokio::select! {
        result = &mut selection => panic!("reads must remain pending: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(1), pending_batch) => {
            result.expect("first batch started");
        }
    }
    drop(selection);
    assert_eq!(activity.started.load(Ordering::SeqCst), 128);
    assert_eq!(activity.active.load(Ordering::SeqCst), 0);
    assert!(leases.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn concurrent_reads_preserve_cooldown_expiry_probe_freeze_and_error_fallback() {
    let now = SystemTime::now();
    for (kind, until, fail_read, expected) in [
        (
            ProviderCooldownKind::RateLimit,
            now + Duration::from_secs(60),
            false,
            "acct_second",
        ),
        (
            ProviderCooldownKind::RateLimit,
            now - Duration::from_secs(60),
            false,
            "acct_first",
        ),
        (
            ProviderCooldownKind::CapacityFreeze,
            now + Duration::from_secs(60),
            false,
            "acct_second",
        ),
        (
            ProviderCooldownKind::CapacityFreeze,
            now - Duration::from_secs(60),
            false,
            "acct_first",
        ),
        (
            ProviderCooldownKind::CapacityFreezeProbe,
            now - Duration::from_secs(60),
            false,
            "acct_second",
        ),
        (
            ProviderCooldownKind::RateLimit,
            now + Duration::from_secs(60),
            true,
            "acct_first",
        ),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        store.set_scheduling("acct_first", None, AccountWeight::new(100).unwrap());
        store.set_scheduling("acct_second", None, AccountWeight::new(1).unwrap());
        let account = store.account("acct_first").unwrap();
        let cooldown =
            ProviderCooldown::new_with_kind(account.id().clone(), account.revision(), until, kind);
        let cooldowns = Arc::new(
            MemoryCooldownPort::new().with_read_hook(Arc::new(move |id| {
                Box::pin(async move {
                    tokio::task::yield_now().await;
                    if fail_read && id.as_str() == "acct_first" {
                        Err(ProviderStoreError::new(
                            ProviderStoreErrorKind::Unavailable,
                            "test cooldown read",
                        ))
                    } else {
                        Ok(())
                    }
                })
            })),
        );
        cooldowns.put_if_later(cooldown.clone()).await.unwrap();
        let selector = selector_with_runtime(
            &store,
            Arc::new(TestLeaseCoordinator::default()),
            Arc::new(MemorySessionAffinity::default()),
            Arc::new(AccountFeedbackStats::default()),
            cooldowns.clone(),
        );
        let request_url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
        let attempt = attempt(BTreeSet::new());
        let selected = selector
            .select(&SelectCodexCredential {
                protocol: gateway_protocol::openai::OPENAI_PROTOCOL,
                upstream_model: "gpt-5.4",
                request_url: &request_url,
                attempt: &attempt,
                session_affinity_key: None,
            })
            .await
            .unwrap();
        assert_eq!(
            selected.account_id().as_str(),
            expected,
            "{kind:?} failure={fail_read}"
        );
        assert_eq!(
            cooldowns.cooldowns.lock().unwrap().get(account.id()),
            Some(&cooldown)
        );
    }
}

#[tokio::test]
async fn shorter_account_cooldown_cannot_unmask_an_active_model_cooldown_during_selection() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "test-first");
    create_account(&store, "acct_second", "test-second");
    store.set_scheduling("acct_first", None, AccountWeight::new(100).unwrap());
    let account = store.account("acct_first").unwrap();
    let cooldowns = MemoryCooldownPort::new();
    let account_cooldowns = cooldowns.cooldowns.clone();
    let revision = account.revision();
    let cooldowns = Arc::new(cooldowns.with_read_hook(Arc::new(move |id| {
        let account_cooldowns = account_cooldowns.clone();
        Box::pin(async move {
            if id.as_str() == "acct_first" {
                account_cooldowns.lock().unwrap().insert(
                    id.clone(),
                    ProviderCooldown::new(
                        id,
                        revision,
                        SystemTime::now() + Duration::from_millis(100),
                    ),
                );
            } else {
                // 后续候选的读取跨过账号冷却期限，模型冷却仍必须阻止高权重账号
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Ok(())
        })
    })));
    cooldowns
        .put_scoped_if_later(ProviderScopedCooldown::new(
            account.id().clone(),
            revision,
            ProviderCooldownScope::upstream_model(UpstreamModelId::new("gpt-5.4").unwrap()),
            SystemTime::now() + Duration::from_secs(60),
        ))
        .await
        .unwrap();
    let selector = selector_with_runtime(
        &store,
        Arc::new(TestLeaseCoordinator::default()),
        Arc::new(MemorySessionAffinity::default()),
        Arc::new(AccountFeedbackStats::default()),
        cooldowns.clone(),
    );
    let request_url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
    let attempt = attempt(BTreeSet::new());
    let selected = selector
        .select(&SelectCodexCredential {
            protocol: gateway_protocol::openai::OPENAI_PROTOCOL,
            upstream_model: "gpt-5.4",
            request_url: &request_url,
            attempt: &attempt,
            session_affinity_key: None,
        })
        .await
        .unwrap();
    assert!(cooldowns.cooldowns.lock().unwrap()[account.id()].until() < SystemTime::now());
    assert_eq!(selected.account_id().as_str(), "acct_second");
}
