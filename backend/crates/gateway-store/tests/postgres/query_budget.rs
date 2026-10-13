//! 验证观测预算的容量、排队期限和取消后的槽位释放

use std::time::Duration;

use gateway_store::{StoreError, postgres::ObservabilityQueryBudget};
use sqlx::{Connection, PgPool, postgres::PgPoolOptions};
use tokio::sync::{mpsc, oneshot};

async fn test_pool() -> Option<PgPool> {
    let url = crate::support::test_env("CPR_TEST_DATABASE_URL")?;
    Some(
        PgPoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .unwrap(),
    )
}

#[test]
fn query_budget_rejects_an_empty_budget() {
    assert!(
        ObservabilityQueryBudget::try_new(0, Duration::from_secs(1), Duration::from_secs(10))
            .is_err()
    );
}

#[tokio::test]
async fn running_query_deadline_releases_the_slot() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let budget =
        ObservabilityQueryBudget::try_new(1, Duration::from_millis(10), Duration::from_millis(250))
            .unwrap();
    let error = budget
        .run(&pool, "never completes", |connection| async move {
            let result = std::future::pending::<Result<(), StoreError>>().await;
            drop(connection);
            result
        })
        .await
        .expect_err("whole operation must have a deadline");
    assert!(
        matches!(error, StoreError::Unavailable { message, .. } if message == "observability PostgreSQL execution budget is exhausted")
    );
    budget
        .run(&pool, "after timeout", |_connection| async { Ok(()) })
        .await
        .expect("timeout releases slot");
    pool.close().await;
}

#[tokio::test]
async fn queued_time_is_included_in_the_operation_deadline() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let budget =
        ObservabilityQueryBudget::try_new(1, Duration::from_secs(2), Duration::from_secs(2))
            .unwrap();
    let (occupied_tx, occupied_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let occupied_budget = budget.clone();
    let occupied_pool = pool.clone();
    let occupied = tokio::spawn(async move {
        occupied_budget
            .run(
                &occupied_pool,
                "hold before queued deadline",
                |connection| async move {
                    occupied_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    drop(connection);
                    Ok(())
                },
            )
            .await
            .unwrap();
    });
    occupied_rx.await.unwrap();
    let started = tokio::time::Instant::now();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2) / 5).await;
        release_tx.send(()).unwrap();
    });
    let error = budget
        .run(
            &pool,
            "queued operation deadline",
            |connection| async move {
                let result = std::future::pending::<Result<(), StoreError>>().await;
                drop(connection);
                result
            },
        )
        .await
        .expect_err("queue and execution share one deadline");
    assert!(
        matches!(error, StoreError::Unavailable { message, .. } if message.contains("execution budget"))
    );
    assert!(
        started.elapsed() < Duration::from_secs(2) + Duration::from_millis(200),
        "acquiring a slot must not restart the operation budget"
    );
    release.await.unwrap();
    occupied.await.unwrap();
    budget
        .run(&pool, "after queued timeout", |_connection| async {
            Ok(())
        })
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn query_budget_times_out_without_exceeding_its_connection_limit() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let budget =
        ObservabilityQueryBudget::try_new(4, Duration::from_millis(10), Duration::from_secs(10))
            .expect("valid query budget");
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let mut releases = Vec::new();
    let mut occupied = Vec::new();
    for _ in 0..4 {
        let occupied_budget = budget.clone();
        let occupied_pool = pool.clone();
        let started_tx = started_tx.clone();
        let (release_tx, release_rx) = oneshot::channel();
        releases.push(release_tx);
        occupied.push(tokio::spawn(async move {
            occupied_budget
                .run(
                    &occupied_pool,
                    "occupied test query",
                    |connection| async move {
                        started_tx.send(()).expect("report occupied slot");
                        release_rx.await.expect("release occupied slot");
                        drop(connection);
                        Ok::<_, StoreError>(())
                    },
                )
                .await
        }));
    }
    for _ in 0..4 {
        started_rx.recv().await.expect("occupied slot started");
    }

    let error = budget
        .run(&pool, "queued test query", |_connection| async {
            Ok::<_, StoreError>(())
        })
        .await
        .expect_err("exhausted query budget must time out");

    assert!(matches!(
        error,
        StoreError::Unavailable { message, .. }
            if message == "observability PostgreSQL connection budget is exhausted"
    ));
    for release in releases {
        release.send(()).expect("release occupied slot");
    }
    for task in occupied {
        task.await
            .expect("occupied task must not panic")
            .expect("occupied query must succeed");
    }
    pool.close().await;
}

#[tokio::test]
async fn queued_query_runs_after_one_slot_is_released() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let budget =
        ObservabilityQueryBudget::try_new(4, Duration::from_secs(1), Duration::from_secs(10))
            .expect("valid query budget");
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let mut releases = Vec::new();
    let mut occupied = Vec::new();
    for _ in 0..4 {
        let occupied_budget = budget.clone();
        let occupied_pool = pool.clone();
        let started_tx = started_tx.clone();
        let (release_tx, release_rx) = oneshot::channel();
        releases.push(release_tx);
        occupied.push(tokio::spawn(async move {
            occupied_budget
                .run(
                    &occupied_pool,
                    "occupied test query",
                    |connection| async move {
                        started_tx.send(()).expect("report occupied slot");
                        release_rx.await.expect("release occupied slot");
                        drop(connection);
                        Ok::<_, StoreError>(())
                    },
                )
                .await
        }));
    }
    for _ in 0..4 {
        started_rx.recv().await.expect("occupied slot started");
    }
    let queued_budget = budget.clone();
    let queued_pool = pool.clone();
    let queued = tokio::spawn(async move {
        queued_budget
            .run(&queued_pool, "queued test query", |_connection| async {
                Ok::<_, StoreError>(())
            })
            .await
    });

    releases
        .pop()
        .expect("one occupied slot")
        .send(())
        .expect("release one occupied slot");
    tokio::time::timeout(Duration::from_millis(100), queued)
        .await
        .expect("one released slot must admit one queued query")
        .expect("queued task must not panic")
        .expect("queued query must succeed");

    for release in releases {
        release.send(()).expect("release occupied slot");
    }
    for task in occupied {
        task.await
            .expect("occupied task must not panic")
            .expect("occupied query must succeed");
    }
    pool.close().await;
}

fn database_error(source: sqlx::Error) -> StoreError {
    StoreError::Unavailable {
        backend: gateway_store::StoreBackend::PostgreSql,
        message: "query budget test SQL failed".to_owned(),
        source: Some(gateway_core::error::ErrorSource::new(source)),
    }
}

#[tokio::test]
async fn operation_timeout_keeps_the_slot_until_the_database_query_finishes() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let budget =
        ObservabilityQueryBudget::try_new(1, Duration::from_millis(100), Duration::from_secs(2))
            .unwrap();
    let (pid_tx, pid_rx) = oneshot::channel();
    let error = budget
        .run(
            &pool,
            "multi statement deadline",
            |mut connection| async move {
                let pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(database_error)?;
                pid_tx.send(pid).unwrap();
                let mut tx = connection.begin().await.map_err(database_error)?;
                sqlx::query("select set_config('statement_timeout',$1,true)")
                    .bind(format!("{}ms", Duration::from_secs(2).as_millis()))
                    .execute(&mut *tx)
                    .await
                    .map_err(database_error)?;
                sqlx::query("select pg_sleep($1)")
                    .bind(Duration::from_secs(2).as_secs_f64() * 0.6)
                    .execute(&mut *tx)
                    .await
                    .map_err(database_error)?;
                sqlx::query("select pg_sleep($1)")
                    .bind(Duration::from_secs(2).as_secs_f64() * 0.9)
                    .execute(&mut *tx)
                    .await
                    .map_err(database_error)?;
                tx.commit().await.map_err(database_error)?;
                Ok(())
            },
        )
        .await
        .expect_err("operation deadline must expire before the second statement");
    assert!(
        matches!(error, StoreError::Unavailable { message, .. } if message.contains("execution budget"))
    );
    let pid = pid_rx.await.unwrap();
    let state: String = sqlx::query_scalar("select state from pg_stat_activity where pid=$1")
        .bind(pid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        state, "active",
        "the timed-out statement is still executing"
    );
    let queued = budget
        .run(&pool, "after operation timeout", |_connection| async {
            Ok(())
        })
        .await;
    assert!(
        matches!(queued, Err(StoreError::Unavailable { message, .. }) if message.contains("connection budget is exhausted"))
    );
    tokio::time::timeout(
        Duration::from_secs(1),
        sqlx::query("select 1").execute(&pool),
    )
    .await
    .expect("data plane capacity stays available")
    .unwrap();
    // 只取消本用例持有的后台查询，避免让睡眠耗尽测试时间
    sqlx::query("select pg_cancel_backend($1)")
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if budget
                .run(&pool, "after database cancellation", |_connection| async {
                    Ok(())
                })
                .await
                .is_ok()
            {
                break;
            }
        }
    })
    .await
    .expect("completed rollback releases the budget");
    pool.close().await;
}

#[tokio::test]
async fn dropping_an_operation_keeps_the_slot_through_database_rollback() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let budget =
        ObservabilityQueryBudget::try_new(1, Duration::from_millis(100), Duration::from_secs(10))
            .unwrap();
    let running_pool = pool.clone();
    let running_budget = budget.clone();
    let (pid_tx, pid_rx) = oneshot::channel();
    let running = tokio::spawn(async move {
        running_budget
            .run(
                &running_pool,
                "cancelled operation",
                |mut connection| async move {
                    let pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
                        .fetch_one(&mut *connection)
                        .await
                        .map_err(database_error)?;
                    pid_tx.send(pid).unwrap();
                    let mut tx = connection.begin().await.map_err(database_error)?;
                    sqlx::query("set local statement_timeout = '10s'")
                        .execute(&mut *tx)
                        .await
                        .map_err(database_error)?;
                    sqlx::query("select pg_sleep(20)")
                        .execute(&mut *tx)
                        .await
                        .map_err(database_error)?;
                    tx.commit().await.map_err(database_error)?;
                    Ok(())
                },
            )
            .await
    });
    let pid = pid_rx.await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let sleeping: bool = sqlx::query_scalar(
                "select coalesce(wait_event='PgSleep',false) from pg_stat_activity where pid=$1",
            )
            .bind(pid)
            .fetch_one(&pool)
            .await
            .unwrap();
            if sleeping {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("query reaches the server before cancellation");
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    let queued = budget
        .run(&pool, "after caller cancellation", |_connection| async {
            Ok(())
        })
        .await;
    assert!(
        matches!(queued, Err(StoreError::Unavailable { message, .. }) if message.contains("connection budget is exhausted"))
    );
    sqlx::query("select pg_cancel_backend($1)")
        .bind(pid)
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if budget
                .run(&pool, "after rollback", |mut connection| async move {
                    sqlx::query("select 1")
                        .execute(&mut *connection)
                        .await
                        .map_err(database_error)?;
                    Ok(())
                })
                .await
                .is_ok()
            {
                break;
            }
        }
    })
    .await
    .expect("cancelled operation eventually returns a usable connection");
    let idle_transactions: i64 = sqlx::query_scalar(
        "select count(*) from pg_stat_activity where pid=$1 and state='idle in transaction'",
    )
    .bind(pid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(idle_transactions, 0);
    pool.close().await;
}
