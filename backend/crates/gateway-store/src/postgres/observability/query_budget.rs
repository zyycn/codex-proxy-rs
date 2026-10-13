//! 管理观测查询的 PostgreSQL 连接槽位预算

use std::{
    future::Future,
    ops::{Deref, DerefMut},
    sync::Arc,
    time::{Duration, Instant},
};

use sqlx::{PgConnection, PgPool, Postgres, pool::PoolConnection};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{StoreBackend, StoreError, StoreResult, postgres_unavailable};

/// 所有管理观测入口共享的 PostgreSQL 连接槽位预算
#[derive(Clone)]
pub struct ObservabilityQueryBudget {
    slots: Arc<Semaphore>,
    wait_timeout: Duration,
    query_timeout: Duration,
}

/// 观测名额与实际 PostgreSQL 连接共同存活，取消后仍覆盖协议收尾和事务回滚
pub struct ObservabilityConnection {
    connection: PoolConnection<Postgres>,
    permit: Option<OwnedSemaphorePermit>,
    query_timeout: Duration,
}

impl ObservabilityConnection {
    pub(crate) const fn query_timeout(&self) -> Duration {
        self.query_timeout
    }
}

impl Deref for ObservabilityConnection {
    type Target = PgConnection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl DerefMut for ObservabilityConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

impl Drop for ObservabilityConnection {
    fn drop(&mut self) {
        let returning = self.connection.return_to_pool();
        let permit = self.permit.take();
        // SQLx 在确认旧语句结束并刷新回滚后才归还连接，名额不能早于这个边界释放
        tokio::spawn(async move {
            returning.await;
            drop(permit);
        });
    }
}

impl ObservabilityQueryBudget {
    /// 预算表示观测类 SQL 最多可同时占用的连接数
    pub fn try_new(
        max_connections: u32,
        wait_timeout: Duration,
        query_timeout: Duration,
    ) -> StoreResult<Self> {
        if max_connections == 0
            || wait_timeout.is_zero()
            || query_timeout.is_zero()
            || query_timeout.as_millis() > i32::MAX as u128
        {
            return Err(StoreError::InvalidData {
                source: None,
                entity: "observability query budget",
                message:
                    "requires positive connection and timeout budgets within PostgreSQL limits"
                        .to_owned(),
            });
        }
        let permits = usize::try_from(max_connections).map_err(|_| StoreError::InvalidData {
            source: None,
            entity: "observability query budget",
            message: "connection budget does not fit this platform".to_owned(),
        })?;
        Ok(Self {
            slots: Arc::new(Semaphore::new(permits)),
            wait_timeout,
            query_timeout,
        })
    }

    #[must_use]
    pub const fn query_timeout(&self) -> Duration {
        self.query_timeout
    }

    /// 在共享槽位与观测总预算内完成排队、连接获取和整个存储操作
    ///
    /// # Errors
    ///
    /// 预算在等待期限内没有空闲槽位、预算已关闭，或查询本身失败时返回错误
    pub async fn run<T, F, Q>(
        &self,
        pool: &PgPool,
        operation: &'static str,
        query: F,
    ) -> StoreResult<T>
    where
        F: FnOnce(ObservabilityConnection) -> Q,
        Q: Future<Output = StoreResult<T>>,
    {
        tokio::time::timeout(self.query_timeout, async {
            let permit = self.acquire(operation).await?;
            let connection = pool.acquire().await.map_err(|source| {
                postgres_unavailable("acquire observability connection", source)
            })?;
            query(ObservabilityConnection {
                connection,
                permit: Some(permit),
                query_timeout: self.query_timeout,
            })
            .await
        })
        .await
        .map_err(|source| StoreError::Unavailable {
            backend: StoreBackend::PostgreSql,
            message: "observability PostgreSQL execution budget is exhausted".to_owned(),
            source: Some(gateway_core::error::ErrorSource::new(source)),
        })?
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
            Ok(Err(source)) => Err(StoreError::Unavailable {
                backend: StoreBackend::PostgreSql,
                message: "observability PostgreSQL connection budget is closed".to_owned(),
                source: Some(gateway_core::error::ErrorSource::new(source)),
            }),
            Err(source) => {
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
                    source: Some(gateway_core::error::ErrorSource::new(source)),
                })
            }
        }
    }
}

fn elapsed_milliseconds(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}
