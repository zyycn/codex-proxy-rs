//! 按所属会话保留、复用与回收插件受管上游连接

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use gateway_host::outbound::ManagedWebSocket;
use gateway_plugin_sdk::{ErrorCode, PluginFault};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ConnectionOwner {
    pub client_key_id: String,
    pub account_id: String,
    pub credential_revision: u64,
}

/// 每个适配器代次独立持有连接；取出即独占，失败或取消不会归还半完成会话
#[derive(Default)]
pub(crate) struct ConnectionPool {
    idle: Mutex<BTreeMap<String, IdleConnection>>,
}

struct IdleConnection {
    owner: ConnectionOwner,
    socket: Option<(String, ManagedWebSocket)>,
    expires: tokio::task::JoinHandle<()>,
}

impl Drop for IdleConnection {
    fn drop(&mut self) {
        self.expires.abort();
    }
}

impl ConnectionPool {
    pub(crate) fn take(
        &self,
        id: &str,
        owner: &ConnectionOwner,
    ) -> Result<(String, ManagedWebSocket), PluginFault> {
        let mut idle = self
            .idle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !idle
            .get(id)
            .is_some_and(|connection| connection.owner == *owner)
        {
            return Err(PluginFault::new(
                ErrorCode::Rejected,
                "upstream continuation connection is unavailable",
            ));
        }
        idle.remove(id)
            .and_then(|mut connection| connection.socket.take())
            .ok_or_else(|| {
                PluginFault::new(
                    ErrorCode::Rejected,
                    "upstream continuation connection is unavailable",
                )
            })
    }

    pub(crate) fn put(
        self: &Arc<Self>,
        owner: ConnectionOwner,
        socket: (String, ManagedWebSocket),
    ) -> Result<String, PluginFault> {
        let mut idle = self
            .idle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if idle.len() >= 64 {
            return Err(PluginFault::new(
                ErrorCode::Capacity,
                "upstream continuation capacity exhausted",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let weak = Arc::downgrade(self);
        let expired_id = id.clone();
        let expires = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(30 * 60)).await;
            if let Some(pool) = weak.upgrade() {
                pool.idle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&expired_id);
            }
        });
        idle.insert(
            id.clone(),
            IdleConnection {
                owner,
                socket: Some(socket),
                expires,
            },
        );
        Ok(id)
    }
}
