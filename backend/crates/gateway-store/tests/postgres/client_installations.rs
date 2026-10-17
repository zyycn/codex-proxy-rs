//! 验证配对 installation id 的升级、跨适配器并发、复用与父记录级联清理

use std::time::Duration;

use gateway_core::{
    account::ProviderAccountId,
    policy::ClientApiKeyId,
    provider_ports::{
        ProviderClientInstallationKey, ProviderClientInstallationPort, ProviderStoreErrorKind,
    },
};
use gateway_store::postgres::PgClientInstallationRepository;
use sqlx::PgPool;
use uuid::Uuid;

use super::TestDatabase;

fn pair(client: &str, account: &str) -> ProviderClientInstallationKey {
    ProviderClientInstallationKey::new(
        ClientApiKeyId::new(client).unwrap(),
        ProviderAccountId::new(account).unwrap(),
    )
}

async fn seed_parents(pool: &PgPool) {
    sqlx::query(
        "insert into client_api_keys (id, name, key, created_at, updated_at)
         values ('key_installation_a', 'A', 'sk_' || repeat('a', 43), now(), now()),
                ('key_installation_b', 'B', 'sk_' || repeat('b', 43), now(), now())",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into provider_accounts
            (id, provider_kind, name, authentication_kind, provider_credentials_json,
             has_refresh_token, credential_observed_at, created_at, updated_at)
         values ('acct_installation_a', 'openai', 'A', 'oauth', '{}', false, now(), now(), now()),
                ('acct_installation_b', 'openai', 'B', 'oauth', '{}', false, now(), now(), now())",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn installation_migration_preserves_existing_parents_and_initializes_lazily() {
    let Some(database) = TestDatabase::create_through("installation_upgrade", 33).await else {
        return;
    };
    seed_parents(&database.pool).await;
    super::TEST_MIGRATOR.run(&database.pool).await.unwrap();

    let repository = PgClientInstallationRepository::new(database.pool.clone());
    let key = pair("key_installation_a", "acct_installation_a");
    assert_eq!(repository.read(&key).await.unwrap(), None);
    let candidate = Uuid::new_v4();
    assert_eq!(
        repository.get_or_insert(&key, candidate).await.unwrap(),
        candidate
    );
    let account_credential: serde_json::Value = sqlx::query_scalar(
        "select provider_credentials_json from provider_accounts where id = 'acct_installation_a'",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(account_credential, serde_json::json!({}));
    database.close().await;
}

#[tokio::test]
async fn installation_mappings_are_distinct_per_pair_and_reused_by_new_adapters() {
    let Some(database) = TestDatabase::create("installation_reuse").await else {
        return;
    };
    seed_parents(&database.pool).await;
    let first = PgClientInstallationRepository::new(database.pool.clone());
    let second = PgClientInstallationRepository::new(database.pool.clone());
    let pairs = [
        pair("key_installation_a", "acct_installation_a"),
        pair("key_installation_a", "acct_installation_b"),
        pair("key_installation_b", "acct_installation_a"),
    ];
    let candidates = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    for (key, candidate) in pairs.iter().zip(candidates) {
        assert_eq!(
            first.get_or_insert(key, candidate).await.unwrap(),
            candidate
        );
        assert_eq!(second.read(key).await.unwrap(), Some(candidate));
        assert_eq!(
            second.get_or_insert(key, Uuid::new_v4()).await.unwrap(),
            candidate
        );
    }
    let persisted: Vec<Uuid> = sqlx::query_scalar(
        "select installation_id from openai_client_installations order by client_api_key_id, provider_account_id",
    )
    .fetch_all(&database.pool)
    .await
    .unwrap();
    assert_eq!(persisted, candidates);
    database.close().await;
}

#[tokio::test]
async fn concurrent_adapters_return_the_same_committed_installation() {
    let Some(database) = TestDatabase::create("installation_concurrent").await else {
        return;
    };
    seed_parents(&database.pool).await;
    let first = PgClientInstallationRepository::new(database.pool.clone());
    let second = PgClientInstallationRepository::new(database.pool.clone());
    let key = pair("key_installation_a", "acct_installation_a");
    let candidates = [Uuid::new_v4(), Uuid::new_v4()];
    let (first_result, second_result) = tokio::join!(
        first.get_or_insert(&key, candidates[0]),
        second.get_or_insert(&key, candidates[1]),
    );
    let winner = first_result.unwrap();
    assert_eq!(second_result.unwrap(), winner);
    assert!(candidates.contains(&winner));
    assert_eq!(first.read(&key).await.unwrap(), Some(winner));
    let count: i64 = sqlx::query_scalar("select count(*) from openai_client_installations")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    database.close().await;
}

#[tokio::test]
async fn insert_loser_reads_winner_committed_after_its_statement_snapshot() {
    let Some(database) = TestDatabase::create("installation_conflict_snapshot").await else {
        return;
    };
    seed_parents(&database.pool).await;
    let mut transaction = database.pool.begin().await.unwrap();
    let blocker: i32 = sqlx::query_scalar("select pg_backend_pid()")
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
    let winner = Uuid::new_v4();
    sqlx::query(
        "insert into openai_client_installations
            (client_api_key_id, provider_account_id, installation_id)
         values ('key_installation_a', 'acct_installation_a', $1)",
    )
    .bind(winner)
    .execute(&mut *transaction)
    .await
    .unwrap();

    let competitor = PgClientInstallationRepository::new(database.pool.clone());
    let initialization = tokio::spawn(async move {
        competitor
            .get_or_insert(
                &pair("key_installation_a", "acct_installation_a"),
                Uuid::new_v4(),
            )
            .await
    });
    // 等待真实唯一键冲突，保证竞争语句已取得看不到胜出行的快照
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "select exists(select 1 from pg_locks
                 where not granted and $1 = any(pg_blocking_pids(pid)))",
            )
            .bind(blocker)
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
            if blocked {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("competing insert must wait for the uncommitted winner");
    transaction.commit().await.unwrap();
    assert_eq!(initialization.await.unwrap().unwrap(), winner);
    database.close().await;
}

#[tokio::test]
async fn deleting_either_parent_cascades_only_its_installation_mappings() {
    let Some(database) = TestDatabase::create("installation_cascade").await else {
        return;
    };
    seed_parents(&database.pool).await;
    let repository = PgClientInstallationRepository::new(database.pool.clone());
    let removed = [
        pair("key_installation_a", "acct_installation_a"),
        pair("key_installation_a", "acct_installation_b"),
    ];
    let retained = pair("key_installation_b", "acct_installation_a");
    for key in removed.iter().chain([&retained]) {
        repository.get_or_insert(key, Uuid::new_v4()).await.unwrap();
    }
    let retained_id = repository.read(&retained).await.unwrap().unwrap();
    sqlx::query("delete from client_api_keys where id = 'key_installation_a'")
        .execute(&database.pool)
        .await
        .unwrap();
    for key in &removed {
        assert_eq!(repository.read(key).await.unwrap(), None);
    }
    assert_eq!(repository.read(&retained).await.unwrap(), Some(retained_id));

    sqlx::query("delete from provider_accounts where id = 'acct_installation_a'")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(repository.read(&retained).await.unwrap(), None);
    database.close().await;
}

#[tokio::test]
async fn missing_parent_rejects_creation_without_persisting_the_candidate() {
    let Some(database) = TestDatabase::create("installation_missing_parent").await else {
        return;
    };
    let repository = PgClientInstallationRepository::new(database.pool.clone());
    let key = pair("key_installation_a", "acct_installation_a");
    let error = repository
        .get_or_insert(&key, Uuid::new_v4())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ProviderStoreErrorKind::Unavailable);
    assert_eq!(repository.read(&key).await.unwrap(), None);
    seed_parents(&database.pool).await;
    let candidate = Uuid::new_v4();
    assert_eq!(
        repository.get_or_insert(&key, candidate).await.unwrap(),
        candidate
    );
    database.close().await;
}
