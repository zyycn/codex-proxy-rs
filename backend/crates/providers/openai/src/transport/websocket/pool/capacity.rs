//! 同一出站代理配置共享物理 WebSocket 预算，仅在容量不足时回收普通空闲连接

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use gateway_core::account::OutboundProxy;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

use super::state::{WebSocketPoolSlot, close_pooled_connection};
use super::{CodexWebSocketPool, CodexWebSocketPoolKey};
use crate::transport::websocket::CodexWebSocketExchangeError;

/// 代理容量按完整连接配置合计，关联的多个上游账号共享名额，不混合不同代理认证
pub(crate) fn proxy_capacity_key(proxy: Option<&OutboundProxy>) -> String {
    proxy.map_or_else(String::new, |proxy| {
        hex::encode(Sha256::digest(proxy.expose_url().as_bytes()))
    })
}

pub(super) struct ProxyCapacity {
    connections: AtomicUsize,
    changed: Arc<Notify>,
}

/// 由建连任务移交给实际 socket，取消、断开和关闭均在析构时归还容量
pub(crate) struct ProxyConnectionPermit(Arc<ProxyCapacity>);

impl Drop for ProxyConnectionPermit {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }
}

impl CodexWebSocketPool {
    fn proxy_capacity(&self, proxy_key: &str) -> Arc<ProxyCapacity> {
        let mut capacities = self
            .proxy_capacities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(capacity) = capacities.get(proxy_key).and_then(std::sync::Weak::upgrade) {
            return capacity;
        }
        // socket 和等待者共同持有计数，无存活连接的出口无需永久保留注册项
        capacities.retain(|_, capacity| capacity.strong_count() > 0);
        let capacity = Arc::new(ProxyCapacity {
            connections: AtomicUsize::new(0),
            changed: Arc::clone(&self.capacity_changed),
        });
        capacities.insert(proxy_key.to_owned(), Arc::downgrade(&capacity));
        capacity
    }

    pub(crate) async fn acquire_proxy_capacity(
        &self,
        proxy: Option<&OutboundProxy>,
        retry_key: Option<&CodexWebSocketPoolKey>,
    ) -> Result<Option<ProxyConnectionPermit>, CodexWebSocketExchangeError> {
        let Some(proxy) = proxy else {
            return Ok(None);
        };
        let proxy_key = proxy_capacity_key(Some(proxy));
        let capacity = self.proxy_capacity(&proxy_key);
        let limit = proxy.max_websocket_connections() as usize;
        loop {
            let changed = self.capacity_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.shutdown.is_cancelled() {
                return Err(CodexWebSocketExchangeError::SharedConnectFailed);
            }
            // 等待期间同一会话可能已完成建连或归还，先回到现有复用路径
            if retry_key.is_some_and(|key| self.lock_state().slots.contains_key(key)) {
                return Ok(None);
            }
            // 不限额时也跟踪 socket，后续调低或启用预算不会丢失已有连接的占用
            if capacity
                .connections
                .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    if limit != 0 && used >= limit {
                        None
                    } else {
                        used.checked_add(1)
                    }
                })
                .is_ok()
            {
                return Ok(Some(ProxyConnectionPermit(Arc::clone(&capacity))));
            }
            let evicted = {
                let mut state = self.lock_state();
                let oldest = state
                    .slots
                    .iter()
                    .filter_map(|(key, slot)| match slot {
                        WebSocketPoolSlot::Idle { idle_since, .. }
                            if key.proxy_capacity_key == proxy_key
                                && !state.has_downstream_binding(key) =>
                        {
                            Some((key, *idle_since))
                        }
                        _ => None,
                    })
                    .min_by_key(|(_, idle_since)| *idle_since)
                    .map(|(key, _)| key.clone());
                oldest.and_then(|key| {
                    let WebSocketPoolSlot::Idle { connection, .. } = state.slots.remove(&key)?
                    else {
                        return None;
                    };
                    state.remember_continuation_loss(
                        &key,
                        connection.continuation.latest_response_id(),
                        connection
                            .websocket
                            .observation()
                            .with_exit_reason("proxy_capacity_evicted"),
                        tokio::time::Instant::now(),
                    );
                    tracing::info!(
                        proxy_scope_hash = proxy_key,
                        websocket_connection_id = %connection.websocket.connection_id(),
                        "Evicted idle WebSocket for proxy capacity"
                    );
                    Some(*connection)
                })
            };
            if let Some(connection) = evicted {
                // 等待旧 socket 真正关闭后再占用它释放的名额，避免 CONNECT 瞬时超额
                close_pooled_connection(connection).await;
                continue;
            }
            tokio::select! {
                _ = &mut changed => {}
                _ = self.shutdown.cancelled() => return Err(CodexWebSocketExchangeError::SharedConnectFailed),
            }
        }
    }
}
