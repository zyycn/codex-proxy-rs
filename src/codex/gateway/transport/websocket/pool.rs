use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{SinkExt, StreamExt};
use tokio::{net::TcpStream, sync::Mutex, time::timeout};
use tokio_tungstenite::{
    tungstenite::{http::HeaderMap as WsHeaderMap, Message},
    MaybeTlsStream, WebSocketStream,
};

use super::codec::{rate_limit_headers, set_cookie_headers, turn_state};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CodexWebSocketPoolKey {
    base_url: String,
    account_id: String,
    conversation_id: String,
}

impl CodexWebSocketPoolKey {
    pub fn new(
        base_url: impl Into<String>,
        account_id: impl Into<String>,
        conversation_id: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            account_id: account_id.into(),
            conversation_id: conversation_id.into(),
        }
    }

    fn account_id(&self) -> &str {
        &self.account_id
    }
}

#[derive(Clone)]
pub struct CodexWebSocketPool {
    inner: Arc<Mutex<WebSocketPoolState>>,
    config: CodexWebSocketPoolConfig,
}

#[derive(Debug, Clone, Copy)]
pub struct CodexWebSocketPoolConfig {
    pub enabled: bool,
    pub max_age: Duration,
    pub max_per_account: usize,
    pub maintenance_interval: Option<Duration>,
    pub ping_interval: Option<Duration>,
    pub ping_timeout: Duration,
    pub liveness_timeout: Option<Duration>,
}

impl CodexWebSocketPool {
    const DEFAULT_MAX_AGE: Duration = Duration::from_secs(55 * 60);
    const DEFAULT_MAX_PER_ACCOUNT: usize = 8;
    const DEFAULT_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(25);
    const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(25);
    const DEFAULT_PING_TIMEOUT: Duration = Duration::from_secs(5);
    const DEFAULT_LIVENESS_TIMEOUT: Duration = Duration::from_millis(62_500);

    pub fn with_default_max_age() -> Self {
        Self::with_config(CodexWebSocketPoolConfig::default())
    }

    pub fn with_limits(max_age: Duration, max_per_account: usize) -> Self {
        Self::with_config(CodexWebSocketPoolConfig {
            max_age,
            max_per_account,
            maintenance_interval: None,
            ping_interval: None,
            liveness_timeout: None,
            ..CodexWebSocketPoolConfig::default()
        })
    }

    pub fn with_config(config: CodexWebSocketPoolConfig) -> Self {
        let pool = Self {
            inner: Arc::new(Mutex::new(WebSocketPoolState::default())),
            config,
        };
        pool.spawn_maintenance_task();
        pool
    }

    pub async fn evict_account(&self, account_id: &str) {
        let mut idle_connections = Vec::new();
        {
            let mut state = self.inner.lock().await;
            let keys = state
                .slots
                .keys()
                .filter(|key| key.account_id() == account_id)
                .cloned()
                .collect::<Vec<_>>();
            for key in keys {
                if let Some(WebSocketPoolSlot::Idle(connection)) = state.slots.remove(&key) {
                    idle_connections.push(*connection);
                }
            }
        }
        close_idle_connections(idle_connections).await;
    }

    pub async fn shutdown(&self) {
        let idle_connections = {
            let mut state = self.inner.lock().await;
            state.shutting_down = true;
            state
                .slots
                .drain()
                .filter_map(|(_, slot)| match slot {
                    WebSocketPoolSlot::Idle(connection) => Some(*connection),
                    WebSocketPoolSlot::Busy | WebSocketPoolSlot::Checking => None,
                })
                .collect::<Vec<_>>()
        };
        close_idle_connections(idle_connections).await;
    }

    pub async fn gc_sweep(&self) {
        self.maintain_idle_connections().await;
    }

    pub(super) async fn acquire(&self, key: &CodexWebSocketPoolKey) -> WebSocketPoolAcquire {
        let mut expired_connection = None;
        let acquire = {
            let mut state = self.inner.lock().await;
            if !self.config.enabled || state.shutting_down {
                return WebSocketPoolAcquire::Bypass;
            }
            match state.slots.get(key) {
                Some(WebSocketPoolSlot::Busy | WebSocketPoolSlot::Checking) => {
                    return WebSocketPoolAcquire::Bypass;
                }
                Some(WebSocketPoolSlot::Idle(_)) => {
                    let Some(WebSocketPoolSlot::Idle(connection)) = state.slots.remove(key) else {
                        return WebSocketPoolAcquire::Bypass;
                    };
                    if connection.created_at.elapsed() <= self.config.max_age {
                        state.slots.insert(key.clone(), WebSocketPoolSlot::Busy);
                        return WebSocketPoolAcquire::Reused(connection);
                    }
                    expired_connection = Some(*connection);
                }
                None => {}
            }

            if self.config.max_per_account == 0
                || account_slot_count(&state.slots, key.account_id()) >= self.config.max_per_account
            {
                WebSocketPoolAcquire::Bypass
            } else {
                state.slots.insert(key.clone(), WebSocketPoolSlot::Busy);
                WebSocketPoolAcquire::FreshReserved
            }
        };

        if let Some(connection) = expired_connection {
            close_idle_connections(vec![connection]).await;
        }

        acquire
    }

    pub(super) async fn put(
        &self,
        key: CodexWebSocketPoolKey,
        connection: PooledWebSocketConnection,
    ) {
        let mut connection = Some(connection);
        {
            let mut state = self.inner.lock().await;
            let expired = connection
                .as_ref()
                .is_some_and(|connection| connection.created_at.elapsed() > self.config.max_age);
            if expired || state.shutting_down || !self.config.enabled {
                state.slots.remove(&key);
            } else if matches!(state.slots.get(&key), Some(WebSocketPoolSlot::Busy)) {
                if let Some(connection) = connection.take() {
                    state
                        .slots
                        .insert(key, WebSocketPoolSlot::Idle(Box::new(connection)));
                }
            }
        }
        if let Some(connection) = connection {
            close_pooled_connection(connection).await;
        }
    }

    pub(super) async fn discard(&self, key: &CodexWebSocketPoolKey) {
        let idle_connection = {
            let mut state = self.inner.lock().await;
            match state.slots.remove(key) {
                Some(WebSocketPoolSlot::Idle(connection)) => Some(*connection),
                Some(WebSocketPoolSlot::Busy | WebSocketPoolSlot::Checking) | None => None,
            }
        };
        if let Some(connection) = idle_connection {
            close_idle_connections(vec![connection]).await;
        }
    }

    fn spawn_maintenance_task(&self) {
        let Some(interval_duration) = self.config.maintenance_interval else {
            return;
        };
        let inner = Arc::downgrade(&self.inner);
        let config = self.config;
        let Ok(_handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(interval_duration);
            loop {
                interval.tick().await;
                let Some(inner) = inner.upgrade() else {
                    break;
                };
                let pool = CodexWebSocketPool { inner, config };
                if pool.is_shutting_down().await {
                    break;
                }
                pool.maintain_idle_connections().await;
            }
        });
    }

    async fn is_shutting_down(&self) -> bool {
        self.inner.lock().await.shutting_down
    }

    pub async fn maintain_idle_connections(&self) {
        let maintenance = self.take_idle_connections_for_maintenance().await;
        close_idle_connections(maintenance.close).await;
        for (key, connection) in maintenance.probe {
            let connection = probe_idle_connection(connection, self.config.ping_timeout).await;
            self.return_maintained_connection(key, connection).await;
        }
    }

    async fn take_idle_connections_for_maintenance(&self) -> WebSocketPoolMaintenance {
        let mut close = Vec::new();
        let mut probe = Vec::new();
        let now = Instant::now();
        let Some(ping_interval) = self.config.ping_interval else {
            let mut state = self.inner.lock().await;
            if state.shutting_down || !self.config.enabled {
                return WebSocketPoolMaintenance { close, probe };
            }
            let expired_keys = expired_idle_keys(&state.slots, self.config.max_age);
            for key in expired_keys {
                if let Some(WebSocketPoolSlot::Idle(connection)) = state.slots.remove(&key) {
                    close.push(*connection);
                }
            }
            return WebSocketPoolMaintenance { close, probe };
        };

        let mut state = self.inner.lock().await;
        if state.shutting_down || !self.config.enabled {
            return WebSocketPoolMaintenance { close, probe };
        }
        let keys = state
            .slots
            .iter()
            .filter_map(|(key, slot)| match slot {
                WebSocketPoolSlot::Idle(connection)
                    if should_close_idle_connection(
                        connection,
                        now,
                        self.config.max_age,
                        self.config.liveness_timeout,
                    ) || should_probe_idle_connection(connection, now, ping_interval) =>
                {
                    Some(key.clone())
                }
                WebSocketPoolSlot::Idle(_)
                | WebSocketPoolSlot::Busy
                | WebSocketPoolSlot::Checking => None,
            })
            .collect::<Vec<_>>();
        for key in keys {
            let Some(WebSocketPoolSlot::Idle(connection)) = state.slots.remove(&key) else {
                continue;
            };
            if should_close_idle_connection(
                &connection,
                now,
                self.config.max_age,
                self.config.liveness_timeout,
            ) {
                close.push(*connection);
            } else {
                state.slots.insert(key.clone(), WebSocketPoolSlot::Checking);
                probe.push((key, *connection));
            }
        }
        WebSocketPoolMaintenance { close, probe }
    }

    async fn return_maintained_connection(
        &self,
        key: CodexWebSocketPoolKey,
        connection: Option<PooledWebSocketConnection>,
    ) {
        let mut connection = connection;
        {
            let mut state = self.inner.lock().await;
            if state.shutting_down || !self.config.enabled {
                state.slots.remove(&key);
            } else if matches!(state.slots.get(&key), Some(WebSocketPoolSlot::Checking)) {
                if let Some(connection) = connection.take() {
                    state
                        .slots
                        .insert(key, WebSocketPoolSlot::Idle(Box::new(connection)));
                } else {
                    state.slots.remove(&key);
                }
            }
        }
        if let Some(connection) = connection {
            close_pooled_connection(connection).await;
        }
    }
}

impl Default for CodexWebSocketPool {
    fn default() -> Self {
        Self::with_default_max_age()
    }
}

impl Default for CodexWebSocketPoolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_age: CodexWebSocketPool::DEFAULT_MAX_AGE,
            max_per_account: CodexWebSocketPool::DEFAULT_MAX_PER_ACCOUNT,
            maintenance_interval: Some(CodexWebSocketPool::DEFAULT_MAINTENANCE_INTERVAL),
            ping_interval: Some(CodexWebSocketPool::DEFAULT_PING_INTERVAL),
            ping_timeout: CodexWebSocketPool::DEFAULT_PING_TIMEOUT,
            liveness_timeout: Some(CodexWebSocketPool::DEFAULT_LIVENESS_TIMEOUT),
        }
    }
}

#[derive(Default)]
struct WebSocketPoolState {
    slots: HashMap<CodexWebSocketPoolKey, WebSocketPoolSlot>,
    shutting_down: bool,
}

pub(super) type CodexWsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CodexWebSocketConnectionMetadata {
    pub(super) turn_state: Option<String>,
    pub(super) set_cookie_headers: Vec<String>,
    pub(super) rate_limit_headers: Vec<(String, String)>,
}

impl CodexWebSocketConnectionMetadata {
    pub(super) fn from_headers(headers: &WsHeaderMap) -> Self {
        Self {
            turn_state: turn_state(headers),
            set_cookie_headers: set_cookie_headers(headers),
            rate_limit_headers: rate_limit_headers(headers),
        }
    }
}

pub(super) struct PooledWebSocketConnection {
    pub(super) websocket: CodexWsStream,
    pub(super) metadata: CodexWebSocketConnectionMetadata,
    pub(super) created_at: Instant,
    pub(super) last_activity_at: Instant,
    pub(super) last_ping_at: Option<Instant>,
}

enum WebSocketPoolSlot {
    Idle(Box<PooledWebSocketConnection>),
    Busy,
    Checking,
}

pub(super) enum WebSocketPoolAcquire {
    Reused(Box<PooledWebSocketConnection>),
    FreshReserved,
    Bypass,
}

fn account_slot_count(
    inner: &HashMap<CodexWebSocketPoolKey, WebSocketPoolSlot>,
    account_id: &str,
) -> usize {
    inner
        .keys()
        .filter(|key| key.account_id() == account_id)
        .count()
}

async fn close_idle_connections(connections: Vec<PooledWebSocketConnection>) {
    for connection in connections {
        close_pooled_connection(connection).await;
    }
}

async fn close_pooled_connection(connection: PooledWebSocketConnection) {
    let mut websocket = connection.websocket;
    let _ = websocket.close(None).await;
}

struct WebSocketPoolMaintenance {
    close: Vec<PooledWebSocketConnection>,
    probe: Vec<(CodexWebSocketPoolKey, PooledWebSocketConnection)>,
}

fn expired_idle_keys(
    slots: &HashMap<CodexWebSocketPoolKey, WebSocketPoolSlot>,
    max_age: Duration,
) -> Vec<CodexWebSocketPoolKey> {
    slots
        .iter()
        .filter_map(|(key, slot)| match slot {
            WebSocketPoolSlot::Idle(connection) if connection.created_at.elapsed() > max_age => {
                Some(key.clone())
            }
            WebSocketPoolSlot::Idle(_) | WebSocketPoolSlot::Busy | WebSocketPoolSlot::Checking => {
                None
            }
        })
        .collect()
}

fn should_close_idle_connection(
    connection: &PooledWebSocketConnection,
    now: Instant,
    max_age: Duration,
    liveness_timeout: Option<Duration>,
) -> bool {
    connection.created_at.elapsed() > max_age
        || liveness_timeout.is_some_and(|timeout| {
            !timeout.is_zero() && now.duration_since(connection.last_activity_at) > timeout
        })
}

fn should_probe_idle_connection(
    connection: &PooledWebSocketConnection,
    now: Instant,
    ping_interval: Duration,
) -> bool {
    !ping_interval.is_zero()
        && connection
            .last_ping_at
            .is_none_or(|last_ping_at| now.duration_since(last_ping_at) >= ping_interval)
}

async fn probe_idle_connection(
    mut connection: PooledWebSocketConnection,
    ping_timeout: Duration,
) -> Option<PooledWebSocketConnection> {
    let now = Instant::now();
    connection.last_ping_at = Some(now);
    if connection
        .websocket
        .send(Message::Ping(Vec::new().into()))
        .await
        .is_err()
    {
        close_pooled_connection(connection).await;
        return None;
    }
    if ping_timeout.is_zero() {
        return Some(connection);
    }
    match timeout(ping_timeout, connection.websocket.next()).await {
        Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) | Err(_) => {
            close_pooled_connection(connection).await;
            None
        }
        Ok(Some(Ok(_message))) => {
            connection.last_activity_at = Instant::now();
            Some(connection)
        }
    }
}
