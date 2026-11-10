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
