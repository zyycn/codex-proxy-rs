//! WebSocket 连接池维护、驱逐与关闭监督。

use std::sync::{
    Arc,
    atomic::{Ordering, Ordering::AcqRel},
};

use tokio::time::Instant;

use super::CodexWebSocketPool;
use super::lease::WebSocketPoolConnectOutcome;
use super::state::{
    PooledWebSocketConnection, WebSocketPoolSlot, close_pooled_connections,
    should_close_idle_connection,
};

impl CodexWebSocketPool {
    /// 驱逐指定账号的全部 slot，取消 opening，并阻止 busy 连接回收到池中。
    pub async fn evict_account(&self, account_id: &str) {
        let mut idle_connections = Vec::new();
        {
            let mut state = self.lock_state();
            let keys = state
                .slots
                .keys()
                .filter(|key| key.account_id() == account_id)
                .cloned()
                .collect::<Vec<_>>();
            for key in keys {
                match state.slots.remove(&key) {
                    Some(WebSocketPoolSlot::Idle { connection, .. }) => {
                        idle_connections.push(*connection);
                    }
                    Some(WebSocketPoolSlot::Connecting(connecting)) => {
                        connecting.cancellation.cancel();
                        connecting
                            .outcome
                            .send_replace(WebSocketPoolConnectOutcome::Failed);
                    }
                    Some(WebSocketPoolSlot::Busy(_)) | None => {}
                }
            }
        }
        close_pooled_connections(idle_connections).await;
    }

    /// 关闭连接池，取消受管任务、关闭 idle 连接，并让后续 acquire 直接绕过池。
    pub async fn shutdown(&self) {
        self.tasks.close();
        self.shutdown.cancel();
        let idle_connections = {
            let mut state = self.lock_state();
            state.shutting_down = true;
            state
                .slots
                .drain()
                .filter_map(|(_, slot)| match slot {
                    WebSocketPoolSlot::Idle { connection, .. } => Some(*connection),
                    WebSocketPoolSlot::Connecting(connecting) => {
                        connecting.cancellation.cancel();
                        connecting
                            .outcome
                            .send_replace(WebSocketPoolConnectOutcome::Failed);
                        None
                    }
                    WebSocketPoolSlot::Busy(_) => None,
                })
                .collect::<Vec<_>>()
        };
        close_pooled_connections(idle_connections).await;
        self.tasks.wait().await;
    }

    /// 维护池 slot：清扫已死亡或超龄的 idle 连接，以及异常残留的 Busy reservation。
    ///
    /// 保活（ping/pong）与失活检测已下沉到每条连接的 pump 任务内部，此处不再做
    /// 同步 ping 探活，只负责把「后台已标记 closed」或「超过 max_age」的 idle
    /// 连接从池中摘除并关闭，避免它们占用 slot。
    pub async fn maintain_idle_connections(&self) {
        let close = self.take_expired_slots().await;
        close_pooled_connections(close).await;
    }

    pub(super) fn spawn_maintenance_task(&self) {
        let Some(interval_duration) = self.config.maintenance_interval else {
            return;
        };
        if interval_duration.is_zero() {
            if !self.maintenance_started.swap(true, AcqRel) {
                tracing::warn!("Disabled WebSocket pool maintenance with zero interval");
            }
            return;
        }
        if self.shutdown.is_cancelled() {
            return;
        }
        let Ok(_handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self
            .maintenance_started
            .compare_exchange(false, true, AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let inner = Arc::downgrade(&self.inner);
        let config = self.config;
        let tasks = self.tasks.clone();
        let shutdown = self.shutdown.clone();
        let connect_semaphore = Arc::clone(&self.connect_semaphore);
        let maintenance_started = Arc::clone(&self.maintenance_started);
        drop(self.tasks.spawn(async move {
            let mut interval = tokio::time::interval(interval_duration);
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = interval.tick() => {}
                }
                let Some(inner) = inner.upgrade() else {
                    break;
                };
                let pool = CodexWebSocketPool {
                    inner,
                    config,
                    tasks: tasks.clone(),
                    shutdown: shutdown.clone(),
                    connect_semaphore: Arc::clone(&connect_semaphore),
                    maintenance_started: Arc::clone(&maintenance_started),
                };
                if pool.is_shutdown().await {
                    break;
                }
                pool.maintain_idle_connections().await;
            }
        }));
    }

    /// 返回连接池是否已进入关闭状态。
    pub async fn is_shutdown(&self) -> bool {
        self.lock_state().shutting_down
    }

    async fn take_expired_slots(&self) -> Vec<PooledWebSocketConnection> {
        let mut close = Vec::new();
        let now = Instant::now();
        let mut state = self.lock_state();
        if state.shutting_down || !self.config.enabled {
            return close;
        }
        let keys = state
            .slots
            .iter()
            .filter_map(|(key, slot)| match slot {
                WebSocketPoolSlot::Idle { connection, .. }
                    if should_close_idle_connection(connection, now, self.config.max_age) =>
                {
                    Some(key.clone())
                }
                WebSocketPoolSlot::Busy(reservation)
                    if now.duration_since(reservation.reserved_at) >= self.config.max_age =>
                {
                    Some(key.clone())
                }
                WebSocketPoolSlot::Connecting(connecting)
                    if now.duration_since(connecting.started_at) >= self.config.max_age =>
                {
                    Some(key.clone())
                }
                WebSocketPoolSlot::Idle { .. }
                | WebSocketPoolSlot::Busy(_)
                | WebSocketPoolSlot::Connecting(_) => None,
            })
            .collect::<Vec<_>>();
        for key in keys {
            match state.slots.remove(&key) {
                Some(WebSocketPoolSlot::Idle { connection, .. }) => close.push(*connection),
                Some(WebSocketPoolSlot::Busy(reservation)) => {
                    tracing::warn!(
                        account_id = key.account_id(),
                        conversation_id_hash = key.conversation_id_hash(),
                        reservation_id = %reservation.id,
                        "Removed stale WebSocket pool reservation"
                    );
                }
                Some(WebSocketPoolSlot::Connecting(connecting)) => {
                    connecting.cancellation.cancel();
                    connecting
                        .outcome
                        .send_replace(WebSocketPoolConnectOutcome::Failed);
                    tracing::warn!(
                        account_id = key.account_id(),
                        conversation_id_hash = key.conversation_id_hash(),
                        connect_id = %connecting.id,
                        "Removed stale WebSocket pool connection attempt"
                    );
                }
                None => {}
            }
        }
        close
    }
}
