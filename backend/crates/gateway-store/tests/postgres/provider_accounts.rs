use std::time::{Duration, SystemTime};

use chrono::{TimeDelta, Utc};
use gateway_admin::{
    model::{
        MutationActor, MutationContext, PageSize, Revision as AdminRevision,
        accounts::{
            AccountListQuery, AccountSort, AccountSortField, AccountStatus, DeleteAccounts,
            SetAccountEnabled as AdminSetAccountEnabled, SortDirection,
        },
        observability::TimeRange,
        provider_credentials::{
            CredentialAvailabilityFilter, CredentialListQuery, CredentialListWindow,
        },
    },
    ports::store::{AccountStore, AdminStoreErrorKind},
};
use gateway_core::engine::credential::{
    CredentialCasOutcome, CredentialCasUpdate, CredentialRevision, OpaqueProviderData,
    PlaintextCredential, ProviderAccountId, ProviderAccountStore, ProviderAccountUpdate,
    QuotaObservation, QuotaWriteOutcome,
};
use gateway_core::routing::ProviderKind;
use gateway_store::{
    ConflictKind, JsonObject, Revision, StoreError,
    postgres::{
        AdminAuditActorKind, AdminAuditEvent, DeleteProviderAccounts, ImportProviderAccounts,
        NewProviderAccount, PgAdminAccountStore, PgProviderAccountRepository,
        ProviderAccountAdminRepository, ProviderAccountAdminScope, ProviderAccountAvailability,
        ProviderAccountObservation, ProviderAccountRepository, ProviderCredentialUpdate,
        RotateProviderAccount, SetProviderAccountEnabled, UpdateProviderAccount,
    },
};
use serde_json::json;

use super::TestDatabase;

#[test]
fn postgres_provider_account_adapter_implements_core_port() {
    fn assert_port<T: ProviderAccountStore>() {}
    assert_port::<PgProviderAccountRepository>();

    fn assert_admin_port<T: ProviderAccountAdminRepository>() {}
    assert_admin_port::<PgProviderAccountRepository>();

    fn assert_terminal_admin_port<T: AccountStore>() {}
    assert_terminal_admin_port::<PgAdminAccountStore>();
}

#[tokio::test]
async fn core_quota_batch_reads_only_observed_accounts_in_one_contract_call() {
    let Some(database) = TestDatabase::create("provider_account_quota_batch").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    for id in ["acct_quota_a", "acct_quota_b", "acct_quota_empty"] {
        repository
            .insert_provider_account(account(id, &format!("user-{id}")))
            .await
            .expect("insert quota fixture");
    }
    let revision = CredentialRevision::new(1).expect("revision");
    for (id, remaining) in [("acct_quota_a", 20), ("acct_quota_b", 80)] {
        let outcome = repository
            .compare_and_swap_quota(QuotaObservation {
                account_id: ProviderAccountId::new(id).expect("account id"),
                expected_revision: revision,
                quota: Some(OpaqueProviderData::new(
                    json!({"remaining": remaining})
                        .as_object()
                        .expect("quota object")
                        .clone(),
                )),
                observed_at: Some(SystemTime::now()),
            })
            .await
            .expect("persist quota");
        assert_eq!(outcome, QuotaWriteOutcome::Updated);
    }

    let mut observations = repository
        .get_quotas(&[
            ProviderAccountId::new("acct_quota_b").expect("account id"),
            ProviderAccountId::new("acct_quota_empty").expect("account id"),
            ProviderAccountId::new("acct_quota_a").expect("account id"),
        ])
        .await
        .expect("read quota batch");
    observations.sort_by(|left, right| left.account_id.cmp(&right.account_id));

    assert_eq!(observations.len(), 2);
    assert_eq!(observations[0].account_id.as_str(), "acct_quota_a");
    assert_eq!(observations[0].expected_revision, revision);
    assert_eq!(
        observations[0]
            .quota
            .as_ref()
            .expect("quota")
            .expose_to_provider()["remaining"],
        20
    );
    assert!(observations.iter().all(|value| value.observed_at.is_some()));
    assert_eq!(
        current_revision(&database.pool).await,
        1,
        "quota observation is runtime state, not a global configuration mutation"
    );

    database.close().await;
}

#[tokio::test]
async fn terminal_admin_list_filters_and_sorts_before_pagination_with_retained_usage() {
    let Some(database) = TestDatabase::create("provider_account_terminal_list").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let now = Utc::now();

    let mut alpha = account("acct_alpha", "user-alpha");
    alpha.email = Some("alpha@example.invalid".to_owned());
    let mut beta = account("acct_beta", "user-beta");
    beta.provider_kind = "xai".to_owned();
    beta.email = Some("beta@example.invalid".to_owned());
    beta.availability = ProviderAccountAvailability::Banned;
    let mut charlie = account("acct_charlie", "user-charlie");
    charlie.email = Some("charlie@example.invalid".to_owned());
    let mut attention = account("acct_attention", "user-attention");
    attention.email = Some("attention@example.invalid".to_owned());
    attention.availability = ProviderAccountAvailability::Invalid;
    for account in [alpha, beta, charlie, attention] {
        repository
            .insert_provider_account(account)
            .await
            .expect("insert account list fixture");
    }

    seed_model_request(
        &database.pool,
        ModelRequestSeed {
            request_id: "req_alpha_recent",
            account_id: "acct_alpha",
            provider_kind: "openai",
            model: "gpt-list",
            total_tokens: 10,
            cost_amount: "0.10",
            started_at: now - TimeDelta::minutes(20),
        },
    )
    .await
    .expect("seed alpha usage");
    seed_model_request(
        &database.pool,
        ModelRequestSeed {
            request_id: "req_beta_recent",
            account_id: "acct_beta",
            provider_kind: "xai",
            model: "grok-list",
            total_tokens: 50,
            cost_amount: "0.50",
            started_at: now - TimeDelta::minutes(5),
        },
    )
    .await
    .expect("seed beta usage");
    seed_model_request(
        &database.pool,
        ModelRequestSeed {
            request_id: "req_beta_expired_retention",
            account_id: "acct_beta",
            provider_kind: "xai",
            model: "grok-list",
            total_tokens: 500,
            cost_amount: "5.00",
            started_at: now - TimeDelta::days(40),
        },
    )
    .await
    .expect("seed expired beta usage");
    seed_model_request(
        &database.pool,
        ModelRequestSeed {
            request_id: "req_charlie_recent",
            account_id: "acct_charlie",
            provider_kind: "openai",
            model: "gpt-list",
            total_tokens: 60,
            cost_amount: "0.60",
            started_at: now - TimeDelta::minutes(10),
        },
    )
    .await
    .expect("seed charlie usage");

    let store = PgAdminAccountStore::new(database.pool.clone());
    let usage_page = store
        .list_accounts(AccountListQuery {
            page: 1,
            page_size: PageSize::new(2).expect("page size"),
            provider_kind: None,
            search: None,
            status: None,
            sort: Some(AccountSort {
                field: AccountSortField::Usage,
                direction: SortDirection::Desc,
            }),
        })
        .await
        .expect("sort accounts by retained usage");
    assert_eq!(usage_page.config_revision.get(), 1);
    assert_eq!(usage_page.total, 4);
    assert_eq!(usage_page.summary.total, 4);
    assert_eq!(usage_page.summary.active, 2);
    assert_eq!(usage_page.summary.quota_exhausted, 0);
    assert_eq!(usage_page.summary.attention, 2);
    assert_eq!(
        usage_page
            .items
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        ["acct_charlie", "acct_beta"]
    );

    let last_used_page = store
        .list_accounts(AccountListQuery {
            page: 1,
            page_size: PageSize::new(2).expect("page size"),
            provider_kind: None,
            search: None,
            status: None,
            sort: Some(AccountSort {
                field: AccountSortField::LastUsedAt,
                direction: SortDirection::Desc,
            }),
        })
        .await
        .expect("sort accounts by retained last use");
    assert_eq!(
        last_used_page
            .items
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        ["acct_beta", "acct_charlie"]
    );

    let filtered = store
        .list_accounts(AccountListQuery {
            page: 1,
            page_size: PageSize::new(10).expect("page size"),
            provider_kind: Some(ProviderKind::new("openai").expect("Provider kind")),
            search: Some("ALPHA@EXAMPLE".to_owned()),
            status: Some(AccountStatus::Active),
            sort: None,
        })
        .await
        .expect("filter account directory");
    assert_eq!(filtered.total, 1);
    assert_eq!(filtered.summary, usage_page.summary);
    assert_eq!(filtered.items[0].id, "acct_alpha");
    assert_eq!(filtered.items[0].provider_kind.as_str(), "openai");

    let banned = store
        .list_accounts(AccountListQuery {
            page: 1,
            page_size: PageSize::new(10).expect("page size"),
            provider_kind: None,
            search: None,
            status: Some(AccountStatus::Banned),
            sort: None,
        })
        .await
        .expect("filter banned accounts");
    assert_eq!(banned.items[0].id, "acct_beta");
    let attention = store
        .list_accounts(AccountListQuery {
            page: 1,
            page_size: PageSize::new(10).expect("page size"),
            provider_kind: None,
            search: None,
            status: Some(AccountStatus::Attention),
            sort: None,
        })
        .await
        .expect("filter attention accounts");
    assert_eq!(attention.items[0].id, "acct_attention");

    database.close().await;
}

#[tokio::test]
async fn terminal_credential_list_preserves_grouped_filters_and_unpaged_collections() {
    let Some(database) = TestDatabase::create("provider_credential_terminal_windows").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    for index in 0..205_u16 {
        let mut credential = account(
            &format!("acct_xai_{index:03}"),
            &format!("user-xai-{index:03}"),
        );
        credential.provider_kind = "xai".to_owned();
        credential.availability = match index {
            0 => ProviderAccountAvailability::Expired,
            1 => ProviderAccountAvailability::Banned,
            2 => ProviderAccountAvailability::Invalid,
            _ => ProviderAccountAvailability::Ready,
        };
        repository
            .insert_provider_account(credential)
            .await
            .expect("insert xAI credential fixture");
    }

    let store = PgAdminAccountStore::new(database.pool.clone());
    let provider = ProviderKind::new("xai").expect("xAI Provider kind");
    let complete = store
        .list_credentials(
            &provider,
            CredentialListQuery {
                availability: None,
                enabled: None,
                window: CredentialListWindow::All,
            },
        )
        .await
        .expect("unpaged credential collection");
    assert_eq!(complete.items.len(), 205);
    assert!(complete.next_cursor.is_none());

    let page = store
        .list_credentials(
            &provider,
            CredentialListQuery {
                availability: None,
                enabled: None,
                window: CredentialListWindow::Page {
                    cursor: None,
                    page_size: PageSize::new(200).expect("page size"),
                },
            },
        )
        .await
        .expect("paged credential collection");
    assert_eq!(page.items.len(), 200);
    assert!(page.next_cursor.is_some());

    let invalid_group = store
        .list_credentials(
            &provider,
            CredentialListQuery {
                availability: Some(CredentialAvailabilityFilter::AnyOf(vec![
                    gateway_admin::model::accounts::AccountAvailability::Expired,
                    gateway_admin::model::accounts::AccountAvailability::Banned,
                    gateway_admin::model::accounts::AccountAvailability::Invalid,
                ])),
                enabled: None,
                window: CredentialListWindow::All,
            },
        )
        .await
        .expect("grouped invalid credential filter");
    assert_eq!(invalid_group.items.len(), 3);

    database.close().await;
}

#[tokio::test]
async fn terminal_admin_usage_chunks_large_selections_and_preserves_exact_costs() {
    let Some(database) = TestDatabase::create("provider_account_terminal_usage").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    repository
        .insert_provider_account(account("acct_usage_exact", "user-usage-exact"))
        .await
        .expect("insert exact usage account");
    let now = Utc::now();
    seed_model_request(
        &database.pool,
        ModelRequestSeed {
            request_id: "req_usage_exact",
            account_id: "acct_usage_exact",
            provider_kind: "openai",
            model: "gpt-exact",
            total_tokens: 18,
            cost_amount: "1.2345678901",
            started_at: now - TimeDelta::minutes(1),
        },
    )
    .await
    .expect("seed exact usage request");
    let mut account_ids = vec!["acct_usage_exact".to_owned()];
    account_ids.extend((0..200).map(|index| format!("missing_account_{index}")));

    let usage = PgAdminAccountStore::new(database.pool.clone())
        .load_account_usage(
            TimeRange {
                start: now - TimeDelta::hours(1),
                end: now + TimeDelta::hours(1),
            },
            &account_ids,
        )
        .await
        .expect("load account usage in bounded chunks");
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].account_id, "acct_usage_exact");
    assert_eq!(usage[0].request_count, 1);
    assert_eq!(usage[0].success_count, 1);
    assert_eq!(usage[0].total_tokens, Some(18));
    assert_eq!(usage[0].costs[0].currency, "USD");
    assert_eq!(usage[0].costs[0].amount.as_str(), "1.2345678901");
    assert_eq!(usage[0].request_buckets.len(), 2);
    assert_eq!(
        usage[0]
            .request_buckets
            .iter()
            .map(|bucket| bucket.request_count)
            .sum::<u64>(),
        1,
    );
    assert_eq!(usage[0].models.len(), 1);
    assert_eq!(usage[0].models[0].model, "gpt-exact");
    assert_eq!(usage[0].models[0].costs[0].amount.as_str(), "1.2345678901");

    database.close().await;
}

#[tokio::test]
async fn terminal_admin_mutations_keep_revision_account_and_audit_atomic() {
    let Some(database) = TestDatabase::create("provider_account_terminal_mutation").await else {
        return;
    };
    PgProviderAccountRepository::new(database.pool.clone())
        .insert_provider_account(account("acct_terminal_mutation", "user-terminal-mutation"))
        .await
        .expect("insert mutation account");
    let store = PgAdminAccountStore::new(database.pool.clone());
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "request_terminal_mutation".to_owned(),
    };

    let revision = store
        .set_account_enabled(
            AdminSetAccountEnabled {
                expected_config_revision: AdminRevision::new(1).expect("initial revision"),
                account_id: "acct_terminal_mutation".to_owned(),
                enabled: false,
            },
            &context,
        )
        .await
        .expect("disable account atomically");
    assert_eq!(revision.get(), 2);
    let enabled: bool = sqlx::query_scalar(
        "select enabled from provider_accounts where id = 'acct_terminal_mutation'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("read disabled state");
    assert!(!enabled);

    let error = store
        .delete_accounts(
            DeleteAccounts {
                expected_config_revision: AdminRevision::new(1).expect("stale revision"),
                account_ids: vec!["acct_terminal_mutation".to_owned()],
            },
            &context,
        )
        .await
        .expect_err("stale delete must roll back");
    assert_eq!(error.kind(), AdminStoreErrorKind::StaleRevision);
    assert_eq!(current_revision(&database.pool).await, 2);
    assert_eq!(
        account_count(&database.pool, "acct_terminal_mutation").await,
        1
    );
    let audit_count: i64 = sqlx::query_scalar("select count(*) from admin_audit_events")
        .fetch_one(&database.pool)
        .await
        .expect("count audit rows after rollback");
    assert_eq!(audit_count, 1);

    let revision = store
        .delete_accounts(
            DeleteAccounts {
                expected_config_revision: revision,
                account_ids: vec!["acct_terminal_mutation".to_owned()],
            },
            &context,
        )
        .await
        .expect("delete disabled account atomically");
    assert_eq!(revision.get(), 3);
    assert_eq!(
        account_count(&database.pool, "acct_terminal_mutation").await,
        0
    );
    let audit_rows: Vec<(String, i64, Vec<String>)> = sqlx::query_as(
        "select action, config_revision, changed_fields
         from admin_audit_events order by config_revision",
    )
    .fetch_all(&database.pool)
    .await
    .expect("load terminal account audits");
    assert_eq!(
        audit_rows,
        vec![
            ("disable".to_owned(), 2, vec!["enabled".to_owned()]),
            ("delete".to_owned(), 3, Vec::new()),
        ]
    );

    database.close().await;
}

#[tokio::test]
async fn terminal_admin_delete_removes_enabled_accounts_in_one_transaction() {
    let Some(database) = TestDatabase::create("provider_account_enabled_delete").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    for (account_id, upstream_user_id) in [
        ("acct_enabled_delete_a", "user-enabled-delete-a"),
        ("acct_enabled_delete_b", "user-enabled-delete-b"),
    ] {
        repository
            .insert_provider_account(account(account_id, upstream_user_id))
            .await
            .expect("insert enabled account");
    }

    let revision = PgAdminAccountStore::new(database.pool.clone())
        .delete_accounts(
            DeleteAccounts {
                expected_config_revision: AdminRevision::new(1).expect("initial revision"),
                account_ids: vec![
                    "acct_enabled_delete_a".to_owned(),
                    "acct_enabled_delete_b".to_owned(),
                ],
            },
            &MutationContext {
                actor: MutationActor::System,
                request_id: "request_enabled_delete".to_owned(),
            },
        )
        .await
        .expect("delete enabled account atomically");

    assert_eq!(revision.get(), 2);
    assert_eq!(
        account_count(&database.pool, "acct_enabled_delete_a").await
            + account_count(&database.pool, "acct_enabled_delete_b").await,
        0
    );
    database.close().await;
}

#[tokio::test]
async fn admin_import_updates_the_same_verified_identity_without_rebinding_it() {
    let Some(database) = TestDatabase::create("provider_account_admin_upsert").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let scope = ProviderAccountAdminScope {
        provider_kind: "openai".to_owned(),
    };
    let imported = repository
        .import_provider_accounts(
            Revision::new(1).expect("initial revision"),
            ImportProviderAccounts {
                scope: scope.clone(),
                accounts: vec![account("acct_admin_upsert", "user-admin-upsert")],
                audit: audit("audit_admin_upsert_create", "import", "acct_admin_upsert"),
            },
        )
        .await
        .expect("create imported account");
    assert_eq!(imported.account_ids, ["acct_admin_upsert"]);
    let revision = imported.config_revision;
    sqlx::query(
        "update provider_accounts
         set provider_quota_json = '{}'::jsonb, quota_observed_at = now(), updated_at = now()
         where id = 'acct_admin_upsert'",
    )
    .execute(&database.pool)
    .await
    .expect("seed stale quota");

    let mut updated = account("acct_admin_reimport", "user-admin-upsert");
    updated.name = "updated import".to_owned();
    updated.provider_credentials_json = credential_json("updated-import-secret");
    updated.availability = ProviderAccountAvailability::Banned;
    updated.availability_reason = Some("upstream_account_deactivated".to_owned());
    let imported = repository
        .import_provider_accounts(
            revision,
            ImportProviderAccounts {
                scope: scope.clone(),
                accounts: vec![updated],
                audit: audit("audit_admin_upsert_update", "import", "acct_admin_upsert"),
            },
        )
        .await
        .expect("update the same imported identity");
    assert_eq!(imported.account_ids, ["acct_admin_upsert"]);
    let revision = imported.config_revision;
    assert_eq!(revision.get(), 3);
    let row: (
        String,
        serde_json::Value,
        i64,
        Option<serde_json::Value>,
        String,
        Option<String>,
    ) = sqlx::query_as(
        "select name, provider_credentials_json, credential_revision, provider_quota_json,
                availability, availability_reason
         from provider_accounts where id = 'acct_admin_upsert'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load updated import");
    assert_eq!(row.0, "updated import");
    assert_eq!(row.1["access_token"], "updated-import-secret");
    assert_eq!(row.2, 2);
    assert!(
        row.3.is_none(),
        "credential replacement must clear stale quota"
    );
    assert_eq!(row.4, "banned");
    assert_eq!(row.5.as_deref(), Some("upstream_account_deactivated"));

    repository
        .import_provider_accounts(
            revision,
            ImportProviderAccounts {
                scope,
                accounts: vec![account("acct_admin_upsert", "another-user")],
                audit: audit("audit_admin_upsert_rebind", "import", "acct_admin_upsert"),
            },
        )
        .await
        .expect_err("an existing account ID must not be rebound");
    assert_eq!(current_revision(&database.pool).await, 3);

    database.close().await;
}

#[tokio::test]
async fn core_refresh_cas_updates_profile_and_credential_under_one_revision() {
    let Some(database) = TestDatabase::create("provider_account_core_refresh").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    repository
        .insert_provider_account(NewProviderAccount {
            id: "acct_core_refresh".to_owned(),
            provider_kind: "xai".to_owned(),
            name: "before refresh".to_owned(),
            email: Some("before@example.invalid".to_owned()),
            upstream_user_id: "upstream-core-refresh".to_owned(),
            upstream_account_id: None,
            plan_type: Some("free".to_owned()),
            authentication_kind: "oauth".to_owned(),
            provider_credentials_json: credential_json("before-secret"),
            has_refresh_token: false,
            access_token_expires_at: Some(Utc::now() + TimeDelta::minutes(5)),
            next_refresh_at: None,
            enabled: true,
            availability: ProviderAccountAvailability::Ready,
            availability_reason: None,
            cooldown_until: None,
            availability_observed_at: Utc::now(),
        })
        .await
        .expect("seed provider account");

    let account_id = ProviderAccountId::new("acct_core_refresh").expect("account ID");
    let refreshed = CredentialCasUpdate::new(
        account_id.clone(),
        CredentialRevision::new(1).expect("credential revision"),
        ProviderAccountUpdate {
            account_id: account_id.clone(),
            name: "after refresh".to_owned(),
            email: Some("after@example.invalid".to_owned()),
            plan_type: Some("premium".to_owned()),
        },
        plaintext_credential("after-secret"),
        true,
        Some(SystemTime::now() + Duration::from_secs(3_600)),
        Some(SystemTime::now() + Duration::from_secs(1_800)),
    )
    .expect("valid refresh update");
    assert_eq!(
        repository
            .compare_and_swap_credential(refreshed)
            .await
            .expect("refresh credential"),
        CredentialCasOutcome::Updated(
            CredentialRevision::new(2).expect("updated credential revision")
        )
    );

    let row: (
        String,
        Option<String>,
        Option<String>,
        serde_json::Value,
        i64,
    ) = sqlx::query_as(
        "select name, email, plan_type, provider_credentials_json, credential_revision
         from provider_accounts where id = 'acct_core_refresh'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load refreshed account");
    assert_eq!(row.0, "after refresh");
    assert_eq!(row.1.as_deref(), Some("after@example.invalid"));
    assert_eq!(row.2.as_deref(), Some("premium"));
    assert_eq!(row.3["access_token"], "after-secret");
    assert_eq!(row.4, 2);

    let stale = CredentialCasUpdate::new(
        account_id.clone(),
        CredentialRevision::new(1).expect("stale credential revision"),
        ProviderAccountUpdate {
            account_id,
            name: "must not persist".to_owned(),
            email: None,
            plan_type: None,
        },
        plaintext_credential("must-not-persist"),
        false,
        Some(SystemTime::now() + Duration::from_secs(3_600)),
        None,
    )
    .expect("valid stale update");
    assert_eq!(
        repository
            .compare_and_swap_credential(stale)
            .await
            .expect("stale CAS is an outcome"),
        CredentialCasOutcome::Conflict
    );
    let unchanged: (String, serde_json::Value, i64) = sqlx::query_as(
        "select name, provider_credentials_json, credential_revision
         from provider_accounts where id = 'acct_core_refresh'",
    )
    .fetch_one(&database.pool)
    .await
    .expect("load account after stale CAS");
    assert_eq!(unchanged.0, "after refresh");
    assert_eq!(unchanged.1["access_token"], "after-secret");
    assert_eq!(unchanged.2, 2);
    assert_eq!(
        current_revision(&database.pool).await,
        1,
        "credential refresh advances only credential_revision"
    );

    database.close().await;
}

#[tokio::test]
async fn provider_account_admin_mutations_are_scoped_audited_and_atomic() {
    let Some(database) = TestDatabase::create("provider_account_admin").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let scope = ProviderAccountAdminScope {
        provider_kind: "openai".to_owned(),
    };

    let mut cooldown_account = account("acct_admin_b", "user-admin-b");
    cooldown_account.availability = ProviderAccountAvailability::Cooldown;
    cooldown_account.cooldown_until = Some(Utc::now() + TimeDelta::minutes(10));
    let imported = repository
        .import_provider_accounts(
            Revision::new(1).expect("initial revision"),
            ImportProviderAccounts {
                scope: scope.clone(),
                accounts: vec![account("acct_admin_a", "user-admin-a"), cooldown_account],
                audit: audit("audit_account_batch", "import_batch", "provider_accounts"),
            },
        )
        .await
        .expect("import provider accounts");
    let revision = imported.config_revision;
    assert_eq!(revision.get(), 2);
    let ready: (bool, String, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "select enabled, availability, cooldown_until from provider_accounts where id = $1",
    )
    .bind("acct_admin_a")
    .fetch_one(&database.pool)
    .await
    .expect("load ready imported account");
    assert_eq!(ready, (true, "ready".to_owned(), None));
    let cooldown: (bool, String, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "select enabled, availability, cooldown_until from provider_accounts where id = $1",
    )
    .bind("acct_admin_b")
    .fetch_one(&database.pool)
    .await
    .expect("load cooldown imported account");
    assert!(cooldown.0);
    assert_eq!(cooldown.1, "cooldown");
    assert!(cooldown.2.is_some());

    repository
        .import_provider_accounts(
            revision,
            ImportProviderAccounts {
                scope: scope.clone(),
                accounts: vec![
                    account("acct_admin_transient", "user-admin-transient"),
                    account("acct_admin_a", "user-admin-duplicate"),
                ],
                audit: audit(
                    "audit_account_failed_batch",
                    "import_batch",
                    "provider_accounts",
                ),
            },
        )
        .await
        .expect_err("duplicate batch row must roll back the entire import");
    assert_eq!(current_revision(&database.pool).await, 2);
    assert_eq!(
        account_count(&database.pool, "acct_admin_transient").await,
        0
    );

    let wrong_scope = ProviderAccountAdminScope {
        provider_kind: "xai".to_owned(),
    };
    let wrong_scope_error = repository
        .rotate_provider_account(
            revision,
            RotateProviderAccount {
                scope: wrong_scope,
                profile: profile("acct_admin_a", "wrong scope"),
                credential: credential_update("acct_admin_a", 1, "wrong-scope-secret"),
                audit: audit("audit_wrong_scope", "rotate", "acct_admin_a"),
            },
        )
        .await
        .expect_err("Provider endpoint must not rotate another Provider account");
    assert!(matches!(
        wrong_scope_error,
        StoreError::Conflict {
            kind: ConflictKind::StaleRevision,
            ..
        }
    ));
    assert_eq!(current_revision(&database.pool).await, 2);

    let rotation = repository
        .rotate_provider_account(
            revision,
            RotateProviderAccount {
                scope: scope.clone(),
                profile: profile("acct_admin_a", "rotated account"),
                credential: credential_update("acct_admin_a", 1, "rotated-secret"),
                audit: audit("audit_account_rotate", "rotate", "acct_admin_a"),
            },
        )
        .await
        .expect("rotate provider account");
    assert_eq!(rotation.config_revision.get(), 3);
    assert_eq!(rotation.credential_revision.get(), 2);

    let revision = repository
        .set_provider_account_enabled_admin(
            rotation.config_revision,
            SetProviderAccountEnabled {
                scope: scope.clone(),
                account_id: "acct_admin_a".to_owned(),
                enabled: false,
                audit: audit("audit_account_disable", "disable", "acct_admin_a"),
            },
        )
        .await
        .expect("disable provider account");
    let exports = repository
        .export_provider_accounts(
            scope.clone(),
            vec!["acct_admin_b".to_owned(), "acct_admin_a".to_owned()],
        )
        .await
        .expect("export selected provider accounts");
    assert_eq!(exports[0].summary.id, "acct_admin_b");
    assert_eq!(exports[1].summary.id, "acct_admin_a");
    assert!(!format!("{exports:?}").contains("rotated-secret"));

    let revision = repository
        .delete_provider_accounts_admin(
            revision,
            DeleteProviderAccounts {
                scope,
                account_ids: vec!["acct_admin_a".to_owned(), "acct_admin_b".to_owned()],
                audit: audit("audit_account_delete", "delete", "provider_accounts"),
            },
        )
        .await
        .expect("delete selected accounts regardless of enabled state");
    assert_eq!(revision.get(), 5);
    assert_eq!(account_count(&database.pool, "acct_admin_a").await, 0);
    assert_eq!(account_count(&database.pool, "acct_admin_b").await, 0);
    let audit_count: i64 = sqlx::query_scalar("select count(*) from admin_audit_events")
        .fetch_one(&database.pool)
        .await
        .expect("count provider account audits");
    assert_eq!(audit_count, 4);

    database.close().await;
}

fn account(id: &str, upstream_user_id: &str) -> NewProviderAccount {
    NewProviderAccount {
        id: id.to_owned(),
        provider_kind: "openai".to_owned(),
        name: id.to_owned(),
        email: Some(format!("{id}@example.invalid")),
        upstream_user_id: upstream_user_id.to_owned(),
        upstream_account_id: None,
        plan_type: Some("pro".to_owned()),
        authentication_kind: "oauth".to_owned(),
        provider_credentials_json: credential_json("initial-secret"),
        has_refresh_token: false,
        access_token_expires_at: Some(Utc::now() + TimeDelta::hours(1)),
        next_refresh_at: None,
        enabled: true,
        availability: ProviderAccountAvailability::Ready,
        availability_reason: None,
        cooldown_until: None,
        availability_observed_at: Utc::now(),
    }
}

fn credential_update(account_id: &str, revision: u64, marker: &str) -> ProviderCredentialUpdate {
    ProviderCredentialUpdate {
        account_id: account_id.to_owned(),
        expected_revision: Revision::new(revision).expect("credential revision"),
        provider_credentials_json: credential_json(marker),
        has_refresh_token: false,
        access_token_expires_at: Some(Utc::now() + TimeDelta::hours(2)),
        next_refresh_at: None,
    }
}

fn profile(account_id: &str, name: &str) -> UpdateProviderAccount {
    UpdateProviderAccount {
        id: account_id.to_owned(),
        name: name.to_owned(),
        email: Some(format!("{account_id}@rotated.example.invalid")),
        plan_type: Some("team".to_owned()),
    }
}

fn credential_json(marker: &str) -> JsonObject {
    JsonObject::try_from_value(
        "provider_credentials_json",
        json!({ "access_token": marker }),
        256 * 1024,
    )
    .expect("credential JSON")
}

fn plaintext_credential(marker: &str) -> PlaintextCredential {
    PlaintextCredential::new(
        [("access_token".to_owned(), json!(marker))]
            .into_iter()
            .collect(),
    )
}

fn audit(id: &str, action: &str, entity_ref: &str) -> AdminAuditEvent {
    AdminAuditEvent {
        id: id.to_owned(),
        actor_kind: AdminAuditActorKind::System,
        actor_admin_user_id: None,
        actor_ref: "system:provider-test".to_owned(),
        admin_request_id: Some(format!("request:{id}")),
        action: action.to_owned(),
        entity_kind: "provider_account".to_owned(),
        entity_ref: entity_ref.to_owned(),
        config_revision: None,
        changed_fields: vec!["provider_account".to_owned()],
        created_at: Utc::now(),
    }
}

struct ModelRequestSeed<'a> {
    request_id: &'a str,
    account_id: &'a str,
    provider_kind: &'a str,
    model: &'a str,
    total_tokens: i64,
    cost_amount: &'a str,
    started_at: chrono::DateTime<Utc>,
}

async fn seed_model_request(
    pool: &sqlx::PgPool,
    seed: ModelRequestSeed<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id,
           provider_kind, provider_account_id,
           provider_account_ref, upstream_model_id, upstream_transport, attempt_count,
           upstream_send_state, outcome, client_status_code, upstream_status_code,
           input_tokens, output_tokens, cached_tokens, cache_write_tokens, reasoning_tokens,
           total_tokens, cost_source, cost_amount, cost_currency,
           started_at, deadline_at, completed_at
         ) values (
           $1, 'key-provider-account-test', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', $4, $3, $2, $2, $4, 'http_sse', 1,
           'sent', 'succeeded', 200, 200, $5, 0, 0, 0, 0,
           $5, 'provider_reported', $6::numeric, 'USD', $7,
           $7 + interval '5 minutes', $7 + interval '1 second'
         )",
    )
    .bind(seed.request_id)
    .bind(seed.account_id)
    .bind(seed.provider_kind)
    .bind(seed.model)
    .bind(seed.total_tokens)
    .bind(seed.cost_amount)
    .bind(seed.started_at)
    .execute(pool)
    .await?;
    Ok(())
}

async fn current_revision(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("select config_revision from runtime_settings where id = 1")
        .fetch_one(pool)
        .await
        .expect("load config revision")
}

async fn account_count(pool: &sqlx::PgPool, account_id: &str) -> i64 {
    sqlx::query_scalar("select count(*) from provider_accounts where id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
        .expect("count provider account")
}

#[test]
fn provider_credentials_are_redacted_from_debug() {
    let secret = "secret-access-token";
    let object =
        JsonObject::try_from_value("credentials", json!({ "access_token": secret }), 256 * 1024)
            .expect("object is valid");
    assert!(!format!("{object:?}").contains(secret));
}

#[test]
fn cooldown_observation_requires_expiry() {
    let observation = ProviderAccountObservation {
        account_id: "account-1".to_owned(),
        availability: ProviderAccountAvailability::Cooldown,
        availability_reason: None,
        cooldown_until: None,
        provider_quota_json: None,
        availability_observed_at: Utc::now(),
        quota_observed_at: None,
    };
    assert!(observation.validate().is_err());
}

#[test]
fn imported_account_rejects_mismatched_cooldown_runtime_facts() {
    let mut imported = account("acct_invalid_cooldown", "user-invalid-cooldown");
    imported.cooldown_until = Some(Utc::now() + TimeDelta::minutes(5));
    assert!(imported.validate().is_err());
}
