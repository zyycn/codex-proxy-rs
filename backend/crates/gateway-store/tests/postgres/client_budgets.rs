use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use gateway_admin::{
    model::{
        MutationActor, MutationContext,
        client_keys::{ReconcileClientCharge, UpdateClientKey},
    },
    ports::store::ClientKeyStore as _,
};
use gateway_core::{
    engine::{
        ModelRequestId,
        budget::{ClientBudgetAdmission, ClientBudgetCharge, ClientBudgetPort, ClientBudgetStatus},
    },
    error::GatewayErrorKind,
    policy::{ClientApiKeyId, RateLimits},
};
use gateway_store::postgres::{
    ClientApiKeyRepository as _, PgAdminClientKeyStore, PgClientApiKeyRepository,
    PgClientBudgetStore,
};

use super::TestDatabase;

fn admission(key: &str, request: &str) -> ClientBudgetAdmission {
    ClientBudgetAdmission {
        key_id: ClientApiKeyId::new(key).unwrap(),
        request_id: ModelRequestId::new(format!("req_{request}")).unwrap(),
        deadline_at: SystemTime::now() + Duration::from_secs(600),
    }
}

fn charge(request: &str, amount: Option<&str>) -> ClientBudgetCharge {
    ClientBudgetCharge {
        request_id: ModelRequestId::new(format!("req_{request}")).unwrap(),
        amount_usd: amount.map(|value| value.parse().unwrap()),
        completed_at: SystemTime::now(),
    }
}

async fn seed(database: &TestDatabase, key: &str, daily: &str, weekly: &str) {
    sqlx::query(
        "insert into client_api_keys (id, name, key, daily_limit_usd, weekly_limit_usd, created_at, updated_at)
        values ($1, $1, $2, $3::text::numeric, $4::text::numeric, now(), now())",
    )
    .bind(key)
    .bind(format!("sk_{key:a<43}"))
    .bind(daily)
    .bind(weekly)
    .execute(&database.pool)
    .await
    .unwrap();
}

async fn status(database: &TestDatabase, key: &str) -> ClientBudgetStatus {
    PgClientApiKeyRepository::new(database.pool.clone())
        .get_client_api_key(key)
        .await
        .unwrap()
        .unwrap()
        .budget
}

fn context() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "reconcile-test".to_owned(),
    }
}

#[tokio::test]
async fn budgets_settle_exactly_once_and_enforce_each_threshold_across_store_instances() {
    let Some(database) = TestDatabase::create("budgets_exact").await else {
        return;
    };
    seed(&database, "day", "0.3", "2").await;
    seed(&database, "week", "2", "0.2").await;
    let first = PgClientBudgetStore::new(database.pool.clone());
    let second = PgClientBudgetStore::new(database.pool.clone());
    for (key, prefix, amount) in [("day", "d", "0.1"), ("week", "w", "0.1")] {
        for index in 0..3 {
            let id = format!("{prefix}-{index}");
            // Admitted work can finish above the threshold; no estimated cost reservation.
            first.admit(admission(key, &id)).await.unwrap();
        }
        for index in 0..3 {
            let id = format!("{prefix}-{index}");
            let (a, b) = tokio::join!(
                first.settle(charge(&id, Some(amount))),
                second.settle(charge(&id, Some(amount)))
            );
            a.unwrap();
            b.unwrap();
        }
    }
    let day = status(&database, "day").await;
    assert_eq!(day.daily_used_usd.canonical(), "0.3");
    assert_eq!(day.weekly_used_usd.canonical(), "0.3");
    let day_error = second
        .admit(admission("day", "d-rejected"))
        .await
        .unwrap_err();
    assert_eq!(day_error.kind(), GatewayErrorKind::RateLimited);
    assert_eq!(
        day_error.client_error_code(),
        Some("key_daily_budget_exceeded")
    );
    assert!(day_error.retry_after().is_some());
    let week_error = first
        .admit(admission("week", "w-rejected"))
        .await
        .unwrap_err();
    assert_eq!(
        week_error.client_error_code(),
        Some("key_weekly_budget_exceeded")
    );
    let rejected_count: i64 = sqlx::query_scalar(
        "select count(*) from client_key_charge_events where request_id like '%rejected'",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(rejected_count, 0);
    database.close().await;
}

#[tokio::test]
async fn window_rollover_is_shanghai_midnight_and_seven_days_with_late_settlement() {
    let Some(database) = TestDatabase::create("budgets_windows").await else {
        return;
    };
    seed(&database, "key", "1", "2").await;
    let store = PgClientBudgetStore::new(database.pool.clone());
    store.admit(admission("key", "first")).await.unwrap();
    let (day_start, day_end, week_end): (DateTime<Utc>, DateTime<Utc>, DateTime<Utc>) =
        sqlx::query_as("select daily_start, daily_end, weekly_end from client_key_budget_windows where client_api_key_id = 'key'")
            .fetch_one(&database.pool).await.unwrap();
    assert_eq!(day_start.timestamp().rem_euclid(86400), 16 * 3600);
    assert_eq!((day_end - day_start).num_hours(), 24);
    assert_eq!((week_end - day_start).num_hours(), 168);
    store.settle(charge("first", Some("1"))).await.unwrap();
    sqlx::query("update client_key_budget_windows set daily_start = daily_start - interval '1 day', daily_end = daily_start")
        .execute(&database.pool).await.unwrap();
    store.admit(admission("key", "second")).await.unwrap();
    assert_eq!(
        status(&database, "key").await.daily_used_usd.canonical(),
        "0"
    );
    assert_eq!(
        status(&database, "key").await.weekly_used_usd.canonical(),
        "1"
    );
    store.settle(charge("second", Some("1"))).await.unwrap();
    sqlx::query("update client_key_budget_windows set daily_end = now() - interval '1 second', weekly_end = now() - interval '1 second'")
        .execute(&database.pool).await.unwrap();
    let virtual_reset = status(&database, "key").await;
    assert_eq!(virtual_reset.daily_used_usd.canonical(), "0");
    assert_eq!(virtual_reset.weekly_used_usd.canonical(), "0");
    store.admit(admission("key", "after-reset")).await.unwrap();
    let old = ClientBudgetCharge {
        completed_at: (day_start - chrono::Duration::seconds(1)).into(),
        ..charge("after-reset", Some("0.9"))
    };
    store.settle(old).await.unwrap();
    let reset = status(&database, "key").await;
    assert_eq!(reset.daily_used_usd.canonical(), "0");
    assert_eq!(reset.weekly_used_usd.canonical(), "0");
    assert!(reset.weekly_resets_at.is_some());
    database.close().await;
}

#[tokio::test]
async fn unknown_and_orphan_charges_block_limited_keys_until_audited_reconciliation() {
    let Some(database) = TestDatabase::create("budgets_unknown").await else {
        return;
    };
    seed(&database, "key", "1", "5").await;
    seed(&database, "other", "0", "0").await;
    let store = PgClientBudgetStore::new(database.pool.clone());
    let admin = PgAdminClientKeyStore::new(database.pool.clone());
    store.admit(admission("key", "unknown")).await.unwrap();
    store.settle(charge("unknown", None)).await.unwrap();
    assert_eq!(
        store
            .admit(admission("key", "blocked"))
            .await
            .unwrap_err()
            .client_error_code(),
        Some("key_budget_unresolved")
    );
    let key_id = ClientApiKeyId::new("key").unwrap();
    assert_eq!(admin.unresolved_charges(&key_id).await.unwrap().len(), 1);
    assert_eq!(status(&database, "key").await.unresolved_requests, 1);
    let reconcile = |key: &str, request: &str, amount: &str| ReconcileClientCharge {
        key_id: ClientApiKeyId::new(key).unwrap(),
        request_id: format!("req_{request}"),
        amount_usd: amount.parse().unwrap(),
        reason: "verified against upstream usage".to_owned(),
    };
    assert!(
        admin
            .reconcile_charge(reconcile("other", "unknown", "0.4"), &context())
            .await
            .is_err()
    );
    admin
        .reconcile_charge(reconcile("key", "unknown", "0.4"), &context())
        .await
        .unwrap();
    admin
        .reconcile_charge(reconcile("key", "unknown", "0.4"), &context())
        .await
        .unwrap();
    assert!(
        admin
            .reconcile_charge(reconcile("key", "unknown", "0.5"), &context())
            .await
            .is_err()
    );
    assert_eq!(
        status(&database, "key").await.daily_used_usd.canonical(),
        "0.4"
    );
    let audits: i64 = sqlx::query_scalar(
        "select count(*) from admin_audit_events where entity_kind = 'client_key_charge'",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(audits, 1);
    store.admit(admission("key", "orphan")).await.unwrap();
    assert!(
        admin
            .reconcile_charge(reconcile("key", "orphan", "0"), &context())
            .await
            .is_err()
    );
    sqlx::query("update client_key_charge_events set deadline_at = now() - interval '1 second' where request_id = 'req_orphan'")
        .execute(&database.pool).await.unwrap();
    let restarted = PgClientBudgetStore::new(database.pool.clone());
    assert_eq!(
        restarted
            .admit(admission("key", "restart-blocked"))
            .await
            .unwrap_err()
            .client_error_code(),
        Some("key_budget_unresolved")
    );
    admin
        .reconcile_charge(reconcile("key", "orphan", "0"), &context())
        .await
        .unwrap();
    restarted
        .admit(admission("key", "recovered"))
        .await
        .unwrap();
    assert_eq!(status(&database, "key").await.unresolved_requests, 0);
    store
        .admit(admission("other", "unlimited-unknown"))
        .await
        .unwrap();
    store
        .settle(charge("unlimited-unknown", None))
        .await
        .unwrap();
    store
        .admit(admission("other", "unlimited-next"))
        .await
        .unwrap();
    database.close().await;
}

#[tokio::test]
async fn budget_updates_preserve_omitted_limits_and_do_not_clear_usage() {
    let Some(database) = TestDatabase::create("budgets_policy").await else {
        return;
    };
    seed(&database, "key", "0", "0").await;
    let store = PgClientBudgetStore::new(database.pool.clone());
    let admin = PgAdminClientKeyStore::new(database.pool.clone());
    store.admit(admission("key", "unlimited")).await.unwrap();
    store
        .settle(charge("unlimited", Some("2.75")))
        .await
        .unwrap();
    let update = UpdateClientKey {
        id: ClientApiKeyId::new("key").unwrap(),
        name: "key".to_owned(),
        label: None,
        group_ids: vec![],
        limits: RateLimits {
            max_concurrency: 3,
            requests_per_minute: 0,
        },
        daily_limit_usd: Some("2".parse().unwrap()),
        weekly_limit_usd: Some("10".parse().unwrap()),
    };
    admin
        .update_client_key(update.clone(), &context())
        .await
        .unwrap();
    admin
        .update_client_key(
            UpdateClientKey {
                daily_limit_usd: None,
                weekly_limit_usd: None,
                ..update.clone()
            },
            &context(),
        )
        .await
        .unwrap();
    let current = status(&database, "key").await;
    assert_eq!(current.limits.daily_usd.canonical(), "2");
    assert_eq!(current.limits.weekly_usd.canonical(), "10");
    assert_eq!(current.daily_used_usd.canonical(), "2.75");
    assert_eq!(
        store
            .admit(admission("key", "blocked"))
            .await
            .unwrap_err()
            .client_error_code(),
        Some("key_daily_budget_exceeded")
    );
    admin
        .update_client_key(
            UpdateClientKey {
                daily_limit_usd: Some("0".parse().unwrap()),
                weekly_limit_usd: None,
                ..update
            },
            &context(),
        )
        .await
        .unwrap();
    store.admit(admission("key", "allowed")).await.unwrap();
    sqlx::query("update client_api_keys set enabled = false where id = 'key'")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .admit(admission("key", "disabled"))
            .await
            .unwrap_err()
            .kind(),
        GatewayErrorKind::PolicyDenied
    );
    sqlx::query("delete from client_api_keys where id = 'key'")
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .admit(admission("key", "deleted"))
            .await
            .unwrap_err()
            .kind(),
        GatewayErrorKind::Unauthorized
    );
    store.settle(charge("allowed", Some("1"))).await.unwrap();
    database.close().await;
}

#[tokio::test]
async fn budget_database_outage_fails_closed() {
    let Some(database) = TestDatabase::create("budgets_outage").await else {
        return;
    };
    let store = PgClientBudgetStore::new(database.pool.clone());
    database.pool.close().await;
    assert_eq!(
        store
            .admit(admission("key", "offline"))
            .await
            .unwrap_err()
            .client_error_code(),
        Some("key_budget_unavailable")
    );
    assert!(store.settle(charge("offline", Some("1"))).await.is_err());
    database.close().await;
}
