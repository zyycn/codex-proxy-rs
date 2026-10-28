//! 下游连接的取消监督与池内归属转移

use std::sync::Arc;

use gateway_core::lifecycle::CancellationToken;
use tokio::time::Instant;

use super::state::{
    PooledWebSocketConnection, WebSocketPoolSlot, WebSocketPoolState, close_pooled_connections,
};
use super::{CodexWebSocketPool, CodexWebSocketPoolKey, WebSocketPoolConnectOutcome};

impl CodexWebSocketPool {
    pub(super) fn register_downstream(
        &self,
        key: &CodexWebSocketPoolKey,
        cancellation: &CancellationToken,
    ) {
        if key.downstream_connection_id.is_empty() {
            return;
        }
        let connection_id = key.downstream_connection_id.clone();
        {
            let mut state = self.lock_state();
            if state.shutting_down || state.downstream_connections.contains_key(&connection_id) {
                return;
            }
            state
                .downstream_connections
                .insert(connection_id.clone(), cancellation.clone());
        }
        let inner = Arc::downgrade(&self.inner);
        let cancellation = cancellation.clone();
        let shutdown = self.shutdown.clone();
        let capacity_changed = Arc::clone(&self.capacity_changed);
        drop(self.tasks.spawn(async move {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return,
                () = cancellation.cancelled() => {}
            }
            let Some(inner) = inner.upgrade() else { return };
            let close = {
                let mut state = inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let close = state.take_downstream_connections(
                    &connection_id,
                    None,
                    "downstream_connection_closed",
                );
                state.downstream_connections.remove(&connection_id);
                close
            };
            close_pooled_connections(close).await;
            capacity_changed.notify_waiters();
        }));
    }

    /// 实际切到 HTTP 或无池连接时撤销旧 WS 绑定，保留客户端本身的生命周期监督
    pub(crate) async fn release_downstream_connections(&self, connection_id: &str) {
        let close = self.lock_state().take_downstream_connections(
            connection_id,
            None,
            "downstream_rebound",
        );
        close_pooled_connections(close).await;
        self.capacity_changed.notify_waiters();
    }

    pub(super) fn close_retired_connections(&self, connections: Vec<PooledWebSocketConnection>) {
        if connections.is_empty() {
            return;
        }
        let capacity_changed = Arc::clone(&self.capacity_changed);
        drop(self.tasks.spawn(async move {
            close_pooled_connections(connections).await;
            capacity_changed.notify_waiters();
        }));
    }
}

impl WebSocketPoolState {
    pub(super) fn has_downstream_binding(&self, key: &CodexWebSocketPoolKey) -> bool {
        self.downstream_connections
            .get(&key.downstream_connection_id)
            .is_some_and(|token| !token.is_cancelled())
    }

    pub(super) fn downstream_closed(&self, key: &CodexWebSocketPoolKey) -> bool {
        self.downstream_connections
            .get(&key.downstream_connection_id)
            .is_some_and(CancellationToken::is_cancelled)
    }

    pub(super) fn take_downstream_connections(
        &mut self,
        connection_id: &str,
        keep: Option<&CodexWebSocketPoolKey>,
        reason: &'static str,
    ) -> Vec<PooledWebSocketConnection> {
        if connection_id.is_empty() {
            return Vec::new();
        }
        let keys = self
            .slots
            .keys()
            .filter(|key| key.downstream_connection_id == connection_id && Some(*key) != keep)
            .cloned()
            .collect::<Vec<_>>();
        let mut close = Vec::new();
        for key in keys {
            match self.slots.remove(&key) {
                Some(WebSocketPoolSlot::Idle { connection, .. }) => {
                    self.remember_continuation_loss(
                        &key,
                        connection.continuation.latest_response_id(),
                        connection.websocket.observation().with_exit_reason(reason),
                        Instant::now(),
                    );
                    close.push(*connection);
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
        close
    }
}
