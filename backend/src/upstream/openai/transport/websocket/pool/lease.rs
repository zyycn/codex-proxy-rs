//! WebSocket 连接池 reservation、handoff 与 lease 生命周期。

use std::time::Duration;

use tokio::{
    sync::{OwnedSemaphorePermit, watch},
    time::Instant,
};
use tokio_util::{
    sync::CancellationToken,
    task::{TaskTracker, task_tracker::TaskTrackerToken},
};
use uuid::Uuid;

use super::state::{CodexWebSocketPoolKey, PooledWebSocketConnection, WebSocketPoolReservation};
use super::{CodexWebSocketPool, WebSocketPoolBypassReason};

pub(crate) enum WebSocketPoolAcquire {
    Reused {
        connection: Box<PooledWebSocketConnection>,
        lease: WebSocketPoolLease,
    },
    Connect(WebSocketPoolConnectLease),
    Wait(WebSocketPoolConnectWaiter),
    Bypass(WebSocketPoolBypassReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebSocketPoolConnectOutcome {
    Pending,
    Ready,
    Failed,
}

pub(crate) struct WebSocketPoolConnectWaiter {
    pub(super) receiver: watch::Receiver<WebSocketPoolConnectOutcome>,
    pub(super) started_at: Instant,
}

impl WebSocketPoolConnectWaiter {
    pub(crate) fn remaining_budget(&self, budget: Duration) -> Duration {
        budget.saturating_sub(self.started_at.elapsed())
    }

    pub(crate) async fn wait(mut self) -> WebSocketPoolConnectOutcome {
        loop {
            let outcome = *self.receiver.borrow_and_update();
            if outcome != WebSocketPoolConnectOutcome::Pending {
                return outcome;
            }
            if self.receiver.changed().await.is_err() {
                return WebSocketPoolConnectOutcome::Failed;
            }
        }
    }
}

pub(crate) struct WebSocketPoolConnectLease {
    pool: CodexWebSocketPool,
    key: CodexWebSocketPoolKey,
    pub(super) id: Uuid,
    pub(super) started_at: Instant,
    pub(super) outcome: watch::Sender<WebSocketPoolConnectOutcome>,
    pub(super) cancellation: CancellationToken,
    // slot 分配时即注册，封闭 acquire 与后台 task spawn 之间的 shutdown 竞态。
    _task_registration: TaskTrackerToken,
    _connect_permit: OwnedSemaphorePermit,
    armed: bool,
}

impl WebSocketPoolConnectLease {
    pub(super) fn reserve(
        pool: CodexWebSocketPool,
        key: CodexWebSocketPoolKey,
        connect_permit: OwnedSemaphorePermit,
    ) -> Self {
        let (outcome, _) = watch::channel(WebSocketPoolConnectOutcome::Pending);
        let cancellation = pool.shutdown.child_token();
        let task_registration = pool.tasks.token();
        Self {
            pool,
            key,
            id: Uuid::new_v4(),
            started_at: Instant::now(),
            outcome,
            cancellation,
            _task_registration: task_registration,
            _connect_permit: connect_permit,
            armed: true,
        }
    }

    pub(crate) fn started_at(&self) -> Instant {
        self.started_at
    }

    pub(crate) fn key(&self) -> &CodexWebSocketPoolKey {
        &self.key
    }

    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub(crate) async fn connected_reserved(
        mut self,
        connection: PooledWebSocketConnection,
    ) -> Result<(Box<PooledWebSocketConnection>, WebSocketPoolLease), Box<PooledWebSocketConnection>>
    {
        let result = self
            .pool
            .finish_connect_reserved(&self.key, self.id, connection)
            .await;
        let outcome = if result.is_ok() {
            WebSocketPoolConnectOutcome::Ready
        } else {
            WebSocketPoolConnectOutcome::Failed
        };
        self.outcome.send_replace(outcome);
        self.armed = false;
        result
    }

    pub(crate) async fn failed(mut self) {
        self.cancellation.cancel();
        // 先唤醒共享等待者，pool mutex 清理不得延迟前台 transport 决策。
        self.outcome
            .send_replace(WebSocketPoolConnectOutcome::Failed);
        self.pool.fail_connect(&self.key, self.id).await;
        self.armed = false;
    }
}

impl Drop for WebSocketPoolConnectLease {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.cancellation.cancel();
        self.outcome
            .send_replace(WebSocketPoolConnectOutcome::Failed);
        self.pool.fail_connect_now(&self.key, self.id);
    }
}

pub(crate) struct WebSocketPoolLease {
    pool: CodexWebSocketPool,
    key: CodexWebSocketPoolKey,
    pub(super) reservation: WebSocketPoolReservation,
    _task_registration: TaskTrackerToken,
    armed: bool,
}

impl WebSocketPoolLease {
    pub(super) fn reserve(
        pool: CodexWebSocketPool,
        key: CodexWebSocketPoolKey,
        latest_response_id: Option<&str>,
    ) -> Self {
        let task_registration = pool.tasks.token();
        Self {
            pool,
            key,
            reservation: WebSocketPoolReservation {
                id: Uuid::new_v4(),
                reserved_at: Instant::now(),
                latest_response_id: latest_response_id.map(str::to_string),
            },
            _task_registration: task_registration,
            armed: true,
        }
    }

    pub(crate) fn stream_task_context(&self) -> (TaskTracker, CancellationToken) {
        (self.pool.tasks.clone(), self.pool.shutdown.clone())
    }

    pub(crate) async fn put(mut self, connection: PooledWebSocketConnection) {
        self.pool
            .put_reserved(&self.key, self.reservation.id, connection)
            .await;
        self.armed = false;
    }

    pub(crate) async fn discard(mut self) {
        self.pool
            .discard_reserved(&self.key, self.reservation.id)
            .await;
        self.armed = false;
    }
}

impl Drop for WebSocketPoolLease {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.pool
            .discard_reserved_now(&self.key, self.reservation.id);
    }
}
