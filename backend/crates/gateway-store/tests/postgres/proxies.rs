//! 验证代理位置、账号绑定、分页查询与事务锁释放

use gateway_admin::{
    model::{
        MutationActor, MutationContext, PageSize,
        accounts::{BatchUpdateAccounts, UpdateAccount},
        proxies::*,
    },
    ports::{
        proxy::ProxyStore,
        store::{AccountStore, AdminStoreErrorKind},
    },
};
use gateway_core::account::{AccountWeight, OutboundProxy, ProviderAccountId};
use gateway_store::postgres::{
    PgProviderAccountRepository, PgProxyRepository, ProviderAccountRepository,
};

use super::{TestDatabase, admin_account_store, provider_accounts::account};

#[tokio::test]
async fn proxy_location_is_shared_preserved_cleared_and_removed_with_binding() {
    use gateway_core::account::{ProviderAccountStore, RequestLocation};
    let Some(database) = TestDatabase::create("proxy_location").await else {
        return;
    };
    let proxies = PgProxyRepository::new(database.pool.clone());
    let accounts = PgProviderAccountRepository::new(database.pool.clone());
    let location: RequestLocation = serde_json::from_value(serde_json::json!({"country":"JP", "region":"Tokyo", "city":"Tokyo", "timezone":"Asia/Tokyo"})).unwrap();
    let mut saved = proxies
        .create(
            NewProxy {
                auto_location: false,
                test: None,
                name: "Tokyo".to_owned(),
                proxy: OutboundProxy::parse("http://127.0.0.1:18080").unwrap(),
                location: Some(location.clone()),
            },
            &context(),
        )
        .await
        .unwrap()
        .record;
    assert_eq!(saved.location.as_ref(), Some(&location));
    proxies
        .record_test(&saved.id, saved.revision, success(), &context())
        .await
        .unwrap();
    for id in ["acct_location_a", "acct_location_b"] {
        let mut input = account(id, id);
        input.outbound_proxy = Some(saved.proxy.clone());
        accounts.insert_provider_account(input).await.unwrap();
        let projected = accounts
            .get_account(&ProviderAccountId::new(id).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(projected.request_location(), Some(&location));
    }
    let directory = accounts.list_accounts().await.unwrap();
    assert_eq!(directory.len(), 2);
    assert!(
        directory
            .iter()
            .all(|account| account.request_location() == Some(&location))
    );
    let original_revision = directory[0].revision();
    saved = proxies
        .update(
            UpdateProxy {
                auto_location: None,
                test: None,
                id: saved.id.clone(),
                revision: saved.revision,
                name: "Renamed".to_owned(),
                proxy: None,
                location: None,
            },
            &context(),
        )
        .await
        .unwrap()
        .record;
    assert_eq!(saved.location.as_ref(), Some(&location));
    assert!(saved.last_test.as_ref().unwrap().success);
    let stale_revision = saved.revision;
    saved = proxies
        .update(
            UpdateProxy {
                auto_location: None,
                test: None,
                id: saved.id.clone(),
                revision: saved.revision,
                name: saved.name.clone(),
                proxy: None,
                location: Some(None),
            },
            &context(),
        )
        .await
        .unwrap()
        .record;
    assert!(saved.location.is_none());
    assert!(saved.last_test.as_ref().unwrap().success);
    assert!(
        accounts
            .list_accounts()
            .await
            .unwrap()
            .iter()
            .all(|account| account.request_location().is_none()
                && account.revision() == original_revision)
    );
    assert!(
        proxies
            .update(
                UpdateProxy {
                    auto_location: None,
                    test: None,
                    id: saved.id.clone(),
                    revision: stale_revision,
                    name: saved.name.clone(),
                    proxy: None,
                    location: Some(Some(location.clone())),
                },
                &context()
            )
            .await
            .is_err()
    );
    assert!(proxies.get(&saved.id).await.unwrap().location.is_none());
    proxies
        .update(
            UpdateProxy {
                auto_location: None,
                test: None,
                id: saved.id.clone(),
                revision: saved.revision,
                name: saved.name.clone(),
                proxy: None,
                location: Some(Some(location.clone())),
            },
            &context(),
        )
        .await
        .unwrap();
    let id = ProviderAccountId::new("acct_location_a").unwrap();
    proxies
        .remove_account(&saved.id, &id, &context())
        .await
        .unwrap();
    let removed = accounts.get_account(&id).await.unwrap().unwrap();
    assert!(removed.outbound_proxy().is_none());
    assert!(removed.request_location().is_none());
    let other = accounts
        .get_account(&ProviderAccountId::new("acct_location_b").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other.request_location(), Some(&location));
    database.close().await;
}

fn context() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "managed-proxy-test".to_owned(),
    }
}

fn success() -> ProxyTestResult {
    ProxyTestResult {
        location: Default::default(),
        success: true,
        latency_ms: 10,
        exit_ip: Some("203.0.113.5".parse().unwrap()),
        exit_ipv4: Some("203.0.113.5".parse().unwrap()),
        exit_ipv6: None,
        message: "Connected".to_owned(),
    }
}

fn update(account_id: &str, selection: AccountProxySelection) -> UpdateAccount {
    UpdateAccount {
        notes: None,
        model_access: Default::default(),
        account_id: account_id.to_owned(),
        enabled: true,
        concurrency_limit: None,
        weight: AccountWeight::DEFAULT,
        group_ids: vec![],
        outbound_proxy: Some(selection),
    }
}

#[tokio::test]
async fn proxy_account_removal_preserves_settings_and_rejects_changed_bindings() {
    let Some(database) = TestDatabase::create("proxy_account_remove").await else {
        return;
    };
    let store = PgProxyRepository::new(database.pool.clone());
    let context = context();
    let saved = store
        .create(
            NewProxy {
                auto_location: false,
                test: None,
                location: None,
                name: "解绑测试".to_owned(),
                proxy: OutboundProxy::parse("http://user:secret@127.0.0.1:17890").unwrap(),
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    sqlx::query(
        "insert into provider_accounts
         (id, provider_kind, name, email, authentication_kind, provider_credentials_json,
          has_refresh_token, enabled, concurrency_limit, weight, plan_type,
          credential_observed_at, created_at, updated_at, outbound_proxy_id, outbound_proxy_url)
         values ('acct_remove', 'openai', '解绑账号', 'remove@example.invalid', 'oauth',
          '{\"access_token\":\"preserve-test-secret\"}'::jsonb, false, false, 3, 7, 'plus',
          now(), now(), now(), $1, $2)",
    )
    .bind(&saved.id)
    .bind(saved.proxy.expose_url())
    .execute(&database.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into account_groups (id, name, color, enabled, created_at, updated_at)
         values ('grp_00000000000000000000000000000001', '保留分组', '#123456FF', true, now(), now())",
    ).execute(&database.pool).await.unwrap();
    sqlx::query(
        "insert into account_group_accounts (account_group_id, provider_account_id, created_at)
         values ('grp_00000000000000000000000000000001', 'acct_remove', now())",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    let account_id = ProviderAccountId::new("acct_remove").unwrap();
    let snapshot = "select to_jsonb(a) - 'updated_at' - 'outbound_proxy_id' - 'outbound_proxy_url'
        from provider_accounts a where id = 'acct_remove'";
    let before: serde_json::Value = sqlx::query_scalar(snapshot)
        .fetch_one(&database.pool)
        .await
        .unwrap();
    let revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id = 1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    let audit_count: i64 = sqlx::query_scalar("select count(*) from admin_audit_events")
        .fetch_one(&database.pool)
        .await
        .unwrap();

    assert_eq!(
        store
            .remove_account("proxy_previous", &account_id, &context)
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::Conflict
    );
    assert_eq!(store.get(&saved.id).await.unwrap().account_count, 1);
    let unchanged: (Option<String>, Option<String>) = sqlx::query_as(
        "select outbound_proxy_id, outbound_proxy_url from provider_accounts where id = 'acct_remove'",
    ).fetch_one(&database.pool).await.unwrap();
    assert_eq!(
        unchanged,
        (
            Some(saved.id.clone()),
            Some(saved.proxy.expose_url().to_owned())
        )
    );
    let current_revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id = 1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(current_revision, revision);

    let committed = store
        .remove_account(&saved.id, &account_id, &context)
        .await
        .unwrap();
    assert_eq!(committed.get(), u64::try_from(revision + 1).unwrap());
    let after: serde_json::Value = sqlx::query_scalar(snapshot)
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(after, before);
    let direct: (Option<String>, Option<String>) = sqlx::query_as(
        "select outbound_proxy_id, outbound_proxy_url from provider_accounts where id = 'acct_remove'",
    ).fetch_one(&database.pool).await.unwrap();
    assert_eq!(direct, (None, None));
    let group_count: i64 = sqlx::query_scalar(
        "select count(*) from account_group_accounts where provider_account_id = 'acct_remove'",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(group_count, 1);
    assert_eq!(store.get(&saved.id).await.unwrap().account_count, 0);
    assert_eq!(
        store
            .list_accounts(ProxyAccountListQuery {
                proxy_id: saved.id.clone(),
                page: 1,
                page_size: PageSize::new(20).unwrap(),
                search: String::new(),
            })
            .await
            .unwrap()
            .total,
        0
    );

    assert_eq!(
        store
            .remove_account(&saved.id, &account_id, &context)
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::Conflict
    );
    let current_audits: i64 = sqlx::query_scalar("select count(*) from admin_audit_events")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(current_audits, audit_count + 1);
    let current_revision: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id = 1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(u64::try_from(current_revision).unwrap(), committed.get());
    database.close().await;
}

#[tokio::test]
async fn proxy_accounts_paginate_thousands_of_accounts_and_search_without_loading_the_catalog() {
    let Some(database) = TestDatabase::create("proxy_account_pagination").await else {
        return;
    };
    let store = PgProxyRepository::new(database.pool.clone());
    let context = context();
    let saved = store
        .create(
            NewProxy {
                auto_location: false,
                test: None,
                location: None,
                name: "分页测试".to_owned(),
                proxy: OutboundProxy::parse("http://127.0.0.1:17890").unwrap(),
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    let empty = store
        .create(
            NewProxy {
                auto_location: false,
                test: None,
                location: None,
                name: "无关联账号".to_owned(),
                proxy: OutboundProxy::parse("http://127.0.0.1:17891").unwrap(),
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    // 相同名称验证稳定排序；账号仅写入隔离测试数据库，不接触真实账号池
    sqlx::query(
        "insert into provider_accounts
        (id, provider_kind, name, email, authentication_kind, provider_credentials_json,
         has_refresh_token, enabled, credential_observed_at, created_at, updated_at,
         outbound_proxy_id, outbound_proxy_url)
        select 'acct_page_' || lpad(n::text, 4, '0'),
            case when n % 2 = 0 then 'xai' else 'openai' end, '共享账号',
            'page_' || lpad(n::text, 4, '0') || '@example.invalid', 'oauth',
            '{\"access_token\":\"page-test-secret\"}'::jsonb, false, n % 2 = 0,
            now(), now(), now(), $1, $2
        from generate_series(1, 1005) as n",
    )
    .bind(&saved.id)
    .bind(saved.proxy.expose_url())
    .execute(&database.pool)
    .await
    .unwrap();
    sqlx::query("update provider_accounts set plan_type = 'plus' where id = 'acct_page_0001'")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into account_groups (id, name, color, enabled, created_at, updated_at)
         values ('grp_00000000000000000000000000000001', '工作组', '#123456FF', false, now(), now()),
                ('grp_00000000000000000000000000000002', '次页分组', '#654321FF', true, now(), now())",
    ).execute(&database.pool).await.unwrap();
    sqlx::query(
        "insert into account_group_accounts (account_group_id, provider_account_id, created_at)
         values ('grp_00000000000000000000000000000001', 'acct_page_0001', now()),
                ('grp_00000000000000000000000000000002', 'acct_page_0021', now())",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    PgProviderAccountRepository::new(database.pool.clone())
        .insert_provider_account(account("acct_unrelated", "unrelated-user"))
        .await
        .unwrap();
    let query = |page, search: &str| ProxyAccountListQuery {
        proxy_id: saved.id.clone(),
        page,
        page_size: PageSize::new(20).unwrap(),
        search: search.to_owned(),
    };
    let first = store.list_accounts(query(1, "")).await.unwrap();
    assert_eq!((first.total, first.items.len()), (1005, 20));
    assert_eq!(first.items[0].id, "acct_page_0001");
    assert_eq!(
        first.items[0].email.as_deref(),
        Some("page_0001@example.invalid")
    );
    assert_eq!(first.items[0].provider_kind, "openai");
    assert_eq!(first.items[0].authentication_kind, "oauth");
    assert_eq!(first.items[0].plan_type.as_deref(), Some("plus"));
    assert_eq!(first.items[0].groups.len(), 1);
    assert_eq!(first.items[0].groups[0].name, "工作组");
    assert_eq!(first.items[0].groups[0].color.as_str(), "#123456FF");
    assert!(!first.items[0].groups[0].enabled);
    assert!(first.items[1].groups.is_empty());
    assert!(!first.items[0].enabled);
    let second = store.list_accounts(query(2, "")).await.unwrap();
    assert_eq!(second.items[0].id, "acct_page_0021");
    assert_eq!(second.items[0].groups[0].name, "次页分组");
    assert!(
        first
            .items
            .iter()
            .all(|left| second.items.iter().all(|right| left.id != right.id))
    );
    let last = store.list_accounts(query(51, "")).await.unwrap();
    assert_eq!((last.total, last.items.len()), (1005, 5));
    assert_eq!(last.items[4].id, "acct_page_1005");
    let filtered = store.list_accounts(query(1, "PAGE_004")).await.unwrap();
    assert_eq!((filtered.total, filtered.items.len()), (10, 10));
    assert_eq!(filtered.items[0].id, "acct_page_0040");
    assert_eq!(store.list_accounts(query(1, "%")).await.unwrap().total, 0);
    assert!(
        store
            .list_accounts(query(52, ""))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        store
            .list_accounts(ProxyAccountListQuery {
                proxy_id: empty.id,
                ..query(1, "")
            })
            .await
            .unwrap()
            .total,
        0
    );
    assert_eq!(
        store
            .list_accounts(ProxyAccountListQuery {
                proxy_id: "missing".to_owned(),
                ..query(1, "")
            })
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::NotFound
    );
    let catalog = store
        .list(ProxyListQuery {
            page: 1,
            page_size: PageSize::new(20).unwrap(),
            search: "分页测试".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(catalog.items[0].account_count, 1005);
    database.close().await;
}

#[tokio::test]
async fn rejected_import_reservations_release_proxy_lock_before_returning() {
    let Some(database) = TestDatabase::create("rejected_proxy_import").await else {
        return;
    };
    let store = PgProxyRepository::new(database.pool.clone());
    let id = "proxy_missing";

    // 读取不存在的代理失败后，应立即允许其他事务取得同一资源的独占锁
    for _ in 0..64 {
        let error = store.reserve_import(id).await.err().unwrap();
        assert_eq!(error.kind(), AdminStoreErrorKind::NotFound);
        let mut transaction = database.pool.begin().await.unwrap();
        let acquired: bool =
            sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended($1, 739219))")
                .bind(id)
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
        assert!(acquired);
        transaction.commit().await.unwrap();
    }
    database.close().await;
}

#[tokio::test]
async fn stale_proxy_test_releases_lock_before_next_update() {
    let Some(database) = TestDatabase::create("stale_proxy_test_lock").await else {
        return;
    };
    let store = PgProxyRepository::new(database.pool.clone());
    let context = context();
    let mut saved = store
        .create(
            NewProxy {
                name: "过期检测结果".to_owned(),
                proxy: OutboundProxy::parse("http://127.0.0.1:19090").unwrap(),
                auto_location: true,
                location: None,
                test: None,
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    let stale_revision = saved.revision;
    saved = store
        .record_test(&saved.id, saved.revision, success(), &context)
        .await
        .unwrap()
        .record;

    // 暂停连接归还时的后台清理，确保锁由返回错误前的显式回滚释放
    let release_gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
    let rejected_pool = database
        .pool
        .options()
        .clone()
        .max_connections(1)
        .after_release({
            let release_gate = release_gate.clone();
            move |_, _| {
                let release_gate = release_gate.clone();
                Box::pin(async move {
                    let _permit = release_gate.acquire().await.unwrap();
                    Ok(true)
                })
            }
        })
        .connect_with((*database.pool.connect_options()).clone())
        .await
        .unwrap();
    let stale_writer = PgProxyRepository::new(rejected_pool.clone());
    let error = stale_writer
        .record_test(&saved.id, stale_revision, success(), &context)
        .await
        .unwrap_err();
    let update = store
        .update(
            UpdateProxy {
                id: saved.id.clone(),
                revision: saved.revision,
                name: saved.name.clone(),
                proxy: None,
                auto_location: None,
                location: None,
                test: None,
            },
            &context,
        )
        .await;
    release_gate.add_permits(1);
    rejected_pool.close().await;
    database.close().await;
    assert_eq!(error.kind(), AdminStoreErrorKind::Conflict);
    assert!(update.is_ok(), "过期检测结果不应阻挡后续编辑: {update:?}");
}

#[tokio::test]
async fn import_reservation_blocks_proxy_mutations_until_rotated_credentials_are_committed() {
    use gateway_store::postgres::{
        ImportProviderAccounts, ProviderAccountAdminRepository, ProviderAccountAdminScope,
    };
    let Some(database) = TestDatabase::create("proxy_import_reservation").await else {
        return;
    };
    let store = PgProxyRepository::new(database.pool.clone());
    let other_process = PgProxyRepository::new(database.pool.clone());
    let context = context();
    let saved = store
        .create(
            NewProxy {
                auto_location: false,
                test: None,
                location: None,
                name: "导入出口".to_owned(),
                proxy: OutboundProxy::parse("http://127.0.0.1:8080").unwrap(),
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    assert!(saved.last_test.is_none());
    store
        .record_test(
            &saved.id,
            saved.revision,
            ProxyTestResult {
                location: Default::default(),
                success: false,
                ..success()
            },
            &context,
        )
        .await
        .unwrap();
    let reservation = store.reserve_import(&saved.id).await.unwrap();
    let replacement = UpdateProxy {
        auto_location: None,
        test: None,
        location: None,
        id: saved.id.clone(),
        revision: saved.revision,
        name: saved.name,
        proxy: Some(OutboundProxy::parse("http://127.0.0.1:9090").unwrap()),
    };
    assert!(
        other_process
            .update(replacement.clone(), &context)
            .await
            .is_err()
    );
    assert!(
        other_process
            .delete(&saved.id, saved.revision, &context)
            .await
            .is_err()
    );
    assert!(
        other_process
            .record_test(
                &saved.id,
                saved.revision,
                ProxyTestResult {
                    location: Default::default(),
                    success: false,
                    ..success()
                },
                &context
            )
            .await
            .is_err()
    );

    // 模拟上游已轮换的新凭据；持有保护时，另一个连接仍能完成导入事务
    let mut candidate = account("acct_reserved_import", "reserved-import-user");
    candidate.outbound_proxy = Some(reservation.binding.proxy.clone());
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let audit =
        super::provider_accounts::audit("audit_reserved_import", "import", "acct_reserved_import");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        repository.import_provider_accounts(ImportProviderAccounts {
            settings: None,
            outbound_proxy: Some(reservation.binding.clone()),
            scope: ProviderAccountAdminScope {
                provider_kind: "openai".to_owned(),
            },
            accounts: vec![candidate],
            audit,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    drop(reservation);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if other_process
                .update(replacement.clone(), &context)
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        repository
            .load_provider_account("acct_reserved_import")
            .await
            .unwrap()
            .is_some()
    );
    database.close().await;
}

#[tokio::test]
async fn managed_proxies_persist_bind_update_all_accounts_and_protect_stale_tests() {
    let Some(database) = TestDatabase::create("managed_proxies").await else {
        return;
    };
    let accounts = PgProviderAccountRepository::new(database.pool.clone());
    for id in ["acct_one", "acct_two"] {
        accounts
            .insert_provider_account(account(id, id))
            .await
            .unwrap();
    }
    let store = PgProxyRepository::new(database.pool.clone());
    let admin = admin_account_store(&database.pool);
    let context = context();
    let old_proxy = OutboundProxy::parse("http://user:secret@127.0.0.1:8080").unwrap();
    let created = store
        .create(
            NewProxy {
                auto_location: false,
                test: None,
                location: None,
                name: "Office".to_owned(),
                proxy: old_proxy.clone(),
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    assert_eq!(store.get(&created.id).await.unwrap().proxy, old_proxy);
    assert!(
        store
            .create(
                NewProxy {
                    auto_location: false,
                    test: None,
                    location: None,
                    name: "Duplicate".to_owned(),
                    proxy: old_proxy.clone()
                },
                &context
            )
            .await
            .is_err()
    );
    let selection = AccountProxySelection::Saved(created.id.clone());
    admin
        .update_account(update("acct_one", selection.clone()), &context)
        .await
        .unwrap();
    let read = || async {
        sqlx::query_as::<_, (String, Option<String>, Option<String>, i64)>("select id, outbound_proxy_id, outbound_proxy_url, credential_revision from provider_accounts order by id")
            .fetch_all(&database.pool).await.unwrap()
    };
    let untested = read().await;
    assert_eq!(untested[0].1.as_deref(), Some(created.id.as_str()));
    assert_eq!(untested[0].2.as_deref(), Some(old_proxy.expose_url()));
    assert!(untested[1].1.is_none() && untested[1].2.is_none());
    assert!(untested.iter().all(|row| row.3 == 1));
    let tested = store
        .record_test(
            &created.id,
            created.revision,
            ProxyTestResult {
                location: Default::default(),
                success: false,
                ..success()
            },
            &context,
        )
        .await
        .unwrap();
    assert!(tested.record.last_test_at.is_some());
    assert!(!tested.record.last_test.unwrap().success);
    for id in ["acct_one", "acct_two"] {
        admin
            .update_account(update(id, selection.clone()), &context)
            .await
            .unwrap();
    }
    assert_eq!(store.get(&created.id).await.unwrap().account_count, 2);
    assert_eq!(
        store
            .delete(&created.id, created.revision, &context)
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::Conflict
    );
    assert!(
        read()
            .await
            .iter()
            .all(|row| row.1.as_deref() == Some(created.id.as_str())
                && row.2.as_deref() == Some(old_proxy.expose_url())
                && row.3 == 1)
    );

    store
        .record_test(&created.id, created.revision, success(), &context)
        .await
        .unwrap();
    let renamed = store
        .update(
            UpdateProxy {
                auto_location: None,
                test: None,
                id: created.id.clone(),
                revision: created.revision,
                location: None,
                name: "Renamed".to_owned(),
                proxy: None,
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    assert_eq!(renamed.proxy, old_proxy);
    assert_eq!(renamed.last_test, Some(success()));
    assert!(read().await.iter().all(|row| row.3 == 1));
    let new_proxy = OutboundProxy::parse("socks5h://next:new-secret@127.0.0.1:1080").unwrap();
    let edited = store
        .update(
            UpdateProxy {
                auto_location: None,
                test: None,
                id: created.id.clone(),
                revision: renamed.revision,
                location: None,
                name: renamed.name,
                proxy: Some(new_proxy.clone()),
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    assert!(edited.last_test.is_none());
    assert!(edited.last_test_at.is_none());
    assert!(
        read()
            .await
            .iter()
            .all(|row| row.2.as_deref() == Some(new_proxy.expose_url()) && row.3 == 1)
    );
    assert!(
        store
            .record_test(&created.id, created.revision, success(), &context)
            .await
            .is_err()
    );
    assert!(store.get(&created.id).await.unwrap().last_test.is_none());
    store
        .record_test(&created.id, edited.revision, success(), &context)
        .await
        .unwrap();

    for id in ["acct_one", "acct_two"] {
        admin
            .update_account(update(id, AccountProxySelection::Direct), &context)
            .await
            .unwrap();
    }
    assert!(
        read()
            .await
            .iter()
            .all(|row| row.1.is_none() && row.2.is_none() && row.3 == 1)
    );
    store
        .delete(&created.id, edited.revision, &context)
        .await
        .unwrap();
    assert!(store.get(&created.id).await.is_err());
    let audits: Vec<serde_json::Value> =
        sqlx::query_scalar("select to_jsonb(a) from admin_audit_events a")
            .fetch_all(&database.pool)
            .await
            .unwrap();
    assert!(!serde_json::to_string(&audits).unwrap().contains("secret"));
    database.close().await;
}

#[tokio::test]
async fn legacy_urls_join_one_catalog_entry_and_invalid_batch_rolls_back() {
    let Some(database) = TestDatabase::create("proxy_catalog_legacy").await else {
        return;
    };
    let accounts = PgProviderAccountRepository::new(database.pool.clone());
    let proxy = OutboundProxy::parse("http://user:secret@127.0.0.1:8080").unwrap();
    for id in ["acct_one", "acct_two"] {
        let mut seed = account(id, id);
        seed.outbound_proxy = Some(proxy.clone());
        accounts.insert_provider_account(seed).await.unwrap();
    }
    let store = PgProxyRepository::new(database.pool.clone());
    let page = store
        .list(ProxyListQuery {
            page: 1,
            page_size: PageSize::new(20).unwrap(),
            search: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].account_count, 2);
    let admin = admin_account_store(&database.pool);
    assert!(
        admin
            .batch_update_accounts(
                BatchUpdateAccounts {
                    model_access: Default::default(),
                    account_ids: vec!["acct_one".to_owned(), "acct_missing".to_owned()],
                    enabled: Some(false),
                    concurrency_limit: Some(None),
                    weight: Some(AccountWeight::DEFAULT),
                    group_ids: Some(vec![]),
                    outbound_proxy: Some(AccountProxySelection::Url(
                        OutboundProxy::parse("http://127.0.0.1:9090").unwrap()
                    )),
                },
                &context()
            )
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from outbound_proxies")
            .fetch_one(&database.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_as::<_, (bool, String)>(
            "select enabled, outbound_proxy_url from provider_accounts where id = 'acct_one'"
        )
        .fetch_one(&database.pool)
        .await
        .unwrap(),
        (true, proxy.expose_url().to_owned())
    );
    database.close().await;
}

#[tokio::test]
async fn migration_backfills_shared_proxies_without_changing_credentials() {
    let Some(database) = TestDatabase::create_through("proxy_backfill", 4).await else {
        return;
    };
    // 从真实旧 schema 正向升级，不拆卸最新结构，避免遗漏后续增加的引用约束
    for id in ["acct_one", "acct_two", "acct_direct"] {
        let proxy_url = (id != "acct_direct").then_some("http://user:secret@127.0.0.1:8080/");
        sqlx::query(
            "insert into provider_accounts (
                id,provider_kind,name,authentication_kind,provider_credentials_json,
                has_refresh_token,credential_observed_at,created_at,updated_at,outbound_proxy_url
             ) values ($1,'openai',$1,'oauth','{}',false,now(),now(),now(),$2)",
        )
        .bind(id)
        .bind(proxy_url)
        .execute(&database.pool)
        .await
        .unwrap();
    }
    super::TEST_MIGRATOR.run(&database.pool).await.unwrap();
    let accounts = PgProviderAccountRepository::new(database.pool.clone());
    let inherited = gateway_core::account::ProviderAccountStore::list_accounts(&accounts)
        .await
        .unwrap();
    assert_eq!(inherited.len(), 3);
    assert!(
        inherited
            .iter()
            .all(|account| account.request_location().is_none())
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from outbound_proxies")
            .fetch_one(&database.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(sqlx::query_scalar::<_, i64>("select count(*) from provider_accounts a join outbound_proxies p on a.outbound_proxy_id = p.id where a.outbound_proxy_url = p.proxy_url and a.credential_revision = 1").fetch_one(&database.pool).await.unwrap(), 2);
    assert!(
        sqlx::query_scalar::<_, Option<String>>(
            "select outbound_proxy_id from provider_accounts where id = 'acct_direct'"
        )
        .fetch_one(&database.pool)
        .await
        .unwrap()
        .is_none()
    );
    database.close().await;
}

#[tokio::test]
async fn automatic_proxy_location_projects_to_accounts_and_guards_refresh_results() {
    use gateway_core::account::{ProviderAccountStore, RequestLocation};
    let Some(database) = TestDatabase::create("auto_proxy_location").await else {
        return;
    };
    let proxies = PgProxyRepository::new(database.pool.clone());
    let accounts = PgProviderAccountRepository::new(database.pool.clone());
    let manual = RequestLocation::default();
    let detected: RequestLocation = serde_json::from_value(serde_json::json!({"country":"JP", "region":"Tokyo", "city":"Tokyo", "timezone":"Asia/Tokyo"})).unwrap();
    let detection = ProxyTestResult {
        location: ProxyLocationDetection::Detected {
            location: detected.clone(),
        },
        ..success()
    };
    let created = proxies
        .create(
            NewProxy {
                name: "Auto".to_owned(),
                proxy: OutboundProxy::parse("http://127.0.0.1:19090").unwrap(),
                location: Some(manual.clone()),
                auto_location: true,
                test: Some(detection.clone()),
            },
            &context(),
        )
        .await
        .unwrap();
    let mut saved = created.record;
    assert_eq!(saved.effective_location(), Some(&detected));
    assert_eq!(saved.location, Some(manual.clone()));
    for id in ["acct_auto_a", "acct_auto_b"] {
        let mut input = account(id, id);
        input.outbound_proxy = Some(saved.proxy.clone());
        accounts.insert_provider_account(input).await.unwrap();
    }
    let initial_accounts = accounts.list_accounts().await.unwrap();
    let credential_revision = initial_accounts[0].revision();
    assert!(
        initial_accounts
            .iter()
            .all(|account| account.request_location() == Some(&detected))
    );
    let single = accounts
        .get_account(&ProviderAccountId::new("acct_auto_a").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(single.request_location(), Some(&detected));
    let original_detection = saved.detected_location.clone();
    let failed = ProxyTestResult {
        location: ProxyLocationDetection::Failed {
            message: "查询失败".to_owned(),
        },
        ..success()
    };
    saved = proxies
        .record_test(&saved.id, saved.revision, failed.clone(), &context())
        .await
        .unwrap()
        .record;
    assert_eq!(saved.detected_location, original_detection);
    let stale = saved.revision;
    let changed_ip = ProxyTestResult {
        exit_ip: Some("203.0.113.77".parse().unwrap()),
        exit_ipv4: Some("203.0.113.77".parse().unwrap()),
        ..failed
    };
    let before: i64 =
        sqlx::query_scalar("select config_revision from runtime_settings where id = 1")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    let changed = proxies
        .record_test(&saved.id, saved.revision, changed_ip, &context())
        .await
        .unwrap();
    assert!(changed.config_revision.get() > before as u64);
    saved = changed.record;
    assert!(saved.detected_location.is_none());
    assert!(
        accounts
            .list_accounts()
            .await
            .unwrap()
            .iter()
            .all(|account| account.request_location().is_none())
    );
    assert_eq!(
        proxies
            .record_test(&saved.id, stale, detection.clone(), &context())
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::Conflict
    );
    saved = proxies
        .record_test(&saved.id, saved.revision, detection.clone(), &context())
        .await
        .unwrap()
        .record;
    let conflict = ProxyTestResult {
        location: ProxyLocationDetection::Conflict,
        ..success()
    };
    saved = proxies
        .record_test(&saved.id, saved.revision, conflict, &context())
        .await
        .unwrap()
        .record;
    assert!(saved.effective_location().is_none());
    saved = proxies
        .record_test(&saved.id, saved.revision, detection, &context())
        .await
        .unwrap()
        .record;
    saved = proxies
        .update(
            UpdateProxy {
                id: saved.id.clone(),
                revision: saved.revision,
                name: saved.name.clone(),
                proxy: None,
                auto_location: None,
                location: None,
                test: None,
            },
            &context(),
        )
        .await
        .unwrap()
        .record;
    assert_eq!(saved.effective_location(), Some(&detected));
    let prior_revision = saved.revision;
    saved = proxies
        .update(
            UpdateProxy {
                id: saved.id.clone(),
                revision: saved.revision,
                name: saved.name.clone(),
                proxy: Some(OutboundProxy::parse("http://127.0.0.1:19091").unwrap()),
                auto_location: None,
                location: None,
                test: None,
            },
            &context(),
        )
        .await
        .unwrap()
        .record;
    assert!(saved.effective_location().is_none());
    assert!(saved.last_test.is_none());
    assert_eq!(
        proxies
            .record_test(&saved.id, prior_revision, success(), &context())
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::Conflict
    );
    saved = proxies
        .update(
            UpdateProxy {
                id: saved.id.clone(),
                revision: saved.revision,
                name: saved.name.clone(),
                proxy: None,
                auto_location: Some(false),
                location: None,
                test: None,
            },
            &context(),
        )
        .await
        .unwrap()
        .record;
    assert_eq!(saved.effective_location(), Some(&manual));
    assert!(
        accounts
            .list_accounts()
            .await
            .unwrap()
            .iter()
            .all(|account| account.request_location() == Some(&manual)
                && account.revision() == credential_revision)
    );
    database.close().await;
}
