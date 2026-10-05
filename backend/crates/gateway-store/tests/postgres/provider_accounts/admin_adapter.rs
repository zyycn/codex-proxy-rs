//! 验证管理账号查询的容量投影、分页筛选与授权提交

use super::*;

#[tokio::test]
async fn account_capacity_should_resolve_limits_for_list_and_detail() {
    let Some(database) = TestDatabase::create("account_capacity").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let store = admin_account_store(&database.pool);
    let mut limited = account("acct_override", "user-override");
    limited.concurrency_limit = gateway_core::account::AccountConcurrencyLimit::new(3);
    for record in [account("acct_inherited", "user-inherited"), limited] {
        repository.insert_provider_account(record).await.unwrap();
    }
    for default_limit in [10_i64, 0] {
        sqlx::query("update runtime_settings set max_concurrent_per_account = $1 where id = 1")
            .bind(default_limit)
            .execute(&database.pool)
            .await
            .unwrap();
        for counts in [
            None,
            Some(BTreeMap::from([("acct_override".to_owned(), 2)])),
        ] {
            let runtime = AccountRuntimeSnapshot {
                in_flight: counts,
                ..Default::default()
            };
            let page = store
                .list_accounts(
                    AccountListQuery {
                        page: 1,
                        page_size: PageSize::new(20).unwrap(),
                        provider_kind: None,
                        group_filter: None,
                        search: None,
                        status: None,
                        sort: None,
                    },
                    runtime.clone(),
                )
                .await
                .unwrap();
            assert_eq!(page.items.len(), 2);
            for item in page.items {
                let inherited = item.account.id == "acct_inherited";
                let expected_limit = if inherited {
                    (default_limit > 0).then_some(10)
                } else {
                    Some(3)
                };
                assert_eq!(item.capacity.total_slots, expected_limit);
                assert_eq!(
                    item.capacity.used_slots,
                    runtime
                        .in_flight
                        .as_ref()
                        .map(|_| if inherited { 0 } else { 2 })
                );
                let detail = store
                    .load_account(&item.account.id, runtime.clone())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(detail.capacity, item.capacity);
            }
        }
    }
    database.close().await;
}

#[tokio::test]
async fn provider_filter_applies_before_pagination_even_on_an_empty_page() {
    let Some(database) = TestDatabase::create("account_provider_filters").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    for (index, provider) in ["openai", "xai", "xai"].into_iter().enumerate() {
        let mut record = account(
            &format!("acct_filter_{index}"),
            &format!("user-filter-{index}"),
        );
        record.provider_kind = provider.into();
        repository.insert_provider_account(record).await.unwrap();
    }
    let query = AccountListQuery {
        page: 2,
        page_size: PageSize::new(1).unwrap(),
        provider_kind: Some(ProviderKind::new("openai").unwrap()),
        group_filter: None,
        search: None,
        status: None,
        sort: None,
    };
    let store = admin_account_store(&database.pool);
    let page = store
        .list_accounts(query.clone(), Default::default())
        .await
        .unwrap();
    assert!(page.items.is_empty());
    let literal = store
        .list_accounts(
            AccountListQuery {
                page: 1,
                provider_kind: Some(ProviderKind::new("all").unwrap()),
                ..query.clone()
            },
            Default::default(),
        )
        .await
        .unwrap();
    assert!(literal.items.is_empty());
    assert_eq!(literal.total, 0);
    sqlx::query("delete from provider_accounts")
        .execute(&database.pool)
        .await
        .unwrap();
    let empty = store
        .list_accounts(query, Default::default())
        .await
        .unwrap();
    assert!(empty.items.is_empty());
    assert_eq!(empty.total, 0);
    database.close().await;
}

#[tokio::test]
async fn plugin_account_provider_filter_is_optional_and_applied_before_cursor_pagination() {
    let Some(database) = TestDatabase::create("plugin_account_scope").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    for id in ["acct_plugin_a", "acct_plugin_b", "acct_plugin_c"] {
        repository
            .insert_provider_account(account(id, &format!("user-{id}")))
            .await
            .unwrap();
    }
    let mut other_provider = account("acct_plugin_other", "user-other");
    other_provider.provider_kind = "xai".to_owned();
    repository
        .insert_provider_account(other_provider)
        .await
        .unwrap();
    let store = admin_account_store(&database.pool);
    let first = store
        .list_plugin_accounts(PluginAccountListQuery {
            provider_kind: Some(ProviderKind::new("openai").unwrap()),
            cursor: None,
            limit: PageSize::new(1).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(
        first
            .accounts
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        ["acct_plugin_a"]
    );
    assert_eq!(
        first.next_cursor.as_ref().map(ProviderAccountId::as_str),
        Some("acct_plugin_a")
    );

    let second = store
        .list_plugin_accounts(PluginAccountListQuery {
            provider_kind: Some(ProviderKind::new("openai").unwrap()),
            cursor: first.next_cursor,
            limit: PageSize::new(1).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(
        second
            .accounts
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        ["acct_plugin_b"]
    );
    assert_eq!(
        second.next_cursor.as_ref().map(ProviderAccountId::as_str),
        Some("acct_plugin_b")
    );

    // 未指定 Provider 时按全局账号 ID 跨 Provider 分页，Provider 归属来自持久记录
    let all = store
        .list_plugin_accounts(PluginAccountListQuery {
            provider_kind: None,
            cursor: None,
            limit: PageSize::new(16).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(
        all.accounts
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        [
            "acct_plugin_a",
            "acct_plugin_b",
            "acct_plugin_c",
            "acct_plugin_other"
        ]
    );
    assert!(all.next_cursor.is_none());

    database.close().await;
}

pub(super) fn credential(id: &str, user: &str) -> PreparedCredentialCreate {
    PreparedCredentialCreate {
        account_id: ProviderAccountId::new(id).unwrap(),
        provider_kind: ProviderKind::new("openai").unwrap(),
        name: "login account".into(),
        email: None,
        upstream_user_id: Some(user.into()),
        upstream_account_id: None,
        plan_type: None,
        authentication_kind: "oauth".into(),
        provider_material: ProviderDocument::new(OpaqueProviderData::new(
            json!({"access_token":"test-only"})
                .as_object()
                .unwrap()
                .clone(),
        )),
        has_refresh_token: false,
        access_token_expires_at: None,
        next_refresh_at: None,
        enabled: true,
        credential_state: CredentialState::Ready,
        credential_observed_at: Utc::now(),
        outbound_proxy: None,
        model_access: None,
    }
}

pub(super) fn command(
    credential: PreparedCredentialCreate,
) -> (AuthorizationCommit, MutationContext) {
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "authorization-account".into(),
    };
    (
        AuthorizationCommit {
            key: gateway_admin::model::provider_credentials::AuthorizationReceiptKey::new(
                ProviderKind::new("openai").unwrap(),
                &uuid::Uuid::new_v4().to_string(),
                &context,
            )
            .unwrap(),
            settings: None,
            pending: PendingAuthorizationMutation::new(
                ProviderKind::new("openai").unwrap(),
                AuthorizationMutationTarget::Create {
                    name: "login".into(),
                },
                AuthorizationOwnerBinding::from_context(&context),
            ),
            credential: AuthorizationCredentialCommit::Create(Box::new(credential)),
        },
        context,
    )
}

#[tokio::test]
async fn authorization_returns_authoritative_account_id_and_revision() {
    let Some(database) = TestDatabase::create("authorization_account").await else {
        return;
    };
    let store = admin_account_store(&database.pool);
    let (first, context) = command(credential("acct_existing", "user-one"));
    store.commit_authorization(first, &context).await.unwrap();
    let (authorization, context) = command(credential("acct_candidate", "user-one"));
    let result = store
        .commit_authorization(authorization, &context)
        .await
        .unwrap()
        .result;
    assert_eq!(result.account_id.as_str(), "acct_existing");
    assert_eq!(result.credential_revision.unwrap().get(), 2);
    assert_eq!(
        result.config_revision.get(),
        current_revision(&database.pool).await as u64
    );
    let accounts: i64 = sqlx::query_scalar("select count(*) from provider_accounts")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(accounts, 1);
    let audit: i64 =
        sqlx::query_scalar("select count(*) from admin_audit_events where action = 'authorize'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(audit, 2, "两次授权各写入一次审计");
    database.close().await;
}

#[tokio::test]
async fn authorization_conflict_preserves_accounts_revision_and_audit() {
    let Some(database) = TestDatabase::create("authorization_conflict").await else {
        return;
    };
    let store = admin_account_store(&database.pool);
    let (seed, context) = command(credential("acct_existing", "user-one"));
    store.commit_authorization(seed, &context).await.unwrap();
    let before = current_revision(&database.pool).await;
    let (authorization, context) = command(credential("acct_existing", "different-user"));
    assert!(
        store
            .commit_authorization(authorization, &context)
            .await
            .is_err()
    );
    let accounts: Vec<String> = sqlx::query_scalar("select id from provider_accounts order by id")
        .fetch_all(&database.pool)
        .await
        .unwrap();
    assert_eq!(accounts, ["acct_existing"]);
    assert_eq!(current_revision(&database.pool).await, before);
    let audit: i64 =
        sqlx::query_scalar("select count(*) from admin_audit_events where action = 'authorize'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(audit, 1);
    database.close().await;
}
