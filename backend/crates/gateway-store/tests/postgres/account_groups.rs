use std::collections::BTreeMap;

use gateway_admin::{
    model::{
        MutationActor, MutationContext, PageSize,
        account_groups::{
            AccountGroupColor, AccountGroupListQuery, DeleteAccountGroup, NewAccountGroup,
        },
        client_keys::NewClientKey,
        client_keys::UpdateClientKey,
    },
    ports::store::{AccountGroupStore, AdminStoreErrorKind, ClientKeyStore},
};
use gateway_core::{
    policy::{ClientApiKeyId, RateLimits},
    routing::AccountGroupId,
};
use gateway_store::postgres::{PgAccountGroupRepository, PgAdminClientKeyStore};

use super::TestDatabase;

const MIXED_GROUP: &str = "grp_00000000000000000000000000000001";
const EMPTY_GROUP: &str = "grp_00000000000000000000000000000002";

#[tokio::test]
async fn groups_aggregate_cross_provider_members_and_key_bindings_without_multiplication() {
    let Some(database) = TestDatabase::create("account_group_aggregate").await else {
        return;
    };
    seed_account(
        &database.pool,
        "acct_group_openai",
        "openai",
        "OpenAI Account",
    )
    .await;
    seed_account(&database.pool, "acct_group_xai", "xai", "xAI Account").await;
    sqlx::query(
        "update provider_accounts set concurrency_limit = 4 where id = 'acct_group_openai'",
    )
    .execute(&database.pool)
    .await
    .expect("set account concurrency override");

    let groups = PgAccountGroupRepository::new(database.pool.clone());
    let keys = PgAdminClientKeyStore::new(database.pool.clone());
    let mixed_group = group_id(MIXED_GROUP);
    let empty_group = group_id(EMPTY_GROUP);
    groups
        .create_account_group(
            NewAccountGroup {
                id: mixed_group.clone(),
                name: "Mixed Production".to_owned(),
                description: Some("cross-provider".to_owned()),
                color: group_color("#2563EBFF"),
            },
            &context("create-mixed"),
        )
        .await
        .expect("create mixed account group");
    groups
        .create_account_group(
            NewAccountGroup {
                id: empty_group.clone(),
                name: "Empty Pool".to_owned(),
                description: None,
                color: group_color("#06B6D4CC"),
            },
            &context("create-empty"),
        )
        .await
        .expect("create empty account group");
    assign_accounts(
        &database.pool,
        MIXED_GROUP,
        &["acct_group_openai", "acct_group_xai"],
    )
    .await;

    for (id, group_ids) in [
        ("key_group_one", vec![mixed_group.clone()]),
        ("key_group_two", vec![mixed_group.clone()]),
        ("key_empty_pool", vec![empty_group.clone()]),
        ("key_all_accounts", Vec::new()),
    ] {
        keys.create_client_key(new_key(id, group_ids), &context(id))
            .await
            .expect("create scoped client key");
    }

    let page = groups
        .list_account_groups(AccountGroupListQuery {
            page: 1,
            page_size: PageSize::new(20).expect("page size"),
            search: None,
            enabled: None,
        })
        .await
        .expect("list account groups");
    assert_eq!(page.total, 2);
    let by_id = page
        .items
        .into_iter()
        .map(|group| (group.id.to_string(), group))
        .collect::<BTreeMap<_, _>>();
    let mixed = by_id.get(MIXED_GROUP).expect("mixed group");
    assert_eq!(mixed.member_count, 2);
    assert_eq!(
        mixed.provider_counts,
        BTreeMap::from([("openai".to_owned(), 1), ("xai".to_owned(), 1)])
    );
    assert_eq!(mixed.client_key_count, 2);
    assert_eq!(mixed.account_summary.available, 2);
    assert_eq!(mixed.account_summary.limited, 0);
    assert_eq!(mixed.account_summary.total, 2);
    assert_eq!(mixed.capacity.used_slots, None);
    assert_eq!(mixed.capacity.total_slots, 7);
    assert_eq!(mixed.usage.today_usd.as_str(), "0");
    assert_eq!(mixed.usage.total_usd.as_str(), "0");
    let empty = by_id.get(EMPTY_GROUP).expect("empty group");
    assert_eq!(empty.member_count, 0);
    assert!(empty.provider_counts.is_empty());
    assert_eq!(empty.client_key_count, 1);

    let all_key = keys
        .reveal_client_key(&client_key_id("key_all_accounts"))
        .await
        .expect("reveal all-accounts key")
        .expect("all-accounts key exists");
    assert!(all_key.record.groups.is_empty());
    assert_eq!(
        all_key
            .record
            .provider_kinds
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>(),
        ["openai", "xai"]
    );
    let empty_pool_key = keys
        .reveal_client_key(&client_key_id("key_empty_pool"))
        .await
        .expect("reveal empty-pool key")
        .expect("empty-pool key exists");
    assert_eq!(empty_pool_key.record.groups.len(), 1);
    assert!(empty_pool_key.record.provider_kinds.is_empty());

    let (scope_revision, widened) = keys
        .update_client_key(
            UpdateClientKey {
                id: client_key_id("key_group_one"),
                name: "key_group_one".to_owned(),
                label: None,
                group_ids: Vec::new(),
                limits: RateLimits::unlimited(),
            },
            &context("widen-group-key"),
        )
        .await
        .expect("widen restricted key to all accounts");
    assert!(widened.groups.is_empty());
    let scope_audit: Vec<String> = sqlx::query_scalar(
        "select changed_fields from admin_audit_events
         where admin_request_id = 'widen-group-key'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load scope widening audit");
    assert!(scope_audit.contains(&"routing_scope:groups->all".to_owned()));
    assert_eq!(current_revision(&database.pool).await, scope_revision.get());
    let (restricted_revision, restricted) = keys
        .update_client_key(
            UpdateClientKey {
                id: client_key_id("key_group_one"),
                name: "key_group_one".to_owned(),
                label: None,
                group_ids: vec![empty_group],
                limits: RateLimits::unlimited(),
            },
            &context("restrict-all-key"),
        )
        .await
        .expect("restrict all-accounts key to groups");
    assert_eq!(restricted.groups.len(), 1);
    let restricted_audit: Vec<String> = sqlx::query_scalar(
        "select changed_fields from admin_audit_events
         where admin_request_id = 'restrict-all-key'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load scope restriction audit");
    assert!(restricted_audit.contains(&"routing_scope:all->groups".to_owned()));
    assert_eq!(
        current_revision(&database.pool).await,
        restricted_revision.get()
    );

    let revision_before_delete = current_revision(&database.pool).await;
    let audit_before_delete = audit_count(&database.pool).await;
    let error = groups
        .delete_account_group(
            DeleteAccountGroup { id: mixed_group },
            &context("delete-referenced"),
        )
        .await
        .expect_err("referenced group must not be deleted");
    assert_eq!(error.kind(), AdminStoreErrorKind::Conflict);
    assert_eq!(
        current_revision(&database.pool).await,
        revision_before_delete
    );
    assert_eq!(audit_count(&database.pool).await, audit_before_delete);

    database.close().await;
}

fn new_key(id: &str, group_ids: Vec<AccountGroupId>) -> NewClientKey {
    let marker = char::from(id.as_bytes().last().copied().unwrap_or(b'k'));
    NewClientKey {
        id: client_key_id(id),
        name: id.to_owned(),
        label: None,
        group_ids,
        limits: RateLimits::unlimited(),
        plaintext: format!("sk_{}", marker.to_string().repeat(43)),
    }
}

fn group_id(value: &str) -> AccountGroupId {
    AccountGroupId::new(value).expect("valid account group ID")
}

fn group_color(value: &str) -> AccountGroupColor {
    AccountGroupColor::parse(value).expect("valid account group color")
}

fn client_key_id(value: &str) -> ClientApiKeyId {
    ClientApiKeyId::new(value).expect("valid client key ID")
}

fn context(request_id: &str) -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: request_id.to_owned(),
    }
}

async fn seed_account(pool: &sqlx::PgPool, id: &str, provider: &str, name: &str) {
    sqlx::query(
        "insert into provider_accounts (
           id, provider_kind, name, email, upstream_user_id, upstream_account_id,
           plan_type, authentication_kind, provider_credentials_json, credential_revision,
           has_refresh_token, access_token_expires_at, next_refresh_at, enabled,
           credential_state, credential_observed_at, created_at, updated_at
         ) values (
           $1, $2, $3, null, $1 || '-user', null, null, 'oauth', '{}'::jsonb, 1,
           false, null, null, true, 'ready', now(), now(), now()
         )",
    )
    .bind(id)
    .bind(provider)
    .bind(name)
    .execute(pool)
    .await
    .expect("seed provider account");
}

async fn assign_accounts(pool: &sqlx::PgPool, group_id: &str, account_ids: &[&str]) {
    for account_id in account_ids {
        sqlx::query(
            "insert into account_group_accounts (
               account_group_id, provider_account_id, created_at
             )
             values ($1, $2, now())",
        )
        .bind(group_id)
        .bind(account_id)
        .execute(pool)
        .await
        .expect("seed account group membership");
    }
}

async fn current_revision(pool: &sqlx::PgPool) -> u64 {
    let value =
        sqlx::query_scalar::<_, i64>("select config_revision from runtime_settings where id = 1")
            .fetch_one(pool)
            .await
            .expect("load config revision");
    u64::try_from(value).expect("positive config revision")
}

async fn audit_count(pool: &sqlx::PgPool) -> u64 {
    let value = sqlx::query_scalar::<_, i64>("select count(*) from admin_audit_events")
        .fetch_one(pool)
        .await
        .expect("count audit rows");
    u64::try_from(value).expect("non-negative audit count")
}
