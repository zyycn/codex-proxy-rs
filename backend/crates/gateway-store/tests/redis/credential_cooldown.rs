use std::{
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, SystemTime},
};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use gateway_core::{
    engine::credential::{
        AccountAvailability, AccountStateChange, CredentialCasOutcome, CredentialCasUpdate,
        CredentialRevision, LoadedCredential, NewProviderAccount, PlaintextCredential,
        ProviderAccount, ProviderAccountId, ProviderAccountStore, ProviderAccountUpdate,
        QuotaObservation, QuotaWriteOutcome,
    },
    error::{StoreError as CoreStoreError, StoreErrorKind},
    routing::ProviderKind,
};
use gateway_store::{
    Revision, StoreBackend, StoreError, StoreResult,
    redis::{
        CooldownCachingProviderAccountStore, CredentialCooldown, CredentialCooldownRepository,
        RedisCredentialCooldownRepository,
    },
};
use redis::aio::ConnectionManager;
use uuid::Uuid;

#[test]
fn credential_cooldown_is_revision_fenced() {
    let cooldown = CredentialCooldown {
        provider_account_id: "account-1".to_owned(),
        credential_revision: Revision::new(2).expect("positive revision"),
        cooldown_until: Utc::now() + Duration::seconds(30),
    };
    assert_eq!(cooldown.credential_revision.get(), 2);
}

struct CooldownTestAccountStore {
    accounts: Vec<ProviderAccount>,
    calls: Arc<Mutex<Vec<&'static str>>>,
    fail_state_write: bool,
}

#[async_trait]
impl ProviderAccountStore for CooldownTestAccountStore {
    async fn create_account(&self, _account: NewProviderAccount) -> Result<(), CoreStoreError> {
        unreachable!("cooldown tests do not create accounts")
    }

    async fn get_account(
        &self,
        _account: &ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, CoreStoreError> {
        unreachable!("cooldown tests do not get one account")
    }

    async fn list_accounts(&self) -> Result<Vec<ProviderAccount>, CoreStoreError> {
        Ok(self.accounts.clone())
    }

    async fn list_for_provider(
        &self,
        _provider: &ProviderKind,
    ) -> Result<Vec<ProviderAccount>, CoreStoreError> {
        unreachable!("cooldown tests do not list one provider")
    }

    async fn load_credential(
        &self,
        _account: &ProviderAccountId,
        _expected_revision: CredentialRevision,
    ) -> Result<LoadedCredential, CoreStoreError> {
        unreachable!("cooldown tests do not load credentials")
    }

    async fn compare_and_swap_credential(
        &self,
        _update: CredentialCasUpdate,
    ) -> Result<CredentialCasOutcome, CoreStoreError> {
        self.calls.lock().expect("calls lock").push("postgres");
        Ok(CredentialCasOutcome::Updated(
            CredentialRevision::new(8).expect("positive revision"),
        ))
    }

    async fn get_quotas(
        &self,
        _accounts: &[ProviderAccountId],
    ) -> Result<Vec<QuotaObservation>, CoreStoreError> {
        unreachable!("cooldown tests do not read quota")
    }

    async fn compare_and_swap_quota(
        &self,
        _observation: QuotaObservation,
    ) -> Result<QuotaWriteOutcome, CoreStoreError> {
        unreachable!("cooldown tests do not write quota")
    }

    async fn apply_state_change(&self, _change: AccountStateChange) -> Result<(), CoreStoreError> {
        self.calls.lock().expect("calls lock").push("postgres");
        if self.fail_state_write {
            Err(CoreStoreError::new(StoreErrorKind::Unavailable))
        } else {
            Ok(())
        }
    }

    async fn update_account(&self, _update: ProviderAccountUpdate) -> Result<(), CoreStoreError> {
        unreachable!("cooldown tests do not update account metadata")
    }

    async fn set_enabled(
        &self,
        _account: &ProviderAccountId,
        _enabled: bool,
    ) -> Result<(), CoreStoreError> {
        unreachable!("cooldown tests do not change account switches")
    }

    async fn delete_account(&self, _account: &ProviderAccountId) -> Result<(), CoreStoreError> {
        self.calls.lock().expect("calls lock").push("postgres");
        Ok(())
    }
}

struct CooldownTestCache {
    calls: Arc<Mutex<Vec<&'static str>>>,
    cached: Mutex<Vec<CredentialCooldown>>,
    invalidated: Mutex<Vec<(String, Revision)>>,
    fail: bool,
}

#[async_trait]
impl CredentialCooldownRepository for CooldownTestCache {
    async fn cache_credential_cooldown(&self, cooldown: &CredentialCooldown) -> StoreResult<bool> {
        self.calls.lock().expect("calls lock").push("redis");
        if self.fail {
            return Err(StoreError::Unavailable {
                backend: StoreBackend::Redis,
                message: "test cache unavailable".to_owned(),
            });
        }
        self.cached
            .lock()
            .expect("cached lock")
            .push(cooldown.clone());
        Ok(true)
    }

    async fn read_credential_cooldown(
        &self,
        _provider_account_id: &str,
    ) -> StoreResult<Option<CredentialCooldown>> {
        unreachable!("write-through adapter does not consume cooldown cache")
    }

    async fn invalidate_credential_cooldown(
        &self,
        provider_account_id: &str,
        through_revision: Revision,
    ) -> StoreResult<bool> {
        self.calls.lock().expect("calls lock").push("redis");
        if self.fail {
            return Err(StoreError::Unavailable {
                backend: StoreBackend::Redis,
                message: "test cache unavailable".to_owned(),
            });
        }
        self.invalidated
            .lock()
            .expect("invalidated lock")
            .push((provider_account_id.to_owned(), through_revision));
        Ok(true)
    }
}

fn composition_fixture(
    fail_state_write: bool,
    fail_cache: bool,
) -> (
    CooldownCachingProviderAccountStore,
    Arc<CooldownTestCache>,
    Arc<Mutex<Vec<&'static str>>>,
) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let authoritative = Arc::new(CooldownTestAccountStore {
        accounts: Vec::new(),
        calls: Arc::clone(&calls),
        fail_state_write,
    });
    let cache = Arc::new(CooldownTestCache {
        calls: Arc::clone(&calls),
        cached: Mutex::new(Vec::new()),
        invalidated: Mutex::new(Vec::new()),
        fail: fail_cache,
    });
    let store = CooldownCachingProviderAccountStore::new(
        authoritative,
        Arc::clone(&cache) as Arc<dyn CredentialCooldownRepository>,
    );
    (store, cache, calls)
}

#[tokio::test]
async fn cooldown_composition_writes_cache_only_after_postgres_commit() {
    let (store, cache, calls) = composition_fixture(false, false);

    store
        .apply_state_change(cooldown_state_change(60))
        .await
        .expect("persist cooldown");

    assert_eq!(*calls.lock().expect("calls lock"), ["postgres", "redis"]);
    assert_eq!(cache.cached.lock().expect("cached lock").len(), 1);
}

#[tokio::test]
async fn cooldown_composition_does_not_cache_failed_postgres_state() {
    let (store, cache, calls) = composition_fixture(true, false);

    assert!(
        store
            .apply_state_change(cooldown_state_change(60))
            .await
            .is_err()
    );
    assert_eq!(*calls.lock().expect("calls lock"), ["postgres"]);
    assert!(cache.cached.lock().expect("cached lock").is_empty());
}

#[tokio::test]
async fn cooldown_composition_keeps_postgres_terminal_when_redis_fails() {
    let (store, _cache, calls) = composition_fixture(false, true);

    store
        .apply_state_change(cooldown_state_change(60))
        .await
        .expect("Redis is only a cache");

    assert_eq!(*calls.lock().expect("calls lock"), ["postgres", "redis"]);
}

#[tokio::test]
async fn ready_state_invalidates_same_revision_cooldown_after_postgres_commit() {
    let (store, cache, calls) = composition_fixture(false, false);
    let mut change = cooldown_state_change(60);
    change.availability = AccountAvailability::Ready;
    change.reason = None;
    change.cooldown_until = None;

    store
        .apply_state_change(change)
        .await
        .expect("persist ready state");

    assert_eq!(
        cache.invalidated.lock().expect("invalidated lock")[0].1,
        Revision::new(7).expect("positive revision")
    );
    assert_eq!(*calls.lock().expect("calls lock"), ["postgres", "redis"]);
}

#[tokio::test]
async fn credential_rotation_invalidates_cooldown_through_new_revision() {
    let (store, cache, calls) = composition_fixture(false, false);

    let outcome = store
        .compare_and_swap_credential(credential_update())
        .await
        .expect("rotate credential");

    assert_eq!(
        outcome,
        CredentialCasOutcome::Updated(CredentialRevision::new(8).expect("positive revision"))
    );
    assert_eq!(
        cache.invalidated.lock().expect("invalidated lock")[0].1,
        Revision::new(8).expect("positive revision")
    );
    assert_eq!(*calls.lock().expect("calls lock"), ["postgres", "redis"]);
}

#[tokio::test]
async fn account_delete_invalidates_all_stale_cooldown_revisions() {
    let (store, cache, calls) = composition_fixture(false, false);
    let account_id = ProviderAccountId::new("acct_cooldown_delete").expect("valid account ID");

    store
        .delete_account(&account_id)
        .await
        .expect("delete account");

    assert_eq!(
        cache.invalidated.lock().expect("invalidated lock")[0],
        (
            account_id.to_string(),
            Revision::new(u64::MAX).expect("positive revision")
        )
    );
    assert_eq!(*calls.lock().expect("calls lock"), ["postgres", "redis"]);
}

#[tokio::test]
async fn cooldown_startup_hydrates_only_active_postgres_cooldowns() {
    let now = SystemTime::now();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let authoritative = Arc::new(CooldownTestAccountStore {
        accounts: vec![
            cooldown_test_account("acct_hydrate_active", now + StdDuration::from_secs(60)),
            cooldown_test_account("acct_hydrate_expired", now - StdDuration::from_secs(1)),
            ready_test_account("acct_hydrate_ready"),
        ],
        calls: Arc::clone(&calls),
        fail_state_write: false,
    });
    let cache = Arc::new(CooldownTestCache {
        calls,
        cached: Mutex::new(Vec::new()),
        invalidated: Mutex::new(Vec::new()),
        fail: false,
    });
    let store = CooldownCachingProviderAccountStore::new(
        authoritative,
        Arc::clone(&cache) as Arc<dyn CredentialCooldownRepository>,
    );

    assert_eq!(store.hydrate(now).await, 1);
    assert_eq!(
        cache.cached.lock().expect("cached lock")[0].provider_account_id,
        "acct_hydrate_active"
    );
}

fn cooldown_state_change(seconds: u64) -> AccountStateChange {
    AccountStateChange {
        account_id: ProviderAccountId::new("acct_cooldown_composition").expect("valid account ID"),
        expected_revision: CredentialRevision::new(7).expect("positive revision"),
        availability: AccountAvailability::Cooldown,
        reason: Some("rate_limited".to_owned()),
        cooldown_until: Some(SystemTime::now() + StdDuration::from_secs(seconds)),
        observed_at: SystemTime::now(),
    }
}

fn credential_update() -> CredentialCasUpdate {
    let account_id = ProviderAccountId::new("acct_cooldown_rotation").expect("valid account ID");
    CredentialCasUpdate::new(
        account_id.clone(),
        CredentialRevision::new(7).expect("positive revision"),
        ProviderAccountUpdate {
            account_id,
            name: "Rotated cooldown account".to_owned(),
            email: None,
            plan_type: None,
        },
        PlaintextCredential::new(serde_json::Map::new()),
        false,
        SystemTime::now() + StdDuration::from_secs(3_600),
        None,
    )
    .expect("valid credential update")
}

fn cooldown_test_account(account_id: &str, cooldown_until: SystemTime) -> ProviderAccount {
    test_provider_account(account_id).with_runtime_state(
        true,
        AccountAvailability::Cooldown,
        Some(cooldown_until),
    )
}

fn ready_test_account(account_id: &str) -> ProviderAccount {
    test_provider_account(account_id).with_runtime_state(true, AccountAvailability::Ready, None)
}

fn test_provider_account(account_id: &str) -> ProviderAccount {
    ProviderAccount::new(
        ProviderAccountId::new(account_id).expect("valid account ID"),
        ProviderKind::new("openai").expect("valid provider kind"),
        "Cooldown test".to_owned(),
        "upstream-user".to_owned(),
        CredentialRevision::new(7).expect("positive revision"),
        SystemTime::now() + StdDuration::from_secs(3_600),
    )
}

#[tokio::test]
async fn credential_cooldown_round_trips_without_raw_account_id_in_key() {
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
    let keys = namespace_keys(&mut connection, &namespace).await;
    assert_eq!(keys.len(), 1);
    assert!(!keys[0].contains(&cooldown.provider_account_id));
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

fn cooldown(account_id: &str, revision: u64, seconds: i64) -> CredentialCooldown {
    CredentialCooldown {
        provider_account_id: account_id.to_owned(),
        credential_revision: Revision::new(revision).expect("positive revision"),
        cooldown_until: millisecond_precision(Utc::now() + Duration::seconds(seconds)),
    }
}

fn millisecond_precision(value: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(value.timestamp_millis()).expect("valid timestamp")
}

async fn repository() -> Option<(RedisCredentialCooldownRepository, ConnectionManager, String)> {
    let redis_url = std::env::var("CPR_TEST_REDIS_URL").ok()?;
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
