use std::time::Duration;

use futures::{StreamExt, TryStreamExt, stream};
use gateway_store::{StoreError, postgres::ObservabilityQueryBudget};
use tokio::sync::{mpsc, oneshot};

#[test]
fn query_budget_rejects_an_empty_budget() {
    assert!(ObservabilityQueryBudget::try_new(0, Duration::from_secs(1)).is_err());
}

#[tokio::test]
async fn query_stream_holds_budget_until_completion_error_or_drop() {
    let budget =
        ObservabilityQueryBudget::try_new(1, Duration::from_millis(10)).expect("query budget");
    for terminal in [
        None,
        Some(Ok(2)),
        Some(Err(StoreError::InvalidData {
            entity: "test stream",
            message: "decode failed".to_owned(),
        })),
    ] {
        let pending = terminal.is_none();
        let query = stream::iter([Ok(1)]).chain(stream::iter(terminal));
        let mut facts = budget.run_stream("stream test", query);
        assert_eq!(facts.try_next().await.expect("first row"), Some(1));
        assert!(budget.run("blocked query", async { Ok(()) }).await.is_err());
        if pending {
            drop(facts);
        } else {
            match facts.try_next().await {
                Ok(Some(2)) => assert!(facts.try_next().await.expect("end of stream").is_none()),
                Err(StoreError::InvalidData { .. }) => {}
                other => panic!("unexpected stream result: {other:?}"),
            }
            // The finished stream may remain alive; its connection slot must already be free.
            budget
                .run("after terminal", async { Ok(()) })
                .await
                .expect("released slot");
        }
        budget
            .run("after stream", async { Ok(()) })
            .await
            .expect("released slot");
    }
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
