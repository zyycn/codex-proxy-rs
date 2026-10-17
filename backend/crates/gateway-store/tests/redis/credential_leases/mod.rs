//! 验证账号调度租约的容量、续租、失效与释放

mod details;

use std::time::Duration;

use gateway_store::redis::{
    CredentialBoundedLeaseAcquisition, CredentialBoundedLeaseRequest, CredentialLeaseRepository,
    CredentialLeaseRequest, CredentialLeaseScope, RedisCredentialLeaseRepository,
};
use redis::aio::ConnectionManager;
use uuid::Uuid;

#[test]
fn credential_lease_rejects_zero_ttl() {
    let request = CredentialLeaseRequest {
        scope: CredentialLeaseScope::OAuthRefresh,
        resource_id: "account-1".to_owned(),
        owner_id: "worker-1".to_owned(),
        ttl: Duration::ZERO,
    };
    assert!(request.validate().is_err());
}

#[test]
fn only_account_scheduling_leases_allow_unlimited_concurrency() {
    let mut request = scheduling_request("acct_unlimited", "worker-unlimited", 0);
    request.validate().expect("unlimited account concurrency");
    for scope in [
        CredentialLeaseScope::OAuthRefresh,
        CredentialLeaseScope::OAuthRefreshCapacity,
    ] {
        request.scope = scope;
        assert!(request.validate().is_err());
    }
}

#[tokio::test]
async fn reserved_and_normal_leases_enforce_independent_atomic_limits_and_release() {
    let Some((repository, _, _)) = repository().await else {
        return;
    };
    let normal = scheduling_request("acct_independent", "normal", 2);
    let reserved = CredentialBoundedLeaseRequest {
        scope: CredentialLeaseScope::ProviderAccountReserved,
        max_concurrent: 3,
        ..normal.clone()
    };
    let attempts = futures::future::join_all((0..20).map(|index| {
        let request = if index % 2 == 0 {
            normal.clone()
        } else {
            reserved.clone()
        };
        let repository = &repository;
        async move {
            (
                request.scope,
                repository
                    .try_acquire_bounded_lease(&request)
                    .await
                    .unwrap(),
            )
        }
    }))
    .await;
    let mut normal_leases = Vec::new();
    let mut reserved_leases = Vec::new();
    for (scope, acquisition) in attempts {
        if let CredentialBoundedLeaseAcquisition::Acquired(lease) = acquisition {
            if scope == CredentialLeaseScope::ProviderAccount {
                normal_leases.push(lease);
            } else {
                reserved_leases.push(lease);
            }
        }
    }
    assert_eq!((normal_leases.len(), reserved_leases.len()), (2, 3));
    assert_eq!(
        repository
            .credential_runtime_signals(std::slice::from_ref(&normal.resource_id))
            .await
            .unwrap()[0]
            .in_flight,
        2
    );

    reserved_leases.pop().unwrap().release().await.unwrap();
    assert!(matches!(
        repository.try_acquire_bounded_lease(&normal).await.unwrap(),
        CredentialBoundedLeaseAcquisition::Busy { .. }
    ));
    reserved_leases.push(acquired(
        repository
            .try_acquire_bounded_lease(&reserved)
            .await
            .unwrap(),
    ));
    normal_leases.pop().unwrap().release().await.unwrap();
    assert!(matches!(
        repository
            .try_acquire_bounded_lease(&reserved)
            .await
            .unwrap(),
        CredentialBoundedLeaseAcquisition::Busy { .. }
    ));
    normal_leases.push(acquired(
        repository.try_acquire_bounded_lease(&normal).await.unwrap(),
    ));
    for lease in normal_leases.into_iter().chain(reserved_leases) {
        lease.release().await.unwrap();
    }
    assert_eq!(
        repository
            .credential_runtime_signals(std::slice::from_ref(&normal.resource_id))
            .await
            .unwrap()[0]
            .in_flight,
        0
    );
}

#[tokio::test]
async fn independent_capacity_pools_share_last_started_signal_without_blocking() {
    let Some((repository, _, _)) = repository().await else {
        return;
    };
    for first_reserved in [false, true] {
        let normal =
            scheduling_request(&format!("acct_shared_signal_{first_reserved}"), "worker", 2);
        let reserved = CredentialBoundedLeaseRequest {
            scope: CredentialLeaseScope::ProviderAccountReserved,
            ..normal.clone()
        };
        let (first, second) = if first_reserved {
            (&reserved, &normal)
        } else {
            (&normal, &reserved)
        };
        let first_lease = acquired(repository.try_acquire_bounded_lease(first).await.unwrap());
        let signals = repository
            .credential_runtime_signals(std::slice::from_ref(&normal.resource_id))
            .await
            .unwrap();
        assert!(signals[0].last_started_at.is_some());
        let second_lease = acquired(repository.try_acquire_bounded_lease(second).await.unwrap());
        first_lease.release().await.unwrap();
        second_lease.release().await.unwrap();
    }
}

#[tokio::test]
async fn scheduling_lease_ignores_future_last_started_signal() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let request = scheduling_request("acct_future_start", "worker", 2);
    let first = acquired(
        repository
            .try_acquire_bounded_lease(&request)
            .await
            .unwrap(),
    );
    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*:last-started"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .unwrap();
    assert_eq!(keys.len(), 1);
    // 模拟旧信号或时钟偏差，排序时间不能阻止尚有容量的账号取得租约
    redis::Script::new("local t = redis.call('TIME'); return redis.call('SET', KEYS[1], tonumber(t[1]) * 1000 + 60000, 'PX', 120000)")
        .key(&keys[0]).invoke_async::<String>(&mut connection).await.unwrap();
    let second = acquired(
        repository
            .try_acquire_bounded_lease(&request)
            .await
            .unwrap(),
    );
    assert!(matches!(
        repository
            .try_acquire_bounded_lease(&request)
            .await
            .unwrap(),
        CredentialBoundedLeaseAcquisition::Busy { .. }
    ));
    first.release().await.unwrap();
    second.release().await.unwrap();
}

#[tokio::test]
async fn maintained_scheduling_lease_survives_initial_ttl_and_releases_on_drop() {
    for scope in [
        CredentialLeaseScope::ProviderAccount,
        CredentialLeaseScope::ProviderAccountReserved,
    ] {
        let Some((repository, _, _)) = repository().await else {
            return;
        };
        let mut request = scheduling_request("acct_long_request", "worker-long", 1);
        request.scope = scope;
        request.ttl = Duration::from_secs(1);
        let lease = acquired(
            repository
                .try_acquire_bounded_lease(&request)
                .await
                .unwrap(),
        );
        let cancellation = gateway_core::lifecycle::CancellationToken::new();
        let lease = lease
            .maintain(Default::default(), cancellation.clone())
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert!(!cancellation.is_cancelled());
        assert!(matches!(
            repository
                .try_acquire_bounded_lease(&request)
                .await
                .unwrap(),
            CredentialBoundedLeaseAcquisition::Busy { .. }
        ));
        drop(lease);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let CredentialBoundedLeaseAcquisition::Acquired(replacement) = repository
                    .try_acquire_bounded_lease(&request)
                    .await
                    .unwrap()
                {
                    replacement.release().await.unwrap();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dropped request releases its slot");
    }
}

#[tokio::test]
async fn maintained_scheduling_lease_does_not_resurrect_a_lost_slot() {
    for scope in [
        CredentialLeaseScope::ProviderAccount,
        CredentialLeaseScope::ProviderAccountReserved,
    ] {
        let Some((repository, _, _)) = repository().await else {
            return;
        };
        let mut request = scheduling_request("acct_lost_request", "worker-lost", 1);
        request.scope = scope;
        let lease = acquired(
            repository
                .try_acquire_bounded_lease(&request)
                .await
                .unwrap(),
        );
        let grant = lease.grant().unwrap().clone();
        let raw = CredentialLeaseRequest {
            scope: request.scope,
            resource_id: request.resource_id.clone(),
            owner_id: request.owner_id.clone(),
            ttl: request.ttl,
        };
        repository
            .release_credential_lease(&raw, &grant)
            .await
            .unwrap();
        let cancellation = gateway_core::lifecycle::CancellationToken::new();
        let lease = lease
            .maintain(Default::default(), cancellation.clone())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), cancellation.cancelled())
            .await
            .expect("lost lease cancels the request");
        let replacement = acquired(
            repository
                .try_acquire_bounded_lease(&request)
                .await
                .unwrap(),
        );
        drop(lease);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(matches!(
            repository
                .try_acquire_bounded_lease(&request)
                .await
                .unwrap(),
            CredentialBoundedLeaseAcquisition::Busy { .. }
        ));
        replacement.release().await.unwrap();
    }
}

#[tokio::test]
async fn unlimited_scheduling_leases_still_count_and_release() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let request = scheduling_request("acct_unlimited", "worker-unlimited", 0);
    let mut leases = Vec::new();
    for _ in 0..5 {
        leases.push(acquired(
            repository
                .try_acquire_bounded_lease(&request)
                .await
                .expect("unlimited slot"),
        ));
    }
    assert_eq!(
        repository
            .credential_runtime_signals(std::slice::from_ref(&request.resource_id))
            .await
            .expect("signals")[0]
            .in_flight,
        5
    );
    let limited = CredentialBoundedLeaseRequest {
        max_concurrent: 3,
        ..request.clone()
    };
    assert!(matches!(
        repository
            .try_acquire_bounded_lease(&limited)
            .await
            .expect("positive limit still applies"),
        CredentialBoundedLeaseAcquisition::Busy { .. }
    ));
    for lease in leases {
        assert!(lease.release().await.expect("release unlimited slot"));
    }
    assert_eq!(
        repository
            .credential_runtime_signals(std::slice::from_ref(&request.resource_id))
            .await
            .expect("released signals")[0]
            .in_flight,
        0
    );
    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .expect("isolated keys");
    for key in &keys {
        assert!(
            redis::cmd("PTTL")
                .arg(key)
                .query_async::<i64>(&mut connection)
                .await
                .expect("TTL")
                > 0
        );
    }
    redis::cmd("DEL")
        .arg(keys)
        .query_async::<i64>(&mut connection)
        .await
        .expect("cleanup isolated keys");
}

#[tokio::test]
async fn scheduling_lease_counts_concurrency_and_drop_release() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let resource_id = "acct_runtime_signal";
    let owner_id = "worker-runtime-signal";
    let request = scheduling_request(resource_id, owner_id, 2);

    let first = acquired(
        repository
            .try_acquire_bounded_lease(&request)
            .await
            .expect("acquire first slot"),
    );
    let first_fence = first.grant().expect("live grant").fencing_token.get();
    let second = acquired(
        repository
            .try_acquire_bounded_lease(&request)
            .await
            .expect("acquire second slot"),
    );
    let signals = repository
        .credential_runtime_signals(&[resource_id.to_owned()])
        .await
        .expect("load counted runtime signal");
    assert_eq!(signals[0].in_flight, 2);
    assert!(signals[0].last_started_at.is_some());

    let third = repository
        .try_acquire_bounded_lease(&request)
        .await
        .expect("capacity decision");
    assert!(matches!(
        third,
        CredentialBoundedLeaseAcquisition::Busy {
            retry_after: Some(_)
        }
    ));

    assert!(first.release().await.expect("release first slot"));
    let replacement = acquired(
        repository
            .try_acquire_bounded_lease(&request)
            .await
            .expect("reuse released slot"),
    );
    assert!(
        replacement
            .grant()
            .expect("replacement grant")
            .fencing_token
            .get()
            > first_fence
    );

    drop(second);
    drop(replacement);
    for _ in 0..100 {
        let signals = repository
            .credential_runtime_signals(&[resource_id.to_owned()])
            .await
            .expect("poll Drop release");
        if signals[0].in_flight == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        repository
            .credential_runtime_signals(&[resource_id.to_owned()])
            .await
            .expect("load released signal")[0]
            .in_flight,
        0
    );

    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .expect("list isolated lease keys");
    assert!(!keys.is_empty());
    assert!(
        keys.iter()
            .all(|key| !key.contains(resource_id) && !key.contains(owner_id))
    );
    redis::cmd("DEL")
        .arg(keys)
        .query_async::<i64>(&mut connection)
        .await
        .expect("clean isolated lease keys");
}

#[tokio::test]
async fn scheduling_lease_ttl_and_cache_loss_rebuild_empty_runtime_signal() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let mut request = scheduling_request("acct_crash_ttl", "worker-crash", 1);
    request.ttl = Duration::from_millis(25);
    let crashed = acquired(
        repository
            .try_acquire_bounded_lease(&request)
            .await
            .expect("acquire crash lease"),
    );
    std::mem::forget(crashed);
    std::thread::sleep(Duration::from_millis(50));

    let recovered = repository
        .credential_runtime_signals(&[request.resource_id.clone()])
        .await
        .expect("clean expired lease");
    assert_eq!(recovered[0].in_flight, 0);
    acquired(
        repository
            .try_acquire_bounded_lease(&request)
            .await
            .expect("reacquire after TTL"),
    )
    .release()
    .await
    .expect("release recovered lease");

    let cached = acquired(
        repository
            .try_acquire_bounded_lease(&request)
            .await
            .expect("acquire before cache loss"),
    );
    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .expect("list isolated cache keys");
    redis::cmd("DEL")
        .arg(keys)
        .query_async::<i64>(&mut connection)
        .await
        .expect("simulate Redis cache loss");
    std::mem::forget(cached);

    let rebuilt = repository
        .credential_runtime_signals(&[request.resource_id.clone()])
        .await
        .expect("rebuild empty runtime signal");
    assert_eq!(rebuilt[0].in_flight, 0);
    assert_eq!(rebuilt[0].last_started_at, None);
    acquired(
        repository
            .try_acquire_bounded_lease(&request)
            .await
            .expect("acquire after cache rebuild"),
    )
    .release()
    .await
    .expect("release rebuilt lease");
}

#[tokio::test]
async fn scheduling_cursor_is_shared_across_processes_and_isolated_by_client_and_provider() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let peer = RedisCredentialLeaseRepository::new(connection.clone(), &namespace)
        .expect("peer repository");

    let primary = gateway_core::policy::ClientApiKeyId::new("key_primary").expect("client key");
    let secondary = gateway_core::policy::ClientApiKeyId::new("key_secondary").expect("client key");
    let openai = gateway_core::routing::ProviderKind::new("openai").expect("provider");
    let xai = gateway_core::routing::ProviderKind::new("xai").expect("provider");

    assert_eq!(
        repository
            .advance_scheduling_cursor(&primary, &openai)
            .await
            .expect("first cursor"),
        0
    );
    assert_eq!(
        peer.advance_scheduling_cursor(&primary, &openai)
            .await
            .expect("shared cursor"),
        1
    );
    assert_eq!(
        peer.advance_scheduling_cursor(&secondary, &openai)
            .await
            .expect("client-isolated cursor"),
        0
    );
    assert_eq!(
        peer.advance_scheduling_cursor(&primary, &xai)
            .await
            .expect("provider-isolated cursor"),
        0
    );

    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .expect("list cursor keys");
    assert_eq!(keys.len(), 3);
    assert!(keys.iter().all(|key| {
        !key.contains("key_primary")
            && !key.contains("key_secondary")
            && !key.contains("openai")
            && !key.contains("xai")
    }));
    redis::cmd("DEL")
        .arg(keys)
        .query_async::<i64>(&mut connection)
        .await
        .expect("clean cursor keys");
}

#[tokio::test]
async fn generic_refresh_guard_is_exclusive_and_released_on_drop() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let request = CredentialLeaseRequest {
        scope: CredentialLeaseScope::OAuthRefresh,
        resource_id: "acct_refresh_guard".to_owned(),
        owner_id: "worker-refresh-1".to_owned(),
        ttl: Duration::from_secs(60),
    };
    let guard = repository
        .try_acquire_guard(request.clone())
        .await
        .expect("acquire refresh guard")
        .expect("first refresh lease is available");
    let competing = CredentialLeaseRequest {
        owner_id: "worker-refresh-2".to_owned(),
        ..request.clone()
    };
    assert!(
        repository
            .try_acquire_guard(competing.clone())
            .await
            .expect("attempt competing refresh guard")
            .is_none()
    );

    drop(guard);
    let mut replacement = None;
    for _ in 0..100 {
        replacement = repository
            .try_acquire_guard(competing.clone())
            .await
            .expect("poll released refresh guard");
        if replacement.is_some() {
            break;
        }
        tokio::task::yield_now().await;
    }
    replacement
        .expect("refresh guard released by Drop")
        .release()
        .await
        .expect("release replacement refresh guard");

    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .expect("list isolated refresh keys");
    if !keys.is_empty() {
        redis::cmd("DEL")
            .arg(keys)
            .query_async::<i64>(&mut connection)
            .await
            .expect("clean isolated refresh keys");
    }
}

#[tokio::test]
async fn refresh_capacity_is_shared_across_provider_callers() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let request = |owner_id: &str| CredentialBoundedLeaseRequest {
        scope: CredentialLeaseScope::OAuthRefreshCapacity,
        resource_id: "oauth-refresh-global".to_owned(),
        owner_id: owner_id.to_owned(),
        max_concurrent: 2,
        ttl: Duration::from_secs(60),
    };

    let openai = acquired(
        repository
            .try_acquire_bounded_lease(&request("openai-worker"))
            .await
            .expect("acquire OpenAI capacity"),
    );
    let xai = acquired(
        repository
            .try_acquire_bounded_lease(&request("xai-worker"))
            .await
            .expect("acquire xAI capacity"),
    );
    assert!(matches!(
        repository
            .try_acquire_bounded_lease(&request("second-openai-worker"))
            .await
            .expect("read shared capacity"),
        CredentialBoundedLeaseAcquisition::Busy { .. }
    ));

    openai.release().await.expect("release OpenAI capacity");
    acquired(
        repository
            .try_acquire_bounded_lease(&request("replacement-xai-worker"))
            .await
            .expect("reuse shared capacity"),
    )
    .release()
    .await
    .expect("release replacement capacity");
    xai.release().await.expect("release xAI capacity");

    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .expect("list isolated capacity keys");
    if !keys.is_empty() {
        redis::cmd("DEL")
            .arg(keys)
            .query_async::<i64>(&mut connection)
            .await
            .expect("clean isolated capacity keys");
    }
}

fn scheduling_request(
    resource_id: &str,
    owner_id: &str,
    max_concurrent: u32,
) -> CredentialBoundedLeaseRequest {
    CredentialBoundedLeaseRequest {
        scope: CredentialLeaseScope::ProviderAccount,
        resource_id: resource_id.to_owned(),
        owner_id: owner_id.to_owned(),
        max_concurrent,
        ttl: Duration::from_secs(60),
    }
}

fn acquired(
    acquisition: CredentialBoundedLeaseAcquisition,
) -> gateway_store::redis::CredentialLeaseGuard {
    match acquisition {
        CredentialBoundedLeaseAcquisition::Acquired(guard) => guard,
        CredentialBoundedLeaseAcquisition::Busy { retry_after } => {
            panic!("expected acquired lease, retry after {retry_after:?}")
        }
    }
}

async fn repository() -> Option<(RedisCredentialLeaseRepository, ConnectionManager, String)> {
    let redis_url = crate::support::test_env("CPR_TEST_REDIS_URL")?;
    let client = redis::Client::open(redis_url).expect("valid CPR_TEST_REDIS_URL");
    let connection = client
        .get_connection_manager()
        .await
        .expect("connect test Redis");
    let namespace = format!("gateway-store-lease-test-{}", Uuid::new_v4());
    let repository = RedisCredentialLeaseRepository::new(connection.clone(), &namespace)
        .expect("valid test namespace");
    Some((repository, connection, namespace))
}

#[tokio::test]
async fn runtime_signals_preserve_order_and_counts_across_batches() {
    let Some((repository, _, _)) = repository().await else {
        return;
    };
    let ids = (0..1000)
        .map(|index| format!("acct_batch_{index}"))
        .collect::<Vec<_>>();
    let mut guards = Vec::new();
    for index in [0, 127, 128, 500, 999] {
        let request = scheduling_request(&ids[index], "batch-test", 2);
        guards.push(acquired(
            repository
                .try_acquire_bounded_lease(&request)
                .await
                .unwrap(),
        ));
    }
    let started = std::time::Instant::now();
    let signals = repository.credential_runtime_signals(&ids).await.unwrap();
    assert_eq!(signals.len(), ids.len());
    for (index, (signal, id)) in signals.iter().zip(&ids).enumerate() {
        assert_eq!(&signal.resource_id, id);
        let acquired = [0, 127, 128, 500, 999].contains(&index);
        assert_eq!(signal.in_flight, u32::from(acquired));
        assert_eq!(signal.last_started_at.is_some(), acquired);
    }
    eprintln!(
        "1000 Redis runtime signals: elapsed={:?}",
        started.elapsed()
    );
    for guard in guards {
        guard.release().await.unwrap();
    }
}
