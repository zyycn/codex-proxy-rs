//! Core 账号列表的冻结范围与实时启停过滤

use super::*;

#[tokio::test]
async fn scoped_account_list_empty_scope_does_not_use_database() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://test:test@localhost/test")
        .expect("lazy database pool");
    pool.close().await;
    let repository = PgProviderAccountRepository::new(pool);
    let provider = ProviderKind::new("openai").expect("provider kind");

    assert!(
        repository
            .list_for_provider_accounts(&provider, &[])
            .await
            .expect("empty scope without a database connection")
            .is_empty()
    );
}

#[tokio::test]
async fn scoped_account_list_filters_ids_provider_and_live_enabled_state() {
    let Some(database) = TestDatabase::create("scoped_account_list").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let first = account("acct_scope_a", "scope-a");
    let last = account("acct_scope_z", "scope-z");
    let outside = account("acct_scope_outside", "scope-outside");
    let mut disabled = account("acct_scope_disabled", "scope-disabled");
    disabled.enabled = false;
    let mut other_provider = account("acct_scope_other_provider", "scope-other-provider");
    other_provider.provider_kind = "xai".to_owned();
    for fixture in [first, last, outside, disabled, other_provider] {
        repository
            .insert_provider_account(fixture)
            .await
            .expect("seed scoped account list");
    }
    let provider = ProviderKind::new("openai").expect("provider kind");
    let account_ids = [
        "acct_scope_z",
        "acct_scope_disabled",
        "acct_scope_other_provider",
        "acct_scope_z",
        "acct_scope_missing",
        "acct_scope_a",
    ]
    .map(|id| ProviderAccountId::new(id).expect("account id"));

    let scoped = repository
        .list_for_provider_accounts(&provider, &account_ids)
        .await
        .expect("load scoped account list");
    assert_eq!(
        scoped
            .iter()
            .map(|account| account.id().as_str())
            .collect::<Vec<_>>(),
        ["acct_scope_a", "acct_scope_z"]
    );
    let full_pool = repository
        .list_for_provider(&provider)
        .await
        .expect("load full provider pool");
    assert_eq!(
        full_pool
            .iter()
            .map(|account| account.id().as_str())
            .collect::<Vec<_>>(),
        ["acct_scope_a", "acct_scope_outside", "acct_scope_z"]
    );
    let all = repository
        .list_provider_accounts(None, true)
        .await
        .expect("load administrative account list");
    assert_eq!(all.len(), 5);
    assert!(all.iter().any(|account| !account.enabled));
    assert!(all.iter().any(|account| account.provider_kind == "xai"));

    let disabled_id = ProviderAccountId::new("acct_scope_disabled").expect("disabled account id");
    let enabled_id = ProviderAccountId::new("acct_scope_a").expect("enabled account id");
    repository
        .set_enabled(&enabled_id, false)
        .await
        .expect("disable an account between scoped reads");
    repository
        .set_enabled(&disabled_id, true)
        .await
        .expect("enable an account between scoped reads");
    let changed = repository
        .list_for_provider_accounts(&provider, &account_ids)
        .await
        .expect("reload scoped account list after enabled state changes");
    assert_eq!(
        changed
            .iter()
            .map(|account| account.id().as_str())
            .collect::<Vec<_>>(),
        ["acct_scope_disabled", "acct_scope_z"]
    );
    database.close().await;
}

#[tokio::test]
async fn bulk_catalog_restore_keeps_ten_thousand_owners_and_fences_late_writes() {
    use gateway_core::account::{AccountCatalogObservation, CredentialRevision};
    let Some(database) = TestDatabase::create("account_catalog_bulk").await else {
        return;
    };
    let repo = PgProviderAccountRepository::new(database.pool.clone());
    sqlx::query("insert into provider_accounts (id,provider_kind,name,authentication_kind,provider_credentials_json,has_refresh_token,credential_state,credential_observed_at,created_at,updated_at,plan_type)
        select 'acct_'||i,'openai','source '||i,'oauth','{}',false,'ready',now(),now(),now(),'pro' from generate_series(1,10000) i").execute(&database.pool).await.unwrap();
    let observation = AccountCatalogObservation {
        account_id: ProviderAccountId::new("acct_1").unwrap(),
        revision: CredentialRevision::new(1).unwrap(),
        observed_at: std::time::SystemTime::now(),
        etag: Some("v1".into()),
        plan: Some("pro".into()),
    };
    let document = vec![b'x'; 64 * 1024];
    assert!(
        repo.save_account_catalog(&observation, &document)
            .await
            .unwrap()
    );
    sqlx::query("insert into provider_account_model_catalogs (account_id,credential_revision,plan_type,observed_at,document)
        select a.id,1,'pro',now(),c.document from provider_accounts a cross join provider_account_model_catalogs c where a.id<>'acct_1'").execute(&database.pool).await.unwrap();
    let provider = ProviderKind::new("openai").unwrap();
    let start = std::time::Instant::now();
    let group = repo.load_account_catalogs(&provider).await.unwrap();
    eprintln!(
        "catalog bulk restore: 10000 accounts, {} documents, {} ms",
        group.len(),
        start.elapsed().as_millis()
    );
    assert_eq!(group.len(), 1);
    assert_eq!(group[0].accounts.len(), 10000);
    assert_eq!(&group[0].document[..], &document);
    let older = AccountCatalogObservation {
        observed_at: observation.observed_at - std::time::Duration::from_secs(1),
        ..observation.clone()
    };
    assert!(!repo.save_account_catalog(&older, b"late").await.unwrap());
    sqlx::query("update provider_accounts set credential_revision=2 where id='acct_1'")
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(
        !repo
            .save_account_catalog(&observation, b"old revision")
            .await
            .unwrap()
    );
    sqlx::query("update provider_accounts set enabled=false where id='acct_2'")
        .execute(&database.pool)
        .await
        .unwrap();
    sqlx::query("delete from provider_accounts where id='acct_3'")
        .execute(&database.pool)
        .await
        .unwrap();
    let group = repo.load_account_catalogs(&provider).await.unwrap();
    assert_eq!(group[0].accounts.len(), 9997);
    assert!(
        group[0]
            .accounts
            .iter()
            .all(|a| !matches!(a.account_id.as_str(), "acct_1" | "acct_2" | "acct_3"))
    );
    // 数据库只能排除有当前版本、未过期的账号证据；显式例外仍进入选号
    let hash: Vec<u8> = sqlx::query_scalar("select sha256($1::bytea)")
        .bind(&document)
        .fetch_one(&database.pool)
        .await
        .unwrap();
    let now = std::time::SystemTime::now();
    sqlx::query("update provider_account_model_catalogs set observed_at=now()-interval '1 hour' where account_id='acct_4'").execute(&database.pool).await.unwrap();
    let filter = gateway_core::account::AccountCatalogOmissions {
        documents: vec![hash.try_into().unwrap()],
        exceptions: vec![ProviderAccountId::new("acct_5").unwrap()],
        observed_after: now - std::time::Duration::from_secs(1800),
        as_of: now,
    };
    let ids = (1..=10000)
        .map(|n| ProviderAccountId::new(format!("acct_{n}")).unwrap())
        .collect::<Vec<_>>();
    let candidates = repo
        .list_catalog_candidates(&provider, &ids, &filter)
        .await
        .unwrap();
    let ids = candidates
        .iter()
        .map(|a| a.id().as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        ids,
        std::collections::BTreeSet::from(["acct_1", "acct_4", "acct_5"])
    );
    database.close().await;
}
