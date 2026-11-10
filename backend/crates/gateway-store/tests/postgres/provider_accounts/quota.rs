//! 共用额度存储的连接隔离、版本读取与大快照生命周期

use super::*;
use gateway_core::account::UpstreamConnection;

#[tokio::test]
async fn connection_changes_reject_in_flight_quota_writes() {
    for change in ["credential", "proxy", "disabled", "deleted"] {
        let Some(database) = TestDatabase::create("quota_connection").await else {
            return;
        };
        let repository = PgProviderAccountRepository::new(database.pool.clone());
        repository
            .insert_provider_account(account("acct_peer", "fixture"))
            .await
            .unwrap();
        let id = ProviderAccountId::new("acct_peer").unwrap();
        let current = repository.get_account(&id).await.unwrap().unwrap();
        let observation = QuotaObservation {
            quota_pause: None,
            expected_connection: Some(UpstreamConnection::for_account(&current)),
            account_id: id.clone(),
            expected_revision: current.revision(),
            quota: OpaqueProviderData::new(json!({"value":"late"}).as_object().unwrap().clone()),
            observed_at: SystemTime::now(),
            plan_type: None,
            state: QuotaState::unknown(),
        };
        let sql = match change {
            "credential" => {
                "update provider_accounts set credential_revision = credential_revision + 1 where id = 'acct_peer'"
            }
            "proxy" => {
                "update provider_accounts set outbound_proxy_url = 'http://127.0.0.1:8080' where id = 'acct_peer'"
            }
            "disabled" => "update provider_accounts set enabled = false where id = 'acct_peer'",
            _ => "delete from provider_accounts where id = 'acct_peer'",
        };
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .execute(&database.pool)
            .await
            .unwrap();
        assert_eq!(
            repository
                .compare_and_swap_quota(observation)
                .await
                .unwrap(),
            QuotaWriteOutcome::Conflict,
            "{change}"
        );
        assert!(repository.get_quotas(&[id]).await.unwrap().is_empty());
        database.close().await;
    }
}

#[tokio::test]
async fn large_quota_uses_shared_snapshot_and_cannot_reappear_after_revocation() {
    let Some(database) = TestDatabase::create("large_quota").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    repository
        .insert_provider_account(account("acct_peer", "fixture"))
        .await
        .unwrap();
    let id = ProviderAccountId::new("acct_peer").unwrap();
    let current = repository.get_account(&id).await.unwrap().unwrap();
    let now = SystemTime::now();
    let observation = QuotaObservation {
        quota_pause: None,
        expected_connection: Some(UpstreamConnection::for_account(&current)),
        account_id: id.clone(),
        expected_revision: current.revision(),
        quota: OpaqueProviderData::new(
            json!({"payload":"x".repeat(2 * 1024 * 1024)})
                .as_object()
                .unwrap()
                .clone(),
        ),
        observed_at: now,
        plan_type: None,
        state: QuotaState::unknown(),
    };
    assert_eq!(
        repository
            .compare_and_swap_quota(observation.clone())
            .await
            .unwrap(),
        QuotaWriteOutcome::Updated
    );
    assert!(repository.load_current_credential(&id).await.is_ok());
    let versions = repository
        .get_quota_versions(std::slice::from_ref(&id))
        .await
        .unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].credential_revision, current.revision());
    assert_eq!(
        repository
            .get_quotas(std::slice::from_ref(&id))
            .await
            .unwrap()[0]
            .quota,
        observation.quota
    );
    let mut cleared = observation.clone();
    cleared.quota = OpaqueProviderData::new(Default::default());
    cleared.observed_at = now + Duration::from_secs(1);
    repository.compare_and_swap_quota(cleared).await.unwrap();
    assert_eq!(
        repository
            .compare_and_swap_quota(observation)
            .await
            .unwrap(),
        QuotaWriteOutcome::Conflict
    );
    assert!(
        repository
            .get_quotas(std::slice::from_ref(&id))
            .await
            .unwrap()[0]
            .quota
            .expose_to_provider()
            .is_empty()
    );
    repository.set_enabled(&id, false).await.unwrap();
    repository.delete_account(&id).await.unwrap();
    assert!(
        repository
            .get_quota_versions(&[id])
            .await
            .unwrap()
            .is_empty()
    );
    database.close().await;
}

#[tokio::test]
async fn snapshot_commit_preserves_newer_quota_failure() {
    let Some(database) = TestDatabase::create("quota_connection_evidence").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    repository
        .insert_provider_account(account("acct_peer", "fixture"))
        .await
        .unwrap();
    let id = ProviderAccountId::new("acct_peer").unwrap();
    let current = repository.get_account(&id).await.unwrap().unwrap();
    let now = SystemTime::now();
    repository
        .apply_quota_access(QuotaAccessChange {
            account_id: id.clone(),
            expected_revision: current.revision(),
            state: QuotaState::exhausted(
                QuotaEvidence::UsageLimitReached,
                now + Duration::from_secs(1),
                None,
            ),
        })
        .await
        .unwrap();
    let observation = QuotaObservation {
        quota_pause: None,
        expected_connection: Some(UpstreamConnection::for_account(&current)),
        account_id: id.clone(),
        expected_revision: current.revision(),
        quota: OpaqueProviderData::new(json!({"observed":"valid"}).as_object().unwrap().clone()),
        observed_at: now,
        plan_type: None,
        state: QuotaState::allowed(now),
    };
    assert_eq!(
        repository
            .compare_and_swap_quota(observation)
            .await
            .unwrap(),
        QuotaWriteOutcome::Updated
    );
    assert!(
        repository
            .get_account(&id)
            .await
            .unwrap()
            .unwrap()
            .quota()
            .is_exhausted()
    );
    database.close().await;
}

#[tokio::test]
async fn managed_proxy_changes_clear_snapshots_and_reject_old_connections() {
    use gateway_admin::{
        model::{
            MutationActor, MutationContext,
            proxies::{NewProxy, UpdateProxy},
        },
        ports::proxy::ProxyStore,
    };
    use gateway_store::postgres::PgProxyRepository;
    let Some(database) = TestDatabase::create("quota_managed_proxy").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let proxies = PgProxyRepository::new(database.pool.clone());
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "quota-proxy-test".to_owned(),
    };
    let mut proxy = proxies
        .create(
            NewProxy {
                name: "fixture".to_owned(),
                proxy: gateway_core::account::OutboundProxy::parse("http://127.0.0.1:18080")
                    .unwrap(),
                auto_location: false,
                location: None,
                test: None,
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    let mut input = account("acct_peer", "fixture");
    input.outbound_proxy = Some(proxy.proxy.clone());
    repository.insert_provider_account(input).await.unwrap();
    let id = ProviderAccountId::new("acct_peer").unwrap();
    for change in ["url", "disabled"] {
        let current = repository.get_account(&id).await.unwrap().unwrap();
        let observation = QuotaObservation {
            quota_pause: None,
            expected_connection: Some(UpstreamConnection::for_account(&current)),
            account_id: id.clone(),
            expected_revision: current.revision(),
            quota: OpaqueProviderData::new(
                json!({"value":"snapshot"}).as_object().unwrap().clone(),
            ),
            observed_at: SystemTime::now(),
            plan_type: None,
            state: QuotaState::unknown(),
        };
        assert_eq!(
            repository
                .compare_and_swap_quota(observation.clone())
                .await
                .unwrap(),
            QuotaWriteOutcome::Updated
        );
        proxy = proxies
            .update(
                UpdateProxy {
                    id: proxy.id.clone(),
                    revision: proxy.revision,
                    name: proxy.name.clone(),
                    proxy: (change == "url").then(|| {
                        gateway_core::account::OutboundProxy::parse("http://127.0.0.1:18081")
                            .unwrap()
                    }),
                    enabled: (change == "disabled").then_some(false),
                    location: None,
                    auto_location: None,
                    max_websocket_connections: None,
                    test: None,
                },
                &context,
            )
            .await
            .unwrap()
            .record;
        assert!(
            repository
                .get_quotas(std::slice::from_ref(&id))
                .await
                .unwrap()
                .is_empty(),
            "{change}"
        );
        assert_eq!(
            repository
                .compare_and_swap_quota(observation)
                .await
                .unwrap(),
            QuotaWriteOutcome::Conflict,
            "{change}"
        );
        assert_eq!(
            repository
                .get_account(&id)
                .await
                .unwrap()
                .unwrap()
                .revision(),
            current.revision()
        );
    }
    // 模拟代理更新持有账号锁，额度查询已经开始但必须等待提交的交错
    let current = repository.get_account(&id).await.unwrap().unwrap();
    let observation = QuotaObservation {
        quota_pause: None,
        expected_connection: Some(UpstreamConnection::for_account(&current)),
        account_id: id.clone(),
        expected_revision: current.revision(),
        quota: OpaqueProviderData::new(json!({"value":"late"}).as_object().unwrap().clone()),
        observed_at: SystemTime::now(),
        plan_type: None,
        state: QuotaState::unknown(),
    };
    let mut transaction = database.pool.begin().await.unwrap();
    let blocker: i32 = sqlx::query_scalar("select pg_backend_pid()")
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("update outbound_proxies set enabled = true where id = $1")
        .bind(&proxy.id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("update provider_accounts set updated_at = updated_at where id = $1")
        .bind(id.as_str())
        .execute(&mut *transaction)
        .await
        .unwrap();
    let pool = database.pool.clone();
    let writer = tokio::spawn(async move {
        PgProviderAccountRepository::new(pool)
            .compare_and_swap_quota(observation)
            .await
            .unwrap()
    });
    // 控制连接独立于已被更新事务和额度写入占用的数据连接池
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("select exists(select 1 from pg_stat_activity where $1 = any(pg_blocking_pids(pid)))").bind(blocker).fetch_one(&database.admin).await.unwrap();
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    transaction.commit().await.unwrap();
    assert_eq!(writer.await.unwrap(), QuotaWriteOutcome::Conflict);
    assert!(repository.get_quotas(&[id]).await.unwrap().is_empty());
    database.close().await;
}
