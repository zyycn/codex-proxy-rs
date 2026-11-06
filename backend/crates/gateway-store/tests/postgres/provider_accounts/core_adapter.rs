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
