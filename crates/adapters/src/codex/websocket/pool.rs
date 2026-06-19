//! Codex WebSocket 连接池。

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{SinkExt, StreamExt};
use tokio::{net::TcpStream, sync::Mutex, time::timeout};
use tokio_tungstenite::{tungstenite::Message, MaybeTlsStream, WebSocketStream};

use super::deflate::PerMessageDeflateStream;

const DEFAULT_MAX_PER_ACCOUNT: usize = 8;
const DEFAULT_MAX_AGE: Duration = Duration::from_secs(55 * 60);
const DEFAULT_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(25);
const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(25);
const DEFAULT_PING_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_LIVENESS_TIMEOUT: Duration = Duration::from_millis(62_500);

/// WebSocket 连接池 key。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CodexWebSocketPoolKey {
    base_url: String,
    account_id: String,
    conversation_id: String,
}

impl CodexWebSocketPoolKey {
    /// 构造连接池 key。
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

    pub(crate) fn account_id(&self) -> &str {
        &self.account_id
    }
}

/// WebSocket 连接池。
#[derive(Clone)]
pub struct CodexWebSocketPool {
    inner: Arc<Mutex<WebSocketPoolState>>,
    config: CodexWebSocketPoolConfig,
}

impl Default for CodexWebSocketPool {
    fn default() -> Self {
        Self::with_config(CodexWebSocketPoolConfig::default())
    }
}

/// WebSocket 连接池配置。
#[derive(Debug, Clone, Copy)]
pub struct CodexWebSocketPoolConfig {
    /// 是否启用连接池。
    pub enabled: bool,
    /// 单个 socket 的最大生命周期。
    pub max_age: Duration,
    /// 单个账号允许占用的最大池 slot 数。
    pub max_per_account: usize,
    /// 后台维护间隔；`None` 表示不启动后台任务。
    pub maintenance_interval: Option<Duration>,
    /// idle socket 探活 ping 间隔；`None` 表示维护时只做过期清理。
    pub ping_interval: Option<Duration>,
    /// 发送 ping 后等待上游响应的超时时间。
    pub ping_timeout: Duration,
    /// idle socket 无活动多久后视为失活。
    pub liveness_timeout: Option<Duration>,
}

impl Default for CodexWebSocketPoolConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_age: DEFAULT_MAX_AGE,
            max_per_account: DEFAULT_MAX_PER_ACCOUNT,
            maintenance_interval: Some(DEFAULT_MAINTENANCE_INTERVAL),
            ping_interval: Some(DEFAULT_PING_INTERVAL),
            ping_timeout: DEFAULT_PING_TIMEOUT,
            liveness_timeout: Some(DEFAULT_LIVENESS_TIMEOUT),
        }
    }
}

impl CodexWebSocketPool {
    /// 构造连接池策略和状态。
    pub fn new(max_per_account: usize, max_age: Duration) -> Self {
        Self::with_config(CodexWebSocketPoolConfig {
            max_per_account: max_per_account.max(1),
            max_age,
            maintenance_interval: None,
            ping_interval: None,
            liveness_timeout: None,
            ..CodexWebSocketPoolConfig::default()
        })
    }

    /// 使用完整配置构造连接池。
    pub fn with_config(config: CodexWebSocketPoolConfig) -> Self {
        let pool = Self {
            inner: Arc::new(Mutex::new(WebSocketPoolState::default())),
            config,
        };
        pool.spawn_maintenance_task();
        pool
    }

    /// 使用默认配置构造连接池。
    pub fn with_default_max_age() -> Self {
        Self::with_config(CodexWebSocketPoolConfig::default())
    }

    /// 使用最大存活时间与账号容量限制构造连接池。
    pub fn with_limits(max_age: Duration, max_per_account: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(WebSocketPoolState::default())),
            config: CodexWebSocketPoolConfig {
                max_age,
                max_per_account: max_per_account.max(1),
                maintenance_interval: None,
                ping_interval: None,
                liveness_timeout: None,
                ..CodexWebSocketPoolConfig::default()
            },
        }
    }

    /// 返回单账号最大连接数。
    pub fn max_per_account(&self) -> usize {
        self.config.max_per_account
    }

    /// 返回连接最大存活时长。
    pub fn max_age(&self) -> Duration {
        self.config.max_age
    }

    /// 判断当前账号是否还能打开新连接。
    pub fn permits_new_connection(&self, current_connections: usize) -> bool {
        current_connections < self.config.max_per_account
    }

    /// 判断连接是否已达到回收年龄。
    pub fn should_recycle(&self, age: Duration) -> bool {
        age >= self.config.max_age
    }

    pub(crate) async fn acquire(&self, key: &CodexWebSocketPoolKey) -> WebSocketPoolAcquire {
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
                    if connection.created_at.elapsed() < self.config.max_age {
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
            close_pooled_connection(connection).await;
        }

        acquire
    }

    pub(crate) async fn put(
        &self,
        key: CodexWebSocketPoolKey,
        connection: PooledWebSocketConnection,
    ) {
        let mut connection = Some(connection);
        {
            let mut state = self.inner.lock().await;
            let expired = connection
                .as_ref()
                .is_some_and(|connection| connection.created_at.elapsed() >= self.config.max_age);
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

    pub(crate) async fn discard(&self, key: &CodexWebSocketPoolKey) {
        let idle_connection = {
            let mut state = self.inner.lock().await;
            match state.slots.remove(key) {
                Some(WebSocketPoolSlot::Idle(connection)) => Some(*connection),
                Some(WebSocketPoolSlot::Busy | WebSocketPoolSlot::Checking) | None => None,
            }
        };
        if let Some(connection) = idle_connection {
            close_pooled_connection(connection).await;
        }
    }

    /// 驱逐指定账号的 idle 连接，并阻止已占用 slot 回收到池中。
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
        close_pooled_connections(idle_connections).await;
    }

    /// 关闭连接池，关闭所有 idle 连接，并让后续 acquire 直接绕过池。
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
        close_pooled_connections(idle_connections).await;
    }

    /// 维护 idle 连接：关闭过期/失活连接，并按需 ping 探活。
    pub async fn gc_sweep(&self) {
        self.maintain_idle_connections().await;
    }

    /// 维护 idle 连接：关闭过期/失活连接，并按需 ping 探活。
    pub async fn maintain_idle_connections(&self) {
        let maintenance = self.take_idle_connections_for_maintenance().await;
        close_pooled_connections(maintenance.close).await;
        for (key, connection) in maintenance.probe {
            let connection = probe_idle_connection(connection, self.config.ping_timeout).await;
            self.return_maintained_connection(key, connection).await;
        }
    }

    fn spawn_maintenance_task(&self) {
        let Some(interval_duration) = self.config.maintenance_interval else {
            return;
        };
        let Ok(_handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let inner = Arc::downgrade(&self.inner);
        let config = self.config;
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

    async fn take_idle_connections_for_maintenance(&self) -> WebSocketPoolMaintenance {
        let mut close = Vec::new();
        let mut probe = Vec::new();
        let now = Instant::now();
        let Some(ping_interval) = self.config.ping_interval else {
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
                        ) =>
                    {
                        Some(key.clone())
                    }
                    WebSocketPoolSlot::Idle(_)
                    | WebSocketPoolSlot::Busy
                    | WebSocketPoolSlot::Checking => None,
                })
                .collect::<Vec<_>>();
            for key in keys {
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

#[derive(Default)]
struct WebSocketPoolState {
    slots: HashMap<CodexWebSocketPoolKey, WebSocketPoolSlot>,
    shutting_down: bool,
}

pub(crate) type CodexWsStream = WebSocketStream<PerMessageDeflateStream<MaybeTlsStream<TcpStream>>>;

#[derive(Clone)]
pub(crate) struct CodexWebSocketConnectionMetadata {
    pub(crate) turn_state: Option<String>,
    pub(crate) set_cookie_headers: Vec<String>,
    pub(crate) rate_limit_headers: Vec<(String, String)>,
    pub(crate) handshake_status: u16,
}

pub(crate) struct PooledWebSocketConnection {
    pub(crate) websocket: CodexWsStream,
    pub(crate) metadata: CodexWebSocketConnectionMetadata,
    pub(crate) created_at: Instant,
    pub(crate) last_activity_at: Instant,
    pub(crate) last_ping_at: Option<Instant>,
}

enum WebSocketPoolSlot {
    Idle(Box<PooledWebSocketConnection>),
    Busy,
    Checking,
}

pub(crate) enum WebSocketPoolAcquire {
    Reused(Box<PooledWebSocketConnection>),
    FreshReserved,
    Bypass,
}

fn account_slot_count(
    slots: &HashMap<CodexWebSocketPoolKey, WebSocketPoolSlot>,
    account_id: &str,
) -> usize {
    slots
        .keys()
        .filter(|key| key.account_id() == account_id)
        .count()
}

async fn close_pooled_connection(mut connection: PooledWebSocketConnection) {
    let _ = connection.websocket.send(Message::Close(None)).await;
}

async fn close_pooled_connections(connections: Vec<PooledWebSocketConnection>) {
    for connection in connections {
        close_pooled_connection(connection).await;
    }
}

struct WebSocketPoolMaintenance {
    close: Vec<PooledWebSocketConnection>,
    probe: Vec<(CodexWebSocketPoolKey, PooledWebSocketConnection)>,
}

fn should_close_idle_connection(
    connection: &PooledWebSocketConnection,
    now: Instant,
    max_age: Duration,
    liveness_timeout: Option<Duration>,
) -> bool {
    now.duration_since(connection.created_at) >= max_age
        || liveness_timeout.is_some_and(|timeout| {
            !timeout.is_zero() && now.duration_since(connection.last_activity_at) >= timeout
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
    connection.last_ping_at = Some(Instant::now());
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
