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

fn context() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "managed-proxy-test".to_owned(),
    }
}

fn success() -> ProxyTestResult {
    ProxyTestResult {
        success: true,
        latency_ms: 10,
        exit_ip: Some("203.0.113.5".parse().unwrap()),
        message: "Connected".to_owned(),
    }
}

fn update(account_id: &str, selection: AccountProxySelection) -> UpdateAccount {
    UpdateAccount {
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
                name: "无关联账号".to_owned(),
                proxy: OutboundProxy::parse("http://127.0.0.1:17891").unwrap(),
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    // 相同名称验证稳定排序；账号仅写入隔离测试数据库，不接触真实账号池。
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
                name: "导入出口".to_owned(),
                proxy: OutboundProxy::parse("http://127.0.0.1:8080").unwrap(),
            },
            &context,
        )
        .await
        .unwrap()
        .record;
    assert!(store.reserve_import(&saved.id).await.is_err());
    store
        .record_test(&saved.id, saved.revision, success(), &context)
        .await
        .unwrap();
    let reservation = store.reserve_import(&saved.id).await.unwrap();
    let replacement = UpdateProxy {
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
                    success: false,
                    ..success()
                },
                &context
            )
            .await
            .is_err()
    );

    // 模拟上游已轮换的新凭据；持有保护时，另一个连接仍能完成导入事务。
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
                    name: "Duplicate".to_owned(),
                    proxy: old_proxy.clone()
                },
                &context
            )
            .await
            .is_err()
    );
    let selection = AccountProxySelection::Saved(created.id.clone());
    assert!(
        admin
            .update_account(update("acct_one", selection.clone()), &context)
            .await
            .is_err()
    );
    let read = || async {
        sqlx::query_as::<_, (String, Option<String>, Option<String>, i64)>("select id, outbound_proxy_id, outbound_proxy_url, credential_revision from provider_accounts order by id")
            .fetch_all(&database.pool).await.unwrap()
    };
    assert!(
        read()
            .await
            .iter()
            .all(|row| row.1.is_none() && row.2.is_none() && row.3 == 1)
    );
    let tested = store
        .record_test(&created.id, created.revision, success(), &context)
        .await
        .unwrap();
    assert!(tested.last_test_at.is_some());
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

    let renamed = store
        .update(
            UpdateProxy {
                id: created.id.clone(),
                revision: created.revision,
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
                id: created.id.clone(),
                revision: renamed.revision,
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
                    account_ids: vec!["acct_one".to_owned(), "acct_missing".to_owned()],
                    enabled: false,
                    concurrency_limit: None,
                    weight: AccountWeight::DEFAULT,
                    group_ids: vec![],
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
    let Some(database) = TestDatabase::create("proxy_backfill").await else {
        return;
    };
    let accounts = PgProviderAccountRepository::new(database.pool.clone());
    for id in ["acct_one", "acct_two", "acct_direct"] {
        accounts
            .insert_provider_account(account(id, id))
            .await
            .unwrap();
    }
    sqlx::raw_sql("alter table provider_accounts drop column outbound_proxy_id; drop table outbound_proxies;
        update provider_accounts set outbound_proxy_url = 'http://user:secret@127.0.0.1:8080/' where id <> 'acct_direct';")
        .execute(&database.pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../../migrations/0005_managed_outbound_proxies.sql"
    ))
    .execute(&database.pool)
    .await
    .unwrap();
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
