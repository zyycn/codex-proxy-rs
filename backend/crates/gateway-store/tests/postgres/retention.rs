//! 验证历史清理仅按批次上限删除已过期记录

use std::num::NonZeroU32;

use chrono::{Duration as ChronoDuration, Utc};
use gateway_admin::{
    model::retention::{RetentionPolicy, RetentionTarget},
    ports::retention::RetentionStore as _,
};
use gateway_store::postgres::PgRetentionRepository;

use super::TestDatabase;

#[tokio::test]
async fn retention_batch_deletes_only_expired_rows_up_to_limit() {
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
                'fixture', 'fixture-' || value::text, array[]::text[],
                case when value = 6 then now() else $1 end
         from generate_series(1, 6) as value",
    )
    .bind(expired_at)
    .execute(&database.pool)
    .await
    .expect("seed expired and retained audit events");

    let repository = PgRetentionRepository::new(database.pool.clone());
    let deleted = repository
        .purge_batch(
            RetentionTarget::AdminAuditEvents,
            Utc::now(),
            RetentionPolicy::try_new(31, 30, 90).unwrap(),
            NonZeroU32::new(2).unwrap(),
        )
        .await
        .expect("bounded retention batch");
    assert_eq!(deleted, 2);
    let remaining: i64 =
        sqlx::query_scalar("select count(*) from admin_audit_events where id like 'retention-%'")
            .fetch_one(&database.pool)
            .await
            .expect("count remaining audit events");
    assert_eq!(remaining, 4);
    let policy = repository.load_policy().await.expect("load valid policy");
    assert_eq!(
        repository
            .purge_batch(
                RetentionTarget::AdminAuditEvents,
                Utc::now(),
                policy,
                NonZeroU32::new(10).unwrap(),
            )
            .await
            .expect("finish expired audit cleanup"),
        3,
    );
    let retained: Vec<String> =
        sqlx::query_scalar("select id from admin_audit_events where id like 'retention-%'")
            .fetch_all(&database.pool)
            .await
            .expect("read retained audit events");
    assert_eq!(retained, ["retention-6"]);
    database.close().await;
}
