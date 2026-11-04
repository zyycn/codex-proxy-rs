//! 在观测总预算内持有只读游标，按消费需求交付有界批次

use super::*;
use futures::TryStreamExt;
use gateway_core::lifecycle::CancellationToken;
use sqlx::postgres::PgRow;
use tokio::sync::{mpsc, oneshot};

type BatchReply = oneshot::Sender<UsageReadBatch>;

struct Reader {
    requests: mpsc::Sender<BatchReply>,
    task: Option<tokio::task::JoinHandle<UsageDataResult<()>>>,
    cancellation: CancellationToken,
}

pub(super) fn open(
    store: &PgUsageDataStore,
    query: UsageReadQuery,
    cancellation: CancellationToken,
) -> Box<dyn UsageReadSession> {
    let (requests, receiver) = mpsc::channel(1);
    let store = store.clone();
    let cancellation = cancellation.child_token();
    let worker_cancellation = cancellation.clone();
    let task = tokio::spawn(async move {
        let run = store
            .budget
            .run(&store.pool, "usage read", |mut connection| async move {
                drive(&mut connection, query, receiver).await
            });
        tokio::select! {
            biased;
            () = worker_cancellation.cancelled() => Err(UsageDataError::new(UsageErrorKind::Cancelled, "用量读取已取消")),
            result = run => result.map_err(read_error),
        }
    });
    Box::new(Reader {
        requests,
        task: Some(task),
        cancellation,
    })
}

#[async_trait::async_trait]
impl UsageReadSession for Reader {
    async fn next_batch(&mut self) -> UsageDataResult<UsageReadBatch> {
        let (reply, result) = oneshot::channel();
        if self.requests.send(reply).await.is_ok()
            && let Ok(batch) = result.await
        {
            return Ok(batch);
        }
        let Some(task) = self.task.take() else {
            return Err(UsageDataError::new(
                UsageErrorKind::NotFound,
                "用量读取已结束",
            ));
        };
        task.await
            .map_err(|_| UsageDataError::unavailable("用量读取任务已中断"))??;
        Err(UsageDataError::new(
            UsageErrorKind::NotFound,
            "用量读取已结束",
        ))
    }

    fn is_finished(&self) -> bool {
        self.task
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished)
    }

    async fn close(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

async fn open_cursor<'a>(
    connection: &'a mut ObservabilityConnection,
    query: &UsageReadQuery,
) -> StoreResult<(Transaction<'a, Postgres>, UsageReadObservation)> {
    let (mut tx, observation) = match query {
        UsageReadQuery::Records(request) => {
            let mut tx = super::super::observability::begin_snapshot(connection).await?;
            scan::declare(&mut tx, request).await?;
            let generated_at: DateTime<Utc> = sqlx::query_scalar("select transaction_timestamp()")
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
            (
                tx,
                UsageReadObservation {
                    generated_at_ms: generated_at.timestamp_millis(),
                    analytics: None,
                    total_groups: None,
                },
            )
        }
        UsageReadQuery::Statistics(request) => {
            let mut snapshot =
                super::super::observability::AnalyticsSnapshot::new(connection).await?;
            super::super::observability::declare_usage_statistics(&mut snapshot, request).await?;
            let metadata = snapshot.metadata();
            let observation = UsageReadObservation {
                generated_at_ms: metadata.generated_at.timestamp_millis(),
                analytics: Some(UsageReadAnalytics {
                    coverage_start_ms: metadata.coverage_start.map(|time| time.timestamp_millis()),
                    coverage_end_ms: metadata.coverage_end.map(|time| time.timestamp_millis()),
                    lag_seconds: metadata.lag_seconds,
                    state: metadata.state.as_str().to_owned(),
                    generation: metadata.generation,
                }),
                total_groups: None,
            };
            (snapshot.tx, observation)
        }
    };
    sqlx::query("set transaction read only")
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    Ok((tx, observation))
}

async fn drive(
    connection: &mut ObservabilityConnection,
    query: UsageReadQuery,
    mut requests: mpsc::Receiver<BatchReply>,
) -> StoreResult<()> {
    let (mut tx, mut observation) = open_cursor(connection, &query).await?;
    // 游标行流始终由本任务持有，等待下一批不会重建查询或丢弃半途读取
    let (reply, batch) = {
        // 游标名称复用而结果列随查询改变，FETCH 每次取得当前列描述
        let mut rows = sqlx::query("fetch all from usage_read")
            .persistent(false)
            .fetch(&mut *tx);
        let mut pending = None;
        loop {
            let Some(reply) = requests.recv().await else {
                return Ok(());
            };
            let mut batch = UsageReadBatch {
                observation: observation.clone(),
                items: Vec::new(),
                complete: false,
            };
            let mut bytes = 0;
            while batch.items.len() < usize::from(query.batch_size()) {
                let item = match pending.take() {
                    Some(item) => item,
                    None => {
                        let Some(row) = rows.try_next().await.map_err(db_error)? else {
                            batch.complete = true;
                            break;
                        };
                        decode(row, &query, &mut observation)?
                    }
                };
                let size = serde_json::to_vec(&item)
                    .map_err(|_| invalid("encode usage read item"))?
                    .len()
                    + 1;
                if batch.items.is_empty() {
                    batch.observation = observation.clone();
                    bytes = serde_json::to_vec(&batch)
                        .map_err(|_| invalid("encode usage batch"))?
                        .len();
                }
                if bytes + size > USAGE_READ_MAX_BATCH_BYTES {
                    if batch.items.is_empty() {
                        return Err(invalid("usage read item exceeds batch capacity")
                            .with_source(ReadCapacity));
                    }
                    pending = Some(item);
                    break;
                }
                bytes += size;
                batch.items.push(item);
            }
            batch.observation = observation.clone();
            if batch.complete {
                break (reply, batch);
            }
            if reply.send(batch).is_err() {
                return Ok(());
            }
        }
    };
    // 完整结束只在数据库事务完成后发布，提交失败不能表现为正常 EOF
    tx.commit().await.map_err(db_error)?;
    let _ = reply.send(batch);
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("usage read item exceeds batch capacity")]
struct ReadCapacity;

fn read_error(error: StoreError) -> UsageDataError {
    if let StoreError::InvalidData {
        source: Some(source),
        ..
    } = &error
        && source.is::<ReadCapacity>()
    {
        return UsageDataError::new(UsageErrorKind::CapacityExceeded, "单条用量结果超过批次容量");
    }
    if let StoreError::Unavailable {
        source: Some(source),
        ..
    } = &error
    {
        let sql_timeout = source
            .downcast_ref::<sqlx::Error>()
            .and_then(sqlx::Error::as_database_error)
            .and_then(|error| error.code())
            .is_some_and(|code| code == "57014");
        if source.is::<tokio::time::error::Elapsed>() || sql_timeout {
            return UsageDataError::new(UsageErrorKind::Timeout, "用量读取超过执行期限");
        }
    }
    public_error(error)
}

fn decode(
    row: PgRow,
    query: &UsageReadQuery,
    observation: &mut UsageReadObservation,
) -> StoreResult<UsageReadItem> {
    match query {
        UsageReadQuery::Records(_) => {
            scan::decode(row).map(|record| UsageReadItem::Record(Box::new(record)))
        }
        UsageReadQuery::Statistics(_) => {
            let groups: i64 = row.try_get("total_groups").map_err(db_error)?;
            observation.total_groups = Some(UsageCount(
                u64::try_from(groups).map_err(|_| invalid("negative usage group count"))?,
            ));
            super::super::observability::decode_usage_statistics(row)
                .map(|statistics| UsageReadItem::Statistics(Box::new(statistics)))
        }
    }
}
