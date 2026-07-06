//! Codex WebSocket 连接池。

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::websocket_pump::{PumpKeepalive, PumpedWebSocket};

const DEFAULT_MAX_PER_ACCOUNT: usize = 8;
const DEFAULT_MAX_AGE: Duration = Duration::from_mins(55);
const DEFAULT_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(25);
const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(25);
const DEFAULT_PING_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const DEFAULT_FIRST_TOKEN_TIMEOUT: Duration = Duration::from_secs(15);

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

    pub(crate) fn conversation_id_hash(&self) -> String {
        short_sha256([self.conversation_id.as_str()])
    }

    pub(crate) fn stable_hash(&self) -> String {
        short_sha256([
            self.base_url.as_str(),
            self.account_id.as_str(),
            self.conversation_id.as_str(),
        ])
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
    /// 首个内容帧到达前的绝对超时；`None` 表示禁用首 token 熔断。
    pub first_token_timeout: Option<Duration>,
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
            // idle 连接不设失活截断：靠 ping/pong 保活，只在 max_age（55 分钟）
            // 或 ping 失败时关闭，最大化跨轮复用（对齐 Codex CLI 的长连接策略）。
            liveness_timeout: None,
            first_token_timeout: Some(DEFAULT_FIRST_TOKEN_TIMEOUT),
        }
    }
}

impl CodexWebSocketPoolConfig {
    /// pump 后台任务的保活策略：从连接池配置派生出 ping 间隔与 liveness 超时。
    pub(crate) fn keepalive(&self) -> PumpKeepalive {
        PumpKeepalive {
            ping_interval: self.ping_interval,
            liveness_timeout: self.liveness_timeout,
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

    /// pump 后台任务的保活策略（供建连时传入）。
    pub(crate) fn keepalive(&self) -> PumpKeepalive {
        self.config.keepalive()
    }

    /// 首个内容帧到达前的绝对超时；`None` 表示禁用首 token 熔断。
    pub(crate) fn first_token_timeout(&self) -> Option<Duration> {
        self.config.first_token_timeout
    }

    pub(crate) async fn acquire(&self, key: &CodexWebSocketPoolKey) -> WebSocketPoolAcquire {
        let mut expired_connection = None;
        let acquire = {
            let mut state = self.inner.lock().await;
            if !self.config.enabled || state.shutting_down {
                return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Disabled);
            }
            match state.slots.get(key) {
                Some(WebSocketPoolSlot::Busy) => {
                    return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Busy);
                }
                Some(WebSocketPoolSlot::Idle(_)) => {
                    let Some(WebSocketPoolSlot::Idle(connection)) = state.slots.remove(key) else {
                        return WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Busy);
                    };
                    // 零成本探活：后台 pump 已实时感知连接死亡（RST/Close/EOF/失活），
                    // 复用前只需读取 is_closed 标志，避免复用到静默死连接后卡到超时。
                    if connection.created_at.elapsed() < self.config.max_age
                        && !connection.websocket.is_closed()
                    {
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
                WebSocketPoolAcquire::Bypass(WebSocketPoolBypassReason::Cap)
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
                Some(WebSocketPoolSlot::Busy) | None => None,
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
                    WebSocketPoolSlot::Busy => None,
                })
                .collect::<Vec<_>>()
        };
        close_pooled_connections(idle_connections).await;
    }

    /// 维护 idle 连接：清扫已被后台 pump 判定死亡或已超龄的连接。
    ///
    /// 保活（ping/pong）与失活检测已下沉到每条连接的 pump 任务内部，此处不再做
    /// 同步 ping 探活，只负责把「后台已标记 closed」或「超过 max_age」的 idle
    /// 连接从池中摘除并关闭，避免它们占用 slot。
    pub async fn maintain_idle_connections(&self) {
        let close = self.take_dead_idle_connections().await;
        close_pooled_connections(close).await;
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
                if pool.is_shutdown().await {
                    break;
                }
                pool.maintain_idle_connections().await;
            }
        });
    }

    /// 返回连接池是否已进入关闭状态。
    pub async fn is_shutdown(&self) -> bool {
        self.inner.lock().await.shutting_down
    }

    async fn take_dead_idle_connections(&self) -> Vec<PooledWebSocketConnection> {
        let mut close = Vec::new();
        let now = Instant::now();
        let mut state = self.inner.lock().await;
        if state.shutting_down || !self.config.enabled {
            return close;
        }
        let keys = state
            .slots
            .iter()
            .filter_map(|(key, slot)| match slot {
                WebSocketPoolSlot::Idle(connection)
                    if should_close_idle_connection(connection, now, self.config.max_age) =>
                {
                    Some(key.clone())
                }
                WebSocketPoolSlot::Idle(_) | WebSocketPoolSlot::Busy => None,
            })
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(WebSocketPoolSlot::Idle(connection)) = state.slots.remove(&key) {
                close.push(*connection);
            }
        }
        close
    }
}

#[derive(Default)]
struct WebSocketPoolState {
    slots: HashMap<CodexWebSocketPoolKey, WebSocketPoolSlot>,
    shutting_down: bool,
}

#[derive(Clone)]
pub(crate) struct CodexWebSocketConnectionMetadata {
    pub(crate) turn_state: Option<String>,
    pub(crate) set_cookie_headers: Vec<String>,
    pub(crate) rate_limit_headers: Vec<(String, String)>,
}

pub(crate) struct PooledWebSocketConnection {
    pub(crate) websocket: PumpedWebSocket,
    pub(crate) metadata: CodexWebSocketConnectionMetadata,
    pub(crate) created_at: Instant,
}

enum WebSocketPoolSlot {
    Idle(Box<PooledWebSocketConnection>),
    Busy,
}

pub(crate) enum WebSocketPoolAcquire {
    Reused(Box<PooledWebSocketConnection>),
    FreshReserved,
    Bypass(WebSocketPoolBypassReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSocketPoolBypassReason {
    Disabled,
    Busy,
    Cap,
}

impl WebSocketPoolBypassReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Busy => "busy",
            Self::Cap => "cap",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSocketPoolDecision {
    kind: WebSocketPoolDecisionKind,
    reason: Option<WebSocketPoolBypassReason>,
}

impl WebSocketPoolDecision {
    pub fn new() -> Self {
        Self {
            kind: WebSocketPoolDecisionKind::New,
            reason: None,
        }
    }

    pub fn reuse() -> Self {
        Self {
            kind: WebSocketPoolDecisionKind::Reuse,
            reason: None,
        }
    }

    pub fn bypass(reason: WebSocketPoolBypassReason) -> Self {
        Self {
            kind: WebSocketPoolDecisionKind::Bypass,
            reason: Some(reason),
        }
    }

    pub fn retry_after_stale_reuse() -> Self {
        Self {
            kind: WebSocketPoolDecisionKind::RetryAfterStaleReuse,
            reason: None,
        }
    }

    pub fn kind(self) -> &'static str {
        self.kind.as_str()
    }

    pub fn reason(self) -> Option<&'static str> {
        self.reason.map(WebSocketPoolBypassReason::as_str)
    }

    pub fn metadata_value(self) -> Value {
        let mut value = json!({ "kind": self.kind() });
        if let (Some(object), Some(reason)) = (value.as_object_mut(), self.reason()) {
            object.insert("reason".to_string(), Value::String(reason.to_string()));
        }
        value
    }
}

impl Default for WebSocketPoolDecision {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebSocketPoolDecisionKind {
    New,
    Reuse,
    Bypass,
    RetryAfterStaleReuse,
}

impl WebSocketPoolDecisionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Reuse => "reuse",
            Self::Bypass => "bypass",
            Self::RetryAfterStaleReuse => "retry_after_stale_reuse",
        }
    }
}

fn short_sha256<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    hex::encode(hasher.finalize()).chars().take(12).collect()
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
    connection.websocket.close().await;
}

async fn close_pooled_connections(connections: Vec<PooledWebSocketConnection>) {
    for connection in connections {
        close_pooled_connection(connection).await;
    }
}

/// idle 连接是否应从池中摘除：被后台 pump 标记死亡，或已超过 `max_age`。
fn should_close_idle_connection(
    connection: &PooledWebSocketConnection,
    now: Instant,
    max_age: Duration,
) -> bool {
    connection.websocket.is_closed() || now.duration_since(connection.created_at) >= max_age
}
