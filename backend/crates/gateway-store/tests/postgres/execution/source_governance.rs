//! 验证历史引用独立、租约并发与终结清理的数据库合同

use super::{TestDatabase, accepted_request};
use chrono::{Duration, Utc};
use gateway_core::engine::ExecutionStore as _;
use gateway_store::postgres::{ModelRequestRepository as _, PgExecutionStore};

#[tokio::test]
async fn lease_renewal_does_not_update_historical_request_tuple() {
    let Some(db) = TestDatabase::create("lease_narrow_update").await else {
        return;
    };
    let store = PgExecutionStore::new(db.pool.clone());
    let request = accepted_request("req_narrow_lease");
    store.create_model_request(request.clone()).await.unwrap();
    let before: (String, String) =
        sqlx::query_as("select ctid::text, xmin::text from model_requests where id = $1")
            .bind(request.id.as_str())
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let updated = sqlx::query("update model_request_leases set lease_expires_at = lease_expires_at + interval '1 minute' where request_id = $1")
        .bind(request.id.as_str()).execute(&db.pool).await.unwrap();
    assert_eq!(updated.rows_affected(), 1);
    let after: (String, String, Option<chrono::DateTime<Utc>>) = sqlx::query_as("select ctid::text, xmin::text, recovery_lease_expires_at from model_requests where id = $1")
        .bind(request.id.as_str()).fetch_one(&db.pool).await.unwrap();
    assert_eq!((after.0, after.1), before);
    assert_eq!(after.2, None);
    db.close().await;
}

#[tokio::test]
async fn recovery_rechecks_lease_after_waiting_for_concurrent_renewal() {
    let Some(db) = TestDatabase::create("lease_recovery_race").await else {
        return;
    };
    let store = PgExecutionStore::new(db.pool.clone());
    let request = accepted_request("req_renewal_wins");
    store.create_model_request(request.clone()).await.unwrap();
    let now = Utc::now() + Duration::seconds(40);
    let mut renewal = db.pool.begin().await.unwrap();
    let renewal_pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
        .fetch_one(&mut *renewal)
        .await
        .unwrap();
    sqlx::query("update model_request_leases set lease_expires_at = $2 where request_id = $1")
        .bind(request.id.as_str())
        .bind(now + Duration::minutes(1))
        .execute(&mut *renewal)
        .await
        .unwrap();
    let recovery = tokio::spawn(async move { store.recover_expired_model_requests(now).await });
    // 必须实际进入等待续期事务的路径，避免只验证续期已提交后的普通读取
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "select exists (select 1 from pg_stat_activity where $1 = any(pg_blocking_pids(pid)))",
            )
            .bind(renewal_pid)
            .fetch_one(&db.admin)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("recovery must wait for the uncommitted renewal");
    renewal.commit().await.unwrap();
    assert_eq!(recovery.await.unwrap().unwrap().requests, 0);
    let outcome: String =
        sqlx::query_scalar("select outcome from model_requests where id = 'req_renewal_wins'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(outcome, "running");
    db.close().await;
}

#[tokio::test]
async fn missing_lease_is_recovered_and_terminal_transition_freezes_last_expiry() {
    let Some(db) = TestDatabase::create("lease_missing_recovery").await else {
        return;
    };
    let store = PgExecutionStore::new(db.pool.clone());
    for id in ["req_missing_lease", "req_expired_lease"] {
        store
            .create_model_request(accepted_request(id))
            .await
            .unwrap();
    }
    let expiry: chrono::DateTime<Utc> = sqlx::query_scalar(
        "select lease_expires_at from model_request_leases where request_id = 'req_expired_lease'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    sqlx::query("delete from model_request_leases where request_id = 'req_missing_lease'")
        .execute(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .recover_expired_model_requests(expiry + Duration::seconds(1))
            .await
            .unwrap()
            .requests,
        2
    );
    let final_expiry: chrono::DateTime<Utc> = sqlx::query_scalar(
        "select recovery_lease_expires_at from model_requests where id = 'req_expired_lease'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(final_expiry, expiry);
    let error_message: String = sqlx::query_scalar("select request_observation_json #>> '{error,message}' from model_requests where id = 'req_missing_lease'")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(error_message, "request recovery lease is missing");
    let count: i64 = sqlx::query_scalar("select count(*) from model_request_leases")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        store
            .recover_expired_model_requests(expiry + Duration::seconds(2))
            .await
            .unwrap()
            .requests,
        0
    );
    db.close().await;
}

#[tokio::test]
async fn terminal_transition_and_lease_cleanup_roll_back_together() {
    let Some(db) = TestDatabase::create("lease_terminal_rollback").await else {
        return;
    };
    let store = PgExecutionStore::new(db.pool.clone());
    store
        .create_model_request(accepted_request("req_terminal_rollback"))
        .await
        .unwrap();
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query("update model_requests set outcome = 'incomplete', completed_at = started_at where id = 'req_terminal_rollback'")
        .execute(&mut *tx).await.unwrap();
    let count: i64 = sqlx::query_scalar("select count(*) from model_request_leases")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 0);
    tx.rollback().await.unwrap();
    let state: (String, i64) = sqlx::query_as("select outcome, (select count(*) from model_request_leases) from model_requests where id = 'req_terminal_rollback'")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(state, ("running".to_owned(), 1));
    db.close().await;
}
