use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use gateway_store::postgres::{
    PgRetentionRepository, RetentionCycleBudget, RetentionRepository as _, RuntimeRetentionSettings,
};

use super::TestDatabase;

#[test]
fn retention_settings_preserve_independent_windows() {
    let settings = RuntimeRetentionSettings {
        usage_retention_days: 31,
        ops_event_retention_days: 30,
        audit_retention_days: 90,
    };
    assert_eq!(settings.audit_retention_days, 90);
}

#[tokio::test]
async fn retention_cycle_should_stop_at_the_batch_budget() {
    let Some(database) = TestDatabase::create("retention_cycle_budget").await else {
        return;
    };
    let expired_at = Utc::now() - ChronoDuration::days(100);
    sqlx::query(
        "insert into admin_audit_events (
           id, actor_kind, actor_ref, action, entity_kind, entity_ref,
           changed_fields, created_at
         )
         select 'retention-' || value::text, 'system', 'retention', 'cleanup',
                'fixture', 'fixture-' || value::text, array[]::text[], $1
         from generate_series(1, 5) as value",
    )
    .bind(expired_at)
    .execute(&database.pool)
    .await
    .expect("seed expired audit events");

    let budget = RetentionCycleBudget::try_new(2, 3, Duration::from_secs(1), Duration::ZERO)
        .expect("retention cycle budget");
    let repository = PgRetentionRepository::with_cycle_budget(database.pool.clone(), budget);
    let report = repository
        .apply_retention(
            Utc::now(),
            RuntimeRetentionSettings {
                usage_retention_days: 31,
                ops_event_retention_days: 30,
                audit_retention_days: 90,
            },
        )
        .await
        .expect("bounded retention cycle");

    assert_eq!(report.model_requests, 0);
    assert_eq!(report.ops_events, 0);
    assert_eq!(report.admin_audit_events, 2);
    assert_eq!(report.batches, 3);
    assert!(report.budget_exhausted);
    let remaining: i64 =
        sqlx::query_scalar("select count(*) from admin_audit_events where id like 'retention-%'")
            .fetch_one(&database.pool)
            .await
            .expect("count remaining audit events");
    assert_eq!(remaining, 3);
    database.close().await;
}
