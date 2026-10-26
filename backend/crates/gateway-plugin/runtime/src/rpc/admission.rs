//! 区分插件执行名额与初次准入队列，恢复中的调用沿用在途资源预算

use std::{sync::Arc, time::Duration};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::RpcError;

pub(super) struct ExecutionCapacity {
    active: Arc<Semaphore>,
    queued: Arc<Semaphore>,
    wait_timeout: Duration,
}

impl ExecutionCapacity {
    pub(super) fn new(active: usize, queued: usize, wait_timeout: Duration) -> Self {
        Self {
            active: Arc::new(Semaphore::new(active)),
            queued: Arc::new(Semaphore::new(queued)),
            wait_timeout,
        }
    }

    pub(super) async fn enter(&self) -> Result<OwnedSemaphorePermit, RpcError> {
        if let Ok(permit) = self.active.clone().try_acquire_owned() {
            return Ok(permit);
        }
        let _queued = self.queued.try_acquire().map_err(|error| {
            if matches!(error, tokio::sync::TryAcquireError::Closed) {
                RpcError::Closed(None)
            } else {
                RpcError::Overloaded
            }
        })?;
        self.resume().await
    }

    /// 恢复的等待者已持有在途名额，不再竞争初次准入队列
    pub(super) async fn resume(&self) -> Result<OwnedSemaphorePermit, RpcError> {
        tokio::time::timeout(self.wait_timeout, self.active.clone().acquire_owned())
            .await
            .map_err(|_| RpcError::Overloaded)?
            .map_err(|_| RpcError::Closed(None))
    }

    pub(super) fn close(&self) {
        self.active.close();
        self.queued.close();
    }

    pub(super) fn available(&self) -> (usize, usize) {
        (
            self.active.available_permits(),
            self.queued.available_permits(),
        )
    }
}
