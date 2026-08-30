use std::time::Duration;

use gateway_store::{StoreError, postgres::ObservabilityQueryBudget};
use tokio::sync::{mpsc, oneshot};

#[test]
fn query_budget_rejects_an_empty_budget() {
    assert!(ObservabilityQueryBudget::try_new(0, Duration::from_secs(1)).is_err());
}

#[tokio::test]
async fn query_budget_times_out_without_exceeding_its_connection_limit() {
    let budget = ObservabilityQueryBudget::try_new(4, Duration::from_millis(10))
        .expect("valid query budget");
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let mut releases = Vec::new();
    let mut occupied = Vec::new();
    for _ in 0..4 {
        let occupied_budget = budget.clone();
        let started_tx = started_tx.clone();
        let (release_tx, release_rx) = oneshot::channel();
        releases.push(release_tx);
        occupied.push(tokio::spawn(async move {
            occupied_budget
                .run("occupied test query", async move {
                    started_tx.send(()).expect("report occupied slot");
                    release_rx.await.expect("release occupied slot");
                    Ok::<_, StoreError>(())
                })
                .await
        }));
    }
    for _ in 0..4 {
        started_rx.recv().await.expect("occupied slot started");
    }

    let error = budget
        .run("queued test query", async { Ok::<_, StoreError>(()) })
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
}

#[tokio::test]
async fn queued_query_runs_after_one_slot_is_released() {
    let budget =
        ObservabilityQueryBudget::try_new(4, Duration::from_secs(1)).expect("valid query budget");
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let mut releases = Vec::new();
    let mut occupied = Vec::new();
    for _ in 0..4 {
        let occupied_budget = budget.clone();
        let started_tx = started_tx.clone();
        let (release_tx, release_rx) = oneshot::channel();
        releases.push(release_tx);
        occupied.push(tokio::spawn(async move {
            occupied_budget
                .run("occupied test query", async move {
                    started_tx.send(()).expect("report occupied slot");
                    release_rx.await.expect("release occupied slot");
                    Ok::<_, StoreError>(())
                })
                .await
        }));
    }
    for _ in 0..4 {
        started_rx.recv().await.expect("occupied slot started");
    }
    let queued_budget = budget.clone();
    let queued = tokio::spawn(async move {
        queued_budget
            .run("queued test query", async { Ok::<_, StoreError>(()) })
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
}
