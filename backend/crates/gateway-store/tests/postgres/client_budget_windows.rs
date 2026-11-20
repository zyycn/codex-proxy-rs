//! 显式窗口的事务、重试和结算边界

use super::{
    TestDatabase,
    client_budgets::{charge, context, key_id, seed, status},
};
use chrono::{Duration, Utc};
use gateway_admin::{
    model::client_keys::{
        ClientKeyBudgetMutationOrigin, ClientKeyBudgetPeriod, ClientKeyBudgetWindowPeriod,
        ExplicitClientKeyBudgetWindow, ResetClientKeyBudget, SetClientKeyBudgetWindow,
    },
    ports::store::{AdminStoreErrorKind, ClientKeyStore as _},
};
use gateway_core::engine::budget::ClientBudgetPort;
use gateway_store::postgres::{PgAdminClientKeyStore, PgClientBudgetStore};

fn window(
    revision: u64,
    start: chrono::DateTime<Utc>,
    end: chrono::DateTime<Utc>,
) -> SetClientKeyBudgetWindow {
    SetClientKeyBudgetWindow {
        id: key_id("key"),
        period: ClientKeyBudgetWindowPeriod::Weekly,
        expected_revision: revision,
        window: Some(ExplicitClientKeyBudgetWindow {
            resume_calendar: false,
            starts_at: start,
            ends_at: end,
        }),
    }
}

#[tokio::test]
async fn explicit_window_retries_preserve_spending_and_reject_obsolete_operations() {
    let Some(database) = TestDatabase::create("explicit_retries").await else {
        return;
    };
    seed(&database, "key", "0", "10").await;
    let admin = PgAdminClientKeyStore::new(database.pool.clone());
    let budgets = PgClientBudgetStore::new(database.pool.clone());
    budgets.settle(charge("key", "before", "2")).await.unwrap();
    let now = chrono::DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    let command = window(0, now - Duration::hours(1), now + Duration::hours(1));
    assert_eq!(
        admin
            .set_client_key_budget_window(
                command.clone(),
                ClientKeyBudgetMutationOrigin::Admin,
                &context()
            )
            .await
            .unwrap(),
        1
    );
    budgets.settle(charge("key", "after", "3")).await.unwrap();
    assert_eq!(
        admin
            .set_client_key_budget_window(
                command.clone(),
                ClientKeyBudgetMutationOrigin::Admin,
                &context()
            )
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        status(&database, "key").await.weekly_used_usd.canonical(),
        "5"
    );
    let mut changed = command.clone();
    changed.window.as_mut().unwrap().ends_at += Duration::hours(1);
    assert_eq!(
        admin
            .set_client_key_budget_window(changed, ClientKeyBudgetMutationOrigin::Admin, &context())
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::Conflict
    );
    let a = window(1, now - Duration::minutes(30), now + Duration::hours(2));
    let b = window(1, now - Duration::minutes(30), now + Duration::hours(3));
    let mutation_context = context();
    let (a, b) = tokio::join!(
        admin.set_client_key_budget_window(
            a,
            ClientKeyBudgetMutationOrigin::Admin,
            &mutation_context
        ),
        admin.set_client_key_budget_window(
            b,
            ClientKeyBudgetMutationOrigin::Admin,
            &mutation_context
        )
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(
        a.err().or(b.err()).unwrap().kind(),
        AdminStoreErrorKind::Conflict
    );
    assert_eq!(
        admin
            .set_client_key_budget_window(command, ClientKeyBudgetMutationOrigin::Admin, &context())
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::StaleRevision
    );
    let audits: i64 = sqlx::query_scalar(
        "select count(*) from admin_audit_events where action='set_budget_window'",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(audits, 2);
    database.close().await;
}

#[tokio::test]
async fn explicit_expiry_never_rolls_and_late_settlement_uses_completion_time() {
    let Some(database) = TestDatabase::create("explicit_expiry").await else {
        return;
    };
    seed(&database, "key", "0", "0").await;
    let admin = PgAdminClientKeyStore::new(database.pool.clone());
    let budgets = PgClientBudgetStore::new(database.pool.clone());
    let now = chrono::DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    let start = now - Duration::hours(2);
    let end = now - Duration::hours(1);
    admin
        .set_client_key_budget_window(
            window(0, start, end),
            ClientKeyBudgetMutationOrigin::Admin,
            &context(),
        )
        .await
        .unwrap();
    assert_eq!(
        budgets
            .admit(key_id("key"))
            .await
            .unwrap_err()
            .client_error_code(),
        Some("key_budget_window_inactive")
    );
    let mut old = charge("key", "old", "2");
    old.completed_at = (start + Duration::minutes(1)).into();
    budgets.settle(old.clone()).await.unwrap();
    budgets.settle(old).await.unwrap();
    let mut boundary = charge("key", "boundary", "3");
    boundary.completed_at = end.into();
    budgets.settle(boundary).await.unwrap();
    let before = admin
        .client_key_budget_window(&key_id("key"), ClientKeyBudgetWindowPeriod::Weekly)
        .await
        .unwrap();
    assert!(before.explicit);
    assert_eq!(before.ends_at, Some(end));
    assert_eq!(before.used_usd.canonical(), "2");
    assert_eq!(
        status(&database, "key").await.weekly_used_usd.canonical(),
        "2"
    );
    admin
        .set_client_key_budget_window(
            window(1, end, now + Duration::hours(1)),
            ClientKeyBudgetMutationOrigin::Admin,
            &context(),
        )
        .await
        .unwrap();
    assert_eq!(
        status(&database, "key").await.weekly_used_usd.canonical(),
        "3"
    );
    budgets.admit(key_id("key")).await.unwrap();
    let mut release = window(2, end, now + Duration::hours(1));
    release.window = None;
    admin
        .set_client_key_budget_window(release, ClientKeyBudgetMutationOrigin::Admin, &context())
        .await
        .unwrap();
    let released = admin
        .client_key_budget_window(&key_id("key"), ClientKeyBudgetWindowPeriod::Weekly)
        .await
        .unwrap();
    assert!(!released.explicit);
    assert_eq!(released.used_usd.canonical(), "3");
    assert_eq!(released.ends_at, Some(now + Duration::hours(1)));
    admin
        .set_client_key_budget_window(
            window(3, now + Duration::hours(1), now + Duration::hours(2)),
            ClientKeyBudgetMutationOrigin::Admin,
            &context(),
        )
        .await
        .unwrap();
    assert_eq!(
        budgets
            .admit(key_id("key"))
            .await
            .unwrap_err()
            .client_error_code(),
        Some("key_budget_window_inactive")
    );
    admin
        .set_client_key_budget_window(
            SetClientKeyBudgetWindow {
                id: key_id("key"),
                expected_revision: 4,
                period: ClientKeyBudgetWindowPeriod::Weekly,
                window: None,
            },
            ClientKeyBudgetMutationOrigin::Admin,
            &context(),
        )
        .await
        .unwrap();
    assert_eq!(
        budgets
            .admit(key_id("key"))
            .await
            .unwrap_err()
            .client_error_code(),
        Some("key_budget_window_inactive")
    );
    assert!(
        !status(&database, "key")
            .await
            .access_at(Utc::now().into())
            .allowed()
    );
    admin
        .reset_client_key_budget(
            ResetClientKeyBudget {
                id: key_id("key"),
                period: ClientKeyBudgetPeriod::Weekly,
            },
            ClientKeyBudgetMutationOrigin::Admin,
            &context(),
        )
        .await
        .unwrap();
    assert_eq!(
        status(&database, "key").await.weekly_used_usd.canonical(),
        "0"
    );
    database.close().await;
}

#[tokio::test]
async fn failed_window_mutation_rolls_back_receipt_and_manual_reset_invalidates_it() {
    let Some(database) = TestDatabase::create("explicit_atomic").await else {
        return;
    };
    seed(&database, "key", "0", "10").await;
    let admin = PgAdminClientKeyStore::new(database.pool.clone());
    let now = Utc::now();
    let command = window(0, now - Duration::hours(1), now + Duration::hours(1));
    sqlx::raw_sql("create function reject_window_audit() returns trigger language plpgsql as $$ begin raise exception 'test rollback'; end $$; create trigger reject_window_audit before insert on admin_audit_events for each row execute function reject_window_audit()")
        .execute(&database.pool).await.unwrap();
    assert!(
        admin
            .set_client_key_budget_window(
                command.clone(),
                ClientKeyBudgetMutationOrigin::Admin,
                &context()
            )
            .await
            .is_err()
    );
    assert_eq!(
        admin
            .client_key_budget_window(&key_id("key"), ClientKeyBudgetWindowPeriod::Weekly)
            .await
            .unwrap()
            .revision,
        0
    );
    sqlx::query("drop trigger reject_window_audit on admin_audit_events")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        admin
            .set_client_key_budget_window(
                command.clone(),
                ClientKeyBudgetMutationOrigin::Admin,
                &context()
            )
            .await
            .unwrap(),
        1
    );
    admin
        .reset_client_key_budget(
            ResetClientKeyBudget {
                id: key_id("key"),
                period: ClientKeyBudgetPeriod::Weekly,
            },
            ClientKeyBudgetMutationOrigin::Admin,
            &context(),
        )
        .await
        .unwrap();
    let current = admin
        .client_key_budget_window(&key_id("key"), ClientKeyBudgetWindowPeriod::Weekly)
        .await
        .unwrap();
    assert!(!current.explicit);
    assert_eq!(current.revision, 2);
    assert_eq!(
        admin
            .set_client_key_budget_window(command, ClientKeyBudgetMutationOrigin::Admin, &context())
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::StaleRevision
    );
    database.close().await;
}

#[tokio::test]
async fn explicit_window_admission_counts_pending_cost_once() {
    use gateway_store::postgres::BufferedClientBudgetStore;
    use std::sync::Arc;
    let Some(database) = TestDatabase::create("explicit_pending").await else {
        return;
    };
    seed(&database, "key", "0", "4").await;
    let admin = PgAdminClientKeyStore::new(database.pool.clone());
    let direct = PgClientBudgetStore::new(database.pool.clone());
    let (buffered, _writer) = BufferedClientBudgetStore::new(
        PgClientBudgetStore::new(database.pool.clone()),
        Arc::new(crate::RecordingDiagnostics::default()),
    );
    let now = Utc::now();
    direct
        .settle(charge("key", "before-window", "1"))
        .await
        .unwrap();
    admin
        .set_client_key_budget_window(
            window(0, now - Duration::hours(1), now + Duration::hours(1)),
            ClientKeyBudgetMutationOrigin::Admin,
            &context(),
        )
        .await
        .unwrap();
    let pending = charge("key", "pending", "2");
    buffered.settle(pending.clone()).await.unwrap();
    buffered.admit(key_id("key")).await.unwrap();
    direct.settle(pending).await.unwrap();
    buffered.admit(key_id("key")).await.unwrap();
    buffered
        .settle(charge("key", "exhausted", "2"))
        .await
        .unwrap();
    assert_eq!(
        buffered
            .admit(key_id("key"))
            .await
            .unwrap_err()
            .client_error_code(),
        Some("key_weekly_budget_exceeded")
    );
    database.close().await;
}

#[tokio::test]
async fn manual_alignment_counts_only_current_charges_and_returns_to_calendar_at_expiry() {
    let Some(db) = TestDatabase::create("manual_alignment").await else {
        return;
    };
    seed(&db, "key", "10", "20").await;
    let admin = PgAdminClientKeyStore::new(db.pool.clone());
    let budgets = PgClientBudgetStore::new(db.pool.clone());
    let now = chrono::DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    let mut outside = charge("key", "outside", "4");
    outside.completed_at = (now - Duration::days(8)).into();
    budgets.settle(outside).await.unwrap();
    budgets.settle(charge("key", "inside", "3")).await.unwrap();
    let before = status(&db, "key").await;
    let end = now + Duration::days(1);
    let mut command = window(0, end - Duration::days(7), end);
    command.window.as_mut().unwrap().resume_calendar = true;
    let mut expired = command.clone();
    expired.window.as_mut().unwrap().ends_at = now;
    let origin = || ClientKeyBudgetMutationOrigin::Admin;
    assert_eq!(
        admin
            .set_client_key_budget_window(expired, origin(), &context())
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::Conflict
    );
    admin
        .set_client_key_budget_window(command.clone(), origin(), &context())
        .await
        .unwrap();
    let after = status(&db, "key").await;
    assert_eq!(after.daily_used_usd, before.daily_used_usd);
    assert_eq!(after.limits, before.limits);
    assert_eq!(after.weekly_used_usd.canonical(), "3");
    let aligned = admin
        .client_key_budget_window(&key_id("key"), ClientKeyBudgetWindowPeriod::Weekly)
        .await
        .unwrap();
    assert_eq!(aligned.ends_at, Some(end));
    assert!(!aligned.explicit);
    budgets.settle(charge("key", "after", "2")).await.unwrap();
    assert_eq!(
        admin
            .set_client_key_budget_window(command, origin(), &context())
            .await
            .unwrap(),
        1
    );
    assert_eq!(status(&db, "key").await.weekly_used_usd.canonical(), "5");
    // 将同一账本推进至到期状态，验证下一次准入会续开原生窗口
    sqlx::query("update client_api_key_budget_windows set weekly_start = weekly_start - interval '2 days', weekly_end = weekly_end - interval '2 days' where client_api_key_id = 'key'").execute(&db.pool).await.unwrap();
    budgets.admit(key_id("key")).await.unwrap();
    let next = admin
        .client_key_budget_window(&key_id("key"), ClientKeyBudgetWindowPeriod::Weekly)
        .await
        .unwrap();
    assert!(next.ends_at.unwrap() > end);
    assert_eq!(next.used_usd.canonical(), "0");
    db.close().await;
}

#[tokio::test]
async fn reference_accounts_page_through_large_pools_and_search_only_key_groups() {
    let Some(database) = TestDatabase::create("budget_reference_accounts").await else {
        return;
    };
    seed(&database, "key", "0", "0").await;
    let key = key_id("key");
    let store = PgAdminClientKeyStore::new(database.pool.clone());
    sqlx::query(
        "insert into provider_accounts (id, name, email, provider_kind, authentication_kind,
            provider_credentials_json, has_refresh_token, credential_observed_at, created_at, updated_at)
         select 'ref-' || n, 'Account ' || n, 'person' || n || '@example.test', 'openai', 'oauth',
                '{}'::jsonb, false, now(), now(), now()
         from generate_series(1000, 2199) n",
    ).execute(&database.pool).await.unwrap();
    let first = store.budget_reference_accounts(&key, "", "").await.unwrap();
    assert_eq!(first.items.len(), 20);
    let next = store
        .budget_reference_accounts(&key, "", first.next_cursor.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(next.items[0].id, "ref-1020");
    let id_only = store
        .budget_reference_accounts(&key, "ref-2199", "")
        .await
        .unwrap();
    assert!(id_only.items.is_empty());
    for term in ["PERSON2199@", "account 2199"] {
        let page = store
            .budget_reference_accounts(&key, term, "")
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, "ref-2199");
        assert!(page.next_cursor.is_none());
    }
    let group = "grp_00000000000000000000000000000001";
    sqlx::query("insert into account_groups(id, name, color, created_at, updated_at) values ($1, 'Test', '#2563EBFF', now(), now())")
        .bind(group).execute(&database.pool).await.unwrap();
    sqlx::query("insert into client_api_key_groups(client_api_key_id, account_group_id, created_at) values ('key', $1, now())").bind(group).execute(&database.pool).await.unwrap();
    sqlx::query("insert into account_group_accounts(account_group_id, provider_account_id, created_at) values ($1, 'ref-2198', now())").bind(group).execute(&database.pool).await.unwrap();
    let scoped = store
        .budget_reference_accounts(&key, "219", "")
        .await
        .unwrap();
    assert_eq!(scoped.items.len(), 1);
    assert_eq!(scoped.items[0].id, "ref-2198");
    sqlx::query("update account_groups set enabled = false where id = $1")
        .bind(group)
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(
        store
            .budget_reference_accounts(&key, "", "")
            .await
            .unwrap()
            .items
            .is_empty()
    );
    database.close().await;
}
