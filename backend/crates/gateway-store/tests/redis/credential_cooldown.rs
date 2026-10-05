//! 验证账号冷却缓存的版本隔离、期限延长与到期清理

use std::time::{Duration as StdDuration, SystemTime};

use chrono::{DateTime, Duration, Utc};
use gateway_core::{
    account::{CredentialRevision, ProviderAccountId},
    provider_ports::{
        ProviderCooldownKind, ProviderCooldownPort, ProviderCooldownScope, ProviderScopedCooldown,
    },
};
use gateway_store::{
    Revision,
    redis::{CredentialCooldown, CredentialCooldownRepository, RedisCredentialCooldownRepository},
};
use redis::aio::ConnectionManager;
use uuid::Uuid;

#[test]
fn credential_cooldown_is_revision_fenced() {
    let cooldown = CredentialCooldown {
        provider_account_id: "account-1".to_owned(),
        credential_revision: Revision::new(2).expect("positive revision"),
        cooldown_until: Utc::now() + Duration::seconds(30),
        kind: ProviderCooldownKind::RateLimit,
    };
    assert_eq!(cooldown.credential_revision.get(), 2);
}

#[tokio::test]
async fn credential_cooldown_round_trips_and_indexes_active_account_without_raw_id_in_keys() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let cooldown = cooldown("acct_cooldown_round_trip", 1, 30);

    assert!(
        repository
            .cache_credential_cooldown(&cooldown)
            .await
            .expect("cache cooldown")
    );
    assert_eq!(
        repository
            .read_credential_cooldown(&cooldown.provider_account_id)
            .await
            .expect("read cooldown"),
        Some(cooldown.clone())
    );
    let mut keys = namespace_keys(&mut connection, &namespace).await;
    keys.sort();
    assert_eq!(keys.len(), 2);
    assert!(
        keys.iter()
            .all(|key| !key.contains(&cooldown.provider_account_id))
    );

    let index_key = keys
        .iter()
        .find(|key| key.ends_with(":account:active-cooldowns"))
        .expect("active cooldown index key");
    let indexed_accounts: Vec<String> = redis::cmd("ZRANGE")
        .arg(index_key)
        .arg(0)
        .arg(-1)
        .query_async(&mut connection)
        .await
        .expect("read active cooldown index");
    assert_eq!(indexed_accounts, [cooldown.provider_account_id]);
}

#[tokio::test]
async fn credential_cooldown_rejects_older_revision_and_fences_invalidation() {
    let Some((repository, _connection, _namespace)) = repository().await else {
        return;
    };
    let current = cooldown("acct_cooldown_revision", 2, 30);
    let stale = cooldown("acct_cooldown_revision", 1, 60);
    repository
        .cache_credential_cooldown(&current)
        .await
        .expect("cache current cooldown");

    assert!(
        !repository
            .cache_credential_cooldown(&stale)
            .await
            .expect("reject stale cooldown")
    );
    assert!(
        !repository
            .invalidate_credential_cooldown(
                &current.provider_account_id,
                Revision::new(1).expect("positive revision"),
            )
            .await
            .expect("fence stale invalidation")
    );
    assert!(
        repository
            .invalidate_credential_cooldown(
                &current.provider_account_id,
                current.credential_revision,
            )
            .await
            .expect("invalidate current cooldown")
    );
    assert_eq!(
        repository
            .read_credential_cooldown(&current.provider_account_id)
            .await
            .expect("read invalidated cooldown"),
        None
    );
}

#[tokio::test]
async fn credential_cooldown_same_revision_only_extends_deadline() {
    let Some((repository, _connection, _namespace)) = repository().await else {
        return;
    };
    let initial = cooldown("acct_cooldown_extend", 3, 30);
    let shorter = CredentialCooldown {
        cooldown_until: initial.cooldown_until - Duration::seconds(5),
        ..initial.clone()
    };
    let longer = CredentialCooldown {
        cooldown_until: initial.cooldown_until + Duration::seconds(5),
        ..initial.clone()
    };
    repository
        .cache_credential_cooldown(&initial)
        .await
        .expect("cache initial cooldown");

    assert!(
        !repository
            .cache_credential_cooldown(&shorter)
            .await
            .expect("reject shorter cooldown")
    );
    assert!(
        repository
            .cache_credential_cooldown(&longer)
            .await
            .expect("extend cooldown")
    );
    assert_eq!(
        repository
            .read_credential_cooldown(&initial.provider_account_id)
            .await
            .expect("read extended cooldown"),
        Some(longer)
    );
}

#[tokio::test]
async fn credential_cooldown_read_removes_expired_grace_key() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let cooldown_until = Utc::now() + Duration::milliseconds(40);
    let cooldown = CredentialCooldown {
        provider_account_id: "acct_cooldown_expiry".to_owned(),
        credential_revision: Revision::new(1).expect("positive revision"),
        cooldown_until: millisecond_precision(cooldown_until),
        kind: ProviderCooldownKind::RateLimit,
    };
    repository
        .cache_credential_cooldown(&cooldown)
        .await
        .expect("cache short cooldown");
    tokio::time::sleep(StdDuration::from_millis(80)).await;

    assert_eq!(
        repository
            .read_credential_cooldown(&cooldown.provider_account_id)
            .await
            .expect("read expired cooldown"),
        None
    );
    assert!(namespace_keys(&mut connection, &namespace).await.is_empty());
}

#[tokio::test]
async fn scoped_cooldown_isolated_by_model_and_revision_fenced() {
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let account_id = ProviderAccountId::new("acct_scoped_cooldown").expect("valid account ID");
    let revision = CredentialRevision::new(2).expect("positive revision");
    let model_a = ProviderCooldownScope::upstream_model(
        gateway_core::routing::UpstreamModelId::new("grok-4.5").expect("model"),
    );
    let model_b = ProviderCooldownScope::upstream_model(
        gateway_core::routing::UpstreamModelId::new("grok-4.6").expect("model"),
    );
    let until_a: SystemTime = millisecond_precision(Utc::now() + Duration::seconds(30)).into();
    let until_b: SystemTime = millisecond_precision(Utc::now() + Duration::seconds(60)).into();

    assert!(
        repository
            .put_scoped_if_later(ProviderScopedCooldown::new(
                account_id.clone(),
                revision,
                model_a.clone(),
                until_a,
            ))
            .await
            .expect("cache model A cooldown")
    );
    assert!(
        !repository
            .put_scoped_if_later(ProviderScopedCooldown::new(
                account_id.clone(),
                CredentialRevision::new(1).expect("stale revision"),
                model_a.clone(),
                until_b,
            ))
            .await
            .expect("reject stale model A cooldown")
    );
    assert!(
        repository
            .put_scoped_if_later(ProviderScopedCooldown::new(
                account_id.clone(),
                revision,
                model_b.clone(),
                until_b,
            ))
            .await
            .expect("cache model B cooldown")
    );
    assert_eq!(
        repository
            .read_scoped(&account_id, &model_a)
            .await
            .expect("read model A cooldown")
            .expect("model A cooldown")
            .until(),
        until_a
    );
    assert_eq!(
        repository
            .read_scoped(&account_id, &model_b)
            .await
            .expect("read model B cooldown")
            .expect("model B cooldown")
            .until(),
        until_b
    );
    assert!(
        !repository
            .clear_scoped(
                &account_id,
                &model_a,
                CredentialRevision::new(1).expect("stale revision"),
            )
            .await
            .expect("fence stale model A invalidation")
    );
    assert!(
        repository
            .clear_scoped(&account_id, &model_a, revision)
            .await
            .expect("clear model A cooldown")
    );
    assert!(
        repository
            .read_scoped(&account_id, &model_a)
            .await
            .expect("read cleared model A cooldown")
            .is_none()
    );
    assert!(
        repository
            .read_scoped(&account_id, &model_b)
            .await
            .expect("read retained model B cooldown")
            .is_some()
    );
    let keys = namespace_keys(&mut connection, &namespace).await;
    assert_eq!(keys.len(), 1);
    assert!(!keys[0].contains(account_id.as_str()));
    assert!(!keys[0].contains(model_b.value()));
}

fn cooldown(account_id: &str, revision: u64, seconds: i64) -> CredentialCooldown {
    CredentialCooldown {
        provider_account_id: account_id.to_owned(),
        credential_revision: Revision::new(revision).expect("positive revision"),
        cooldown_until: millisecond_precision(Utc::now() + Duration::seconds(seconds)),
        kind: ProviderCooldownKind::RateLimit,
    }
}

fn millisecond_precision(value: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(value.timestamp_millis()).expect("valid timestamp")
}

async fn repository() -> Option<(RedisCredentialCooldownRepository, ConnectionManager, String)> {
    let redis_url = crate::support::test_env("CPR_TEST_REDIS_URL")?;
    let client = redis::Client::open(redis_url).expect("valid CPR_TEST_REDIS_URL");
    let connection = client
        .get_connection_manager()
        .await
        .expect("connect test Redis");
    let namespace = format!("gateway-store-cooldown-test-{}", Uuid::new_v4());
    let repository = RedisCredentialCooldownRepository::new(connection.clone(), &namespace)
        .expect("valid cooldown namespace");
    Some((repository, connection, namespace))
}

async fn namespace_keys(connection: &mut ConnectionManager, namespace: &str) -> Vec<String> {
    redis::cmd("KEYS")
        .arg(format!("{namespace}:*"))
        .query_async(connection)
        .await
        .expect("list isolated cooldown keys")
}

fn runtime_store(
    repository: &RedisCredentialCooldownRepository,
    connection: ConnectionManager,
    namespace: &str,
) -> gateway_store::redis::RedisAdminAccountRuntimeStore {
    gateway_store::redis::RedisAdminAccountRuntimeStore::new(
        repository.clone(),
        gateway_store::redis::RedisCredentialLeaseRepository::new(connection, namespace)
            .expect("lease repository"),
    )
}

#[tokio::test]
async fn capacity_freeze_survives_due_time_and_runtime_restart_until_probe_succeeds() {
    use gateway_admin::ports::store::AccountRuntimeStore as _;
    let Some((repository, mut connection, namespace)) = repository().await else {
        return;
    };
    let frozen = CredentialCooldown {
        kind: ProviderCooldownKind::CapacityFreezeProbe,
        cooldown_until: millisecond_precision(Utc::now() - Duration::seconds(90)),
        ..cooldown("acct_probe_due", 1, 30)
    };
    repository
        .cache_credential_cooldown(&frozen)
        .await
        .expect("write due freeze");
    let runtime = runtime_store(&repository, connection.clone(), &namespace);
    let snapshot = runtime.active_rate_limits().await.expect("status snapshot");
    assert!(snapshot.cooldown[&frozen.provider_account_id].is_active(SystemTime::now()));
    assert!(
        snapshot.cooldown[&frozen.provider_account_id]
            .kind
            .requires_probe()
    );
    let key = namespace_keys(&mut connection, &namespace)
        .await
        .into_iter()
        .find(|key| key.ends_with(":cooldown"))
        .expect("freeze key");
    let ttl: i64 = redis::cmd("PTTL")
        .arg(key)
        .query_async(&mut connection)
        .await
        .expect("TTL");
    assert_eq!(
        ttl, -1,
        "awaiting probe must not expire while worker is unavailable"
    );
    drop(runtime);
    let restarted = runtime_store(&repository, connection, &namespace);
    let freezes = restarted
        .active_freezes()
        .await
        .expect("due freezes after restart");
    assert_eq!(freezes.len(), 1);
    assert!(
        restarted
            .finish_freeze(
                &frozen.provider_account_id,
                &freezes[&frozen.provider_account_id],
                None
            )
            .await
            .expect("probe completion")
    );
    assert!(
        repository
            .read_credential_cooldown(&frozen.provider_account_id)
            .await
            .expect("read")
            .is_none()
    );
}

#[tokio::test]
async fn stale_probe_cannot_recreate_manual_recovery_or_clear_or_extend_new_freeze() {
    use gateway_admin::ports::store::AccountRuntimeStore as _;
    let Some((repository, connection, namespace)) = repository().await else {
        return;
    };
    let frozen = CredentialCooldown {
        kind: ProviderCooldownKind::CapacityFreezeProbe,
        ..cooldown("acct_probe_generation", 1, 30)
    };
    let runtime = runtime_store(&repository, connection, &namespace);
    repository
        .cache_credential_cooldown(&frozen)
        .await
        .expect("freeze");
    let original =
        runtime.active_freezes().await.expect("snapshot")[&frozen.provider_account_id].clone();
    repository
        .delete_account_cooldowns(&frozen.provider_account_id)
        .await
        .expect("manual recovery");
    let postponed = Utc::now() + Duration::hours(2);
    assert!(
        !runtime
            .finish_freeze(&frozen.provider_account_id, &original, Some(postponed))
            .await
            .expect("stale failure")
    );
    assert!(
        repository
            .read_credential_cooldown(&frozen.provider_account_id)
            .await
            .expect("read")
            .is_none()
    );
    // 同凭据版本、相同截止时间的新冻结仍必须视为不同代次
    repository
        .cache_credential_cooldown(&frozen)
        .await
        .expect("new freeze");
    assert!(
        !runtime
            .finish_freeze(&frozen.provider_account_id, &original, None)
            .await
            .expect("stale success")
    );
    assert!(
        !runtime
            .finish_freeze(&frozen.provider_account_id, &original, Some(postponed))
            .await
            .expect("stale failure")
    );
    let current =
        runtime.active_freezes().await.expect("new snapshot")[&frozen.provider_account_id].clone();
    assert_ne!(current.generation, original.generation);
    assert!(
        runtime
            .finish_freeze(&frozen.provider_account_id, &current, Some(postponed))
            .await
            .expect("postpone current freeze")
    );
    assert!(
        !runtime
            .finish_freeze(&frozen.provider_account_id, &current, None)
            .await
            .expect("result from previous probe cycle")
    );
    let extended = runtime.active_freezes().await.expect("extended snapshot")
        [&frozen.provider_account_id]
        .clone();
    assert!(extended.until >= current.until);
    assert!(
        runtime
            .finish_freeze(&frozen.provider_account_id, &extended, None)
            .await
            .expect("recover current cycle")
    );
}

#[tokio::test]
async fn ordinary_success_and_later_429_preserve_capacity_freeze_and_peak_evidence() {
    let Some((repository, _connection, _namespace)) = repository().await else {
        return;
    };
    let id = ProviderAccountId::new("acct_late_success").expect("account id");
    let revision = CredentialRevision::new(1).expect("revision");
    repository
        .record_capacity_failure(&id, StdDuration::from_secs(600), 8)
        .await
        .expect("failure evidence");
    let frozen = CredentialCooldown {
        kind: ProviderCooldownKind::CapacityFreezeProbe,
        ..cooldown(id.as_str(), 1, 30)
    };
    repository
        .cache_credential_cooldown(&frozen)
        .await
        .expect("freeze");
    repository
        .clear_after_success(&id, revision)
        .await
        .expect("late success");
    assert_eq!(
        repository.capacity_peak_in_flight(&id).await.expect("peak"),
        Some(8)
    );
    assert!(
        !repository
            .cache_credential_cooldown(&cooldown(id.as_str(), 1, 60))
            .await
            .expect("429 must not downgrade freeze")
    );
    assert_eq!(
        repository
            .read_credential_cooldown(id.as_str())
            .await
            .expect("read"),
        Some(frozen)
    );
}

#[tokio::test]
async fn ordinary_success_still_clears_rate_limit_and_unfrozen_failure_evidence() {
    let Some((repository, _connection, _namespace)) = repository().await else {
        return;
    };
    let id = ProviderAccountId::new("acct_success_429").expect("id");
    repository
        .record_capacity_failure(&id, StdDuration::from_secs(600), 3)
        .await
        .expect("failure evidence");
    repository
        .cache_credential_cooldown(&cooldown(id.as_str(), 1, 30))
        .await
        .expect("429");
    repository
        .clear_after_success(&id, CredentialRevision::new(1).expect("revision"))
        .await
        .expect("success");
    assert!(
        repository
            .read_credential_cooldown(id.as_str())
            .await
            .expect("read")
            .is_none()
    );
    assert!(
        repository
            .capacity_peak_in_flight(&id)
            .await
            .expect("peak")
            .is_none()
    );
}
