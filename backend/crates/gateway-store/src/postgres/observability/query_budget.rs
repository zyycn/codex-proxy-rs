//! 管理观测查询的 PostgreSQL 连接槽位预算。

use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{Stream, TryStreamExt, stream::BoxStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{StoreBackend, StoreError, StoreResult};

/// 所有管理观测入口共享的 PostgreSQL 连接槽位预算。
#[derive(Clone)]
pub struct ObservabilityQueryBudget {
    slots: Arc<Semaphore>,
    wait_timeout: Duration,
}

impl ObservabilityQueryBudget {
    /// 预算表示观测类 SQL 最多可同时占用的连接数。
    pub fn try_new(max_connections: u32, wait_timeout: Duration) -> StoreResult<Self> {
        if max_connections == 0 || wait_timeout.is_zero() {
            return Err(StoreError::InvalidData {
                entity: "observability query budget",
                message: "requires a positive connection count and wait timeout".to_owned(),
            });
        }
        let permits = usize::try_from(max_connections).map_err(|_| StoreError::InvalidData {
            entity: "observability query budget",
            message: "connection budget does not fit this platform".to_owned(),
        })?;
        Ok(Self {
            slots: Arc::new(Semaphore::new(permits)),
            wait_timeout,
        })
    }

    /// 在共享管理观测预算内执行一条 PostgreSQL 查询。
    ///
    /// # Errors
    ///
    /// 预算在等待期限内没有空闲槽位、预算已关闭，或查询本身失败时返回错误。
    pub async fn run<T, F>(&self, operation: &'static str, query: F) -> StoreResult<T>
    where
        F: Future<Output = StoreResult<T>>,
    {
        let _slot = self.acquire(operation).await?;
        query.await
    }

    /// 将槽位持有到查询流消费结束、报错或被丢弃，而不是仅持有到流创建完成。
    pub fn run_stream<'a, T, S>(
        &'a self,
        operation: &'static str,
        query: S,
    ) -> BoxStream<'a, StoreResult<T>>
    where
        T: Send + 'a,
        S: Stream<Item = StoreResult<T>> + Send + 'a,
    {
        Box::pin(async_stream::stream! {
            let mut query = Box::pin(query);
            let slot = match self.acquire(operation).await {
                Ok(slot) => slot,
                Err(error) => {
                    drop(query);
                    yield Err(error);
                    return;
                }
            };
            loop {
                match query.try_next().await {
                    Ok(Some(item)) => yield Ok(item),
                    Ok(None) => break,
                    Err(error) => {
                        // 返回错误后调用方可能不再 poll，但仍保留流对象。
                        // 必须在交付错误之前释放数据库流和预算。
                        drop(query);
                        drop(slot);
                        yield Err(error);
                        return;
                    }
                }
            }
        })
    }

    async fn acquire(&self, operation: &'static str) -> StoreResult<OwnedSemaphorePermit> {
        let started_at = Instant::now();
        let acquisition = Arc::clone(&self.slots).acquire_owned();
        match tokio::time::timeout(self.wait_timeout, acquisition).await {
            Ok(Ok(permit)) => {
                tracing::debug!(
                    query_class = "observability",
                    operation,
                    wait_milliseconds = elapsed_milliseconds(started_at),
                    available_slots = self.slots.available_permits(),
                    "PostgreSQL query budget acquired"
                );
                Ok(permit)
            }
            Ok(Err(_)) => Err(StoreError::Unavailable {
                backend: StoreBackend::PostgreSql,
                message: "observability PostgreSQL connection budget is closed".to_owned(),
            }),
            Err(_) => {
                tracing::warn!(
                    query_class = "observability",
                    operation,
                    wait_milliseconds = elapsed_milliseconds(started_at),
                    available_slots = self.slots.available_permits(),
                    "PostgreSQL query budget exhausted"
                );
                Err(StoreError::Unavailable {
                    backend: StoreBackend::PostgreSql,
                    message: "observability PostgreSQL connection budget is exhausted".to_owned(),
                })
            }
        }
    }
}

fn elapsed_milliseconds(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}
