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
fn scheduling_lease_rejects_zero_concurrency() {
    let request = scheduling_request("acct_invalid", "worker-invalid", 0, Duration::ZERO);
    assert!(request.validate().is_err());
}

#[tokio::test]
async fn scheduling_lease_counts_concurrency_interval_and_drop_release() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let resource_id = "acct_runtime_signal";
    let owner_id = "worker-runtime-signal";
    let request = scheduling_request(resource_id, owner_id, 2, Duration::ZERO);

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

    let interval_request = scheduling_request(
        "acct_interval",
        "worker-interval",
        2,
        Duration::from_secs(30),
    );
    acquired(
        repository
            .try_acquire_bounded_lease(&interval_request)
            .await
            .expect("acquire interval slot"),
    )
    .release()
    .await
    .expect("release interval slot");
    let interval_denied = repository
        .try_acquire_bounded_lease(&interval_request)
        .await
        .expect("interval decision");
    assert!(matches!(
        interval_denied,
        CredentialBoundedLeaseAcquisition::Busy {
            retry_after: Some(value)
        } if value > Duration::ZERO && value <= Duration::from_secs(30)
    ));

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
    let mut request = scheduling_request("acct_crash_ttl", "worker-crash", 1, Duration::ZERO);
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
async fn scheduling_cursor_is_shared_across_gateway_processes() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let peer = RedisCredentialLeaseRepository::new(connection.clone(), &namespace)
        .expect("peer repository");

    assert_eq!(
        repository
            .advance_scheduling_cursor("route_openai_real")
            .await
            .expect("first cursor"),
        0
    );
    assert_eq!(
        peer.advance_scheduling_cursor("route_openai_real")
            .await
            .expect("shared cursor"),
        1
    );
    assert_eq!(
        peer.advance_scheduling_cursor("route_openai_backup")
            .await
            .expect("independent cursor"),
        0
    );

    let keys = redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async::<Vec<String>>(&mut connection)
        .await
        .expect("list cursor keys");
    assert_eq!(keys.len(), 2);
    assert!(
        keys.iter().all(|key| {
            !key.contains("route_openai_real") && !key.contains("route_openai_backup")
        })
    );
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
        request_interval: Duration::ZERO,
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
    request_interval: Duration,
) -> CredentialBoundedLeaseRequest {
    CredentialBoundedLeaseRequest {
        scope: CredentialLeaseScope::ProviderAccount,
        resource_id: resource_id.to_owned(),
        owner_id: owner_id.to_owned(),
        max_concurrent,
        request_interval,
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
    let redis_url = std::env::var("CPR_TEST_REDIS_URL").ok()?;
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
