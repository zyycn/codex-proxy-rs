//! 验证慢结算隔离、待结算额度、幂等重试和后台写入生命周期

use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use gateway_admin::{
    model::client_keys::{
        ClientKeyBudgetMutationOrigin, ClientKeyBudgetPeriod, ResetClientKeyBudget,
    },
    ports::store::ClientKeyStore as _,
};
use gateway_core::{
    engine::budget::ClientBudgetPort,
    lifecycle::CancellationToken,
    task::{DaemonTask, WorkerDaemonContext},
};
use gateway_store::postgres::{
    BufferedClientBudgetStore, ClientBudgetWriter, PgAdminClientKeyStore, PgClientBudgetStore,
};

use super::{
    TestDatabase,
    client_budgets::{charge, context, key_id, seed, status},
};

fn buffered(pool: sqlx::PgPool) -> (BufferedClientBudgetStore, Arc<ClientBudgetWriter>) {
    let (store, writer) = BufferedClientBudgetStore::new(
        PgClientBudgetStore::new(pool),
        Arc::new(crate::RecordingDiagnostics::default()),
    );
    (store, Arc::new(writer))
}

async fn wait_for_writes(context: &WorkerDaemonContext, expected: u64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let completed = context.completed_cycles();
            if completed >= expected {
                break;
            }
            context.progress_since(completed).await;
        }
    })
    .await
    .expect("background budget writes must complete");
}

async fn drain(writer: &ClientBudgetWriter) {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    writer
        .run(WorkerDaemonContext::new(cancellation))
        .await
        .unwrap();
}

#[tokio::test]
async fn slow_settlement_does_not_block_receipt_or_concurrent_admission() {
    let Some(database) = TestDatabase::create("budget_buffer_slow").await else {
        return;
    };
    let direct = PgClientBudgetStore::new(database.pool.clone());
    for (key, daily) in [("allowed", "10"), ("exhausted", "1")] {
        seed(&database, key, daily, "0").await;
        direct.admit(key_id(key)).await.unwrap();
    }
    // 控制连接独立于数据面连接池，保证测试只阻塞结算所需行锁
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect_with((*database.pool.connect_options()).clone())
        .await
        .unwrap();
    let (store, writer) = buffered(pool.clone());
    let mut blocker = database.pool.begin().await.unwrap();
    sqlx::query("select id from client_api_keys for update")
        .fetch_all(&mut *blocker)
        .await
        .unwrap();
    for key in ["allowed", "exhausted"] {
        let fee = charge(key, key, "1.25");
        tokio::time::timeout(Duration::from_millis(250), async {
            store.settle(fee.clone()).await.unwrap();
            store.settle(fee).await.unwrap();
        })
        .await
        .expect("completion must only enqueue, including duplicate receipts");
    }
    let worker_context = WorkerDaemonContext::new(CancellationToken::new());
    let running_context = worker_context.clone();
    let task = tokio::spawn(async move { writer.run(running_context).await });
    // 行锁直到准入断言结束才释放，等待行锁会被准入自身的一秒期限判为失败
    // 不给整组请求另设更短的期限，避免把连接建立和并发调度耗时误判为锁等待
    for result in futures::future::join_all((0..4).map(|_| store.admit(key_id("allowed")))).await {
        result.expect("admission must not wait for settlement row locks");
    }
    let error = store.admit(key_id("exhausted")).await.unwrap_err();
    assert_eq!(error.client_error_code(), Some("key_daily_budget_exceeded"));
    let before: i64 = sqlx::query_scalar("select count(*) from client_api_key_charge_events")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(before, 0);
    blocker.commit().await.unwrap();
    wait_for_writes(&worker_context, 2).await;
    worker_context.cancellation().cancel();
    task.await.unwrap().unwrap();
    assert_eq!(
        status(&database, "allowed")
            .await
            .daily_used_usd
            .canonical(),
        "1.25"
    );
    assert_eq!(
        status(&database, "exhausted")
            .await
            .daily_used_usd
            .canonical(),
        "1.25"
    );
    pool.close().await;
    database.close().await;
}

#[tokio::test]
async fn pending_cost_respects_committed_ids_and_manual_reset_boundaries() {
    let Some(database) = TestDatabase::create("budget_buffer_reset").await else {
        return;
    };
    seed(&database, "key", "2", "0").await;
    let direct = PgClientBudgetStore::new(database.pool.clone());
    let (store, writer) = buffered(database.pool.clone());
    store.admit(key_id("key")).await.unwrap();
    let committed = charge("key", "committed", "1.25");
    store.settle(committed.clone()).await.unwrap();
    direct.settle(committed).await.unwrap();
    // 模拟提交成功、队列尚未移除，金额不能按 2.5 重复计算
    store.admit(key_id("key")).await.unwrap();
    store
        .settle(charge("key", "before-reset", "3"))
        .await
        .unwrap();
    assert_eq!(
        store
            .admit(key_id("key"))
            .await
            .unwrap_err()
            .client_error_code(),
        Some("key_daily_budget_exceeded")
    );
    PgAdminClientKeyStore::new(database.pool.clone())
        .reset_client_key_budget(
            ResetClientKeyBudget {
                id: key_id("key"),
                period: ClientKeyBudgetPeriod::All,
            },
            ClientKeyBudgetMutationOrigin::Admin,
            &context(),
        )
        .await
        .unwrap();
    store.admit(key_id("key")).await.unwrap();
    store
        .settle(charge("key", "after-reset", "2.5"))
        .await
        .unwrap();
    assert_eq!(
        store
            .admit(key_id("key"))
            .await
            .unwrap_err()
            .client_error_code(),
        Some("key_daily_budget_exceeded")
    );
    drain(&writer).await;
    assert_eq!(
        status(&database, "key").await.daily_used_usd.canonical(),
        "2.5"
    );
    assert_eq!(
        status(&database, "key").await.weekly_used_usd.canonical(),
        "2.5"
    );
    database.close().await;
}

#[tokio::test]
async fn failed_write_retains_cost_and_retries_without_another_request() {
    let Some(database) = TestDatabase::create("budget_buffer_retry").await else {
        return;
    };
    seed(&database, "key", "1", "0").await;
    let (store, writer) = buffered(database.pool.clone());
    store.admit(key_id("key")).await.unwrap();
    sqlx::raw_sql(
        "create sequence write_attempts;
        create function reject_budget_update() returns trigger language plpgsql as $$ begin
            perform nextval('write_attempts');
            raise exception 'injected settlement failure';
        end $$;
        create trigger reject_budget_update before update on client_api_key_budget_windows
            for each row execute function reject_budget_update();",
    )
    .execute(&database.pool)
    .await
    .unwrap();
    for (request, amount) in [("retry-a", "0.25"), ("retry-b", "0.5"), ("retry-c", "0.5")] {
        store.settle(charge("key", request, amount)).await.unwrap();
    }
    writer
        .run(WorkerDaemonContext::new(CancellationToken::new()))
        .await
        .unwrap_err();
    for result in futures::future::join_all((0..4).map(|_| store.admit(key_id("key")))).await {
        assert_eq!(
            result.unwrap_err().client_error_code(),
            Some("key_daily_budget_exceeded")
        );
    }
    let attempts: i64 = sqlx::query_scalar("select last_value from write_attempts")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(attempts, 1, "admission never retries writes");
    let rows: i64 = sqlx::query_scalar("select count(*) from client_api_key_charge_events")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "the entire batch rolls back");
    assert_eq!(
        status(&database, "key").await.daily_used_usd.canonical(),
        "0"
    );
    sqlx::query("drop trigger reject_budget_update on client_api_key_budget_windows")
        .execute(&database.pool)
        .await
        .unwrap();
    let worker_context = WorkerDaemonContext::new(CancellationToken::new());
    let running_context = worker_context.clone();
    let task = tokio::spawn(async move { writer.run(running_context).await });
    wait_for_writes(&worker_context, 1).await;
    worker_context.cancellation().cancel();
    task.await.unwrap().unwrap();
    assert_eq!(
        status(&database, "key").await.daily_used_usd.canonical(),
        "1.25"
    );
    database.close().await;
}

#[tokio::test]
async fn cancelled_writer_requeues_inflight_cost_for_shutdown_drain() {
    let Some(database) = TestDatabase::create("budget_buffer_cancel").await else {
        return;
    };
    seed(&database, "key", "0", "0").await;
    let name = format!("budget_cancel_{}", uuid::Uuid::new_v4().simple());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect_with(
            (*database.pool.connect_options())
                .clone()
                .application_name(&name),
        )
        .await
        .unwrap();
    let (store, writer) = buffered(pool.clone());
    store.admit(key_id("key")).await.unwrap();
    let mut blocker = database.pool.begin().await.unwrap();
    sqlx::query("select id from client_api_keys for update")
        .fetch_all(&mut *blocker)
        .await
        .unwrap();
    store
        .settle(charge("key", "cancelled", "0.75"))
        .await
        .unwrap();
    let running = writer.clone();
    let task = tokio::spawn(async move {
        running
            .run(WorkerDaemonContext::new(CancellationToken::new()))
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let blocked: bool = sqlx::query_scalar("select exists(select 1 from pg_stat_activity where application_name=$1 and wait_event_type='Lock')")
                .bind(&name).fetch_one(&database.pool).await.unwrap();
            if blocked { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    blocker.commit().await.unwrap();
    drain(&writer).await;
    assert_eq!(
        status(&database, "key").await.daily_used_usd.canonical(),
        "0.75"
    );
    pool.close().await;
    database.close().await;
}

#[tokio::test]
async fn full_queue_rejects_new_admission_but_keeps_already_completed_costs() {
    let Some(database) = TestDatabase::create("budget_buffer_full").await else {
        return;
    };
    for key in ["key", "other"] {
        seed(&database, key, "0", "0").await;
    }
    let (store, writer) = BufferedClientBudgetStore::with_capacity(
        PgClientBudgetStore::new(database.pool.clone()),
        NonZeroUsize::new(2).unwrap(),
        Arc::new(crate::RecordingDiagnostics::default()),
    );
    for (i, (key, amount)) in [("key", "1"), ("other", "0.25"), ("key", "2")]
        .into_iter()
        .enumerate()
    {
        store
            .settle(charge(key, &format!("fee-{i}"), amount))
            .await
            .unwrap();
    }
    assert_eq!(
        store
            .admit(key_id("key"))
            .await
            .unwrap_err()
            .client_error_code(),
        Some("key_budget_unavailable")
    );
    drain(&writer).await;
    assert_eq!(
        status(&database, "key").await.daily_used_usd.canonical(),
        "3"
    );
    assert_eq!(
        status(&database, "other").await.daily_used_usd.canonical(),
        "0.25"
    );
    database.close().await;
}

#[tokio::test]
async fn stalled_budget_read_has_a_total_admission_deadline() {
    let Some(database) = TestDatabase::create("budget_buffer_deadline").await else {
        return;
    };
    seed(&database, "key", "0", "0").await;
    let (store, _writer) = buffered(database.pool.clone());
    let mut blocker = database.pool.begin().await.unwrap();
    sqlx::query("lock table client_api_keys in access exclusive mode")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let error = tokio::time::timeout(Duration::from_millis(1_500), store.admit(key_id("key")))
        .await
        .expect("budget query must have a total deadline")
        .unwrap_err();
    assert_eq!(error.client_error_code(), Some("key_budget_unavailable"));
    blocker.rollback().await.unwrap();
    database.close().await;
}
