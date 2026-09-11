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
use gateway_core::account::{AccountWeight, OutboundProxy};
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
    assert_eq!(store.get(&created.id).await.unwrap().accounts.len(), 2);
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
    assert_eq!(page.items[0].accounts.len(), 2);
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
