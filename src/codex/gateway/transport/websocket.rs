use std::{
    collections::HashMap,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use thiserror::Error;
use tokio::{net::TcpStream, sync::Mutex, time::timeout};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{HeaderMap as WsHeaderMap, Request as WsRequest},
        Error as WsError, Message,
    },
    MaybeTlsStream, WebSocketStream,
};

use futures::{channel::mpsc, SinkExt, Stream, StreamExt};
use reqwest::{header::HeaderMap, StatusCode};
use serde_json::{json, Value};

use crate::codex::gateway::transport::{
    rate_limits::{parse_rate_limits_event_raw, rate_limits_to_header_pairs, ParsedRateLimits},
    sse::encode_sse_event,
    types::CodexResponsesRequest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexTransport {
    HttpSse,
    WebSocketPreferred,
    WebSocketRequired,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebSocketSupportError {
    #[error("previous_response_id requires Codex WebSocket transport")]
    PreviousResponseRequiresWebSocket,
    #[error("request explicitly requires Codex WebSocket transport")]
    ExplicitWebSocketRequired,
}

#[derive(Debug, Error)]
pub enum CodexWebSocketError {
    #[error("invalid WebSocket request: {0}")]
    InvalidRequest(#[from] tokio_tungstenite::tungstenite::http::Error),
    #[error("websocket transport error: {0}")]
    Transport(#[source] WsError),
    #[error("websocket handshake returned status {status}: {body}")]
    Upstream {
        status: StatusCode,
        body: String,
        retry_after_seconds: Option<u64>,
    },
    #[error("websocket response ended before any events")]
    EmptyResponse,
    #[error("websocket closed before terminal event")]
    ClosedBeforeTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexWebSocketResponse {
    pub body: String,
    pub turn_state: Option<String>,
    pub set_cookie_headers: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
}

pub type CodexWebSocketSseStream =
    Pin<Box<dyn Stream<Item = Result<String, CodexWebSocketError>> + Send>>;
pub type SharedRateLimitUpdates = Arc<Mutex<Vec<ParsedRateLimits>>>;

pub struct CodexWebSocketStreamResponse {
    pub body_stream: CodexWebSocketSseStream,
    pub turn_state: Option<String>,
    pub set_cookie_headers: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
    pub rate_limit_updates: SharedRateLimitUpdates,
}

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

    async fn acquire(&self, key: &CodexWebSocketPoolKey) -> WebSocketPoolAcquire {
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

    async fn put(&self, key: CodexWebSocketPoolKey, connection: PooledWebSocketConnection) {
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

    async fn discard(&self, key: &CodexWebSocketPoolKey) {
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

type CodexWsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexWebSocketConnectionMetadata {
    turn_state: Option<String>,
    set_cookie_headers: Vec<String>,
    rate_limit_headers: Vec<(String, String)>,
}

impl CodexWebSocketConnectionMetadata {
    fn from_headers(headers: &WsHeaderMap) -> Self {
        Self {
            turn_state: turn_state(headers),
            set_cookie_headers: set_cookie_headers(headers),
            rate_limit_headers: rate_limit_headers(headers),
        }
    }
}

struct PooledWebSocketConnection {
    websocket: CodexWsStream,
    metadata: CodexWebSocketConnectionMetadata,
    created_at: Instant,
    last_activity_at: Instant,
    last_ping_at: Option<Instant>,
}

enum WebSocketPoolSlot {
    Idle(Box<PooledWebSocketConnection>),
    Busy,
    Checking,
}

enum WebSocketPoolAcquire {
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

struct ActiveWebSocket {
    websocket: CodexWsStream,
    metadata: CodexWebSocketConnectionMetadata,
    pool_return: Option<WebSocketPoolReturn>,
    reused: bool,
    last_activity_at: Instant,
}

struct WebSocketPoolReturn {
    pool: Arc<CodexWebSocketPool>,
    key: CodexWebSocketPoolKey,
    created_at: Instant,
}

impl ActiveWebSocket {
    async fn finish(self) {
        let Self {
            websocket,
            metadata,
            pool_return,
            reused: _,
            last_activity_at,
        } = self;
        let Some(pool_return) = pool_return else {
            let mut websocket = websocket;
            let _ = websocket.close(None).await;
            return;
        };
        pool_return
            .pool
            .put(
                pool_return.key,
                PooledWebSocketConnection {
                    websocket,
                    metadata,
                    created_at: pool_return.created_at,
                    last_activity_at,
                    last_ping_at: None,
                },
            )
            .await;
    }

    async fn discard(self) {
        let Self {
            mut websocket,
            pool_return,
            ..
        } = self;
        if let Some(pool_return) = pool_return {
            pool_return.pool.discard(&pool_return.key).await;
        }
        let _ = websocket.close(None).await;
    }
}

pub fn transport_for_request(request: &CodexResponsesRequest) -> CodexTransport {
    if request.previous_response_id.is_some() {
        CodexTransport::WebSocketRequired
    } else if request.force_http_sse {
        CodexTransport::HttpSse
    } else {
        CodexTransport::WebSocketPreferred
    }
}

pub fn http_sse_fallback_allowed(request: &CodexResponsesRequest) -> bool {
    matches!(
        transport_for_request(request),
        CodexTransport::WebSocketPreferred | CodexTransport::HttpSse
    )
}

#[tracing::instrument(
    skip(base_url, request, headers),
    fields(
        model = %request.model,
        has_previous_response_id = request.previous_response_id.is_some(),
    )
)]
pub async fn create_response_via_websocket(
    base_url: &str,
    request: &CodexResponsesRequest,
    headers: HeaderMap,
) -> Result<CodexWebSocketResponse, CodexWebSocketError> {
    let CodexWebSocketStreamResponse {
        mut body_stream,
        turn_state,
        set_cookie_headers,
        mut rate_limit_headers,
        rate_limit_updates,
    } = create_response_via_websocket_stream(base_url, request, headers).await?;

    let mut body = String::new();
    while let Some(chunk) = body_stream.next().await {
        body.push_str(&chunk?);
    }
    if body.is_empty() {
        return Err(CodexWebSocketError::EmptyResponse);
    }
    append_rate_limit_updates(&mut rate_limit_headers, &rate_limit_updates).await;

    Ok(CodexWebSocketResponse {
        body,
        turn_state,
        set_cookie_headers,
        rate_limit_headers,
    })
}

#[tracing::instrument(
    skip(base_url, request, headers),
    fields(
        model = %request.model,
        has_previous_response_id = request.previous_response_id.is_some(),
    )
)]
pub async fn create_response_via_websocket_stream(
    base_url: &str,
    request: &CodexResponsesRequest,
    headers: HeaderMap,
) -> Result<CodexWebSocketStreamResponse, CodexWebSocketError> {
    create_response_via_websocket_stream_inner(base_url, request, headers, None).await
}

#[tracing::instrument(
    skip(base_url, request, headers, pool, pool_key),
    fields(
        model = %request.model,
        has_previous_response_id = request.previous_response_id.is_some(),
    )
)]
pub async fn create_response_via_websocket_stream_with_pool(
    base_url: &str,
    request: &CodexResponsesRequest,
    headers: HeaderMap,
    pool: Arc<CodexWebSocketPool>,
    pool_key: CodexWebSocketPoolKey,
) -> Result<CodexWebSocketStreamResponse, CodexWebSocketError> {
    create_response_via_websocket_stream_inner(base_url, request, headers, Some((pool, pool_key)))
        .await
}

async fn create_response_via_websocket_stream_inner(
    base_url: &str,
    request: &CodexResponsesRequest,
    headers: HeaderMap,
    pool: Option<(Arc<CodexWebSocketPool>, CodexWebSocketPoolKey)>,
) -> Result<CodexWebSocketStreamResponse, CodexWebSocketError> {
    let mut pool_context = pool;
    let mut retry_stale_reuse = pool_context.is_some();
    let rate_limit_updates = Arc::new(Mutex::new(Vec::new()));
    loop {
        let mut active = acquire_websocket(base_url, headers.clone(), pool_context.clone()).await?;
        if let Err(error) = active
            .websocket
            .send(Message::Text(
                websocket_request_body(request).to_string().into(),
            ))
            .await
        {
            let reused = active.reused;
            active.discard().await;
            if reused && retry_stale_reuse {
                retry_stale_reuse = false;
                pool_context = None;
                continue;
            }
            return Err(CodexWebSocketError::Transport(error));
        }
        let metadata = active.metadata.clone();

        loop {
            let Some(message) = active.websocket.next().await else {
                let reused = active.reused;
                active.discard().await;
                if reused && retry_stale_reuse {
                    retry_stale_reuse = false;
                    pool_context = None;
                    break;
                }
                return Err(CodexWebSocketError::EmptyResponse);
            };
            let message = match message {
                Ok(message) => message,
                Err(error) => {
                    let reused = active.reused;
                    active.discard().await;
                    if reused && retry_stale_reuse {
                        retry_stale_reuse = false;
                        pool_context = None;
                        break;
                    }
                    return Err(CodexWebSocketError::Transport(error));
                }
            };
            active.last_activity_at = Instant::now();
            let Some(raw) = websocket_message_text(message) else {
                continue;
            };
            if is_internal_websocket_event(&raw) {
                capture_internal_rate_limit_event(&raw, &rate_limit_updates).await;
                continue;
            }
            if let Some(classified) = classify_ws_error_frame(&raw) {
                if classified.connection_fatal {
                    active.discard().await;
                } else {
                    active.finish().await;
                }
                return Err(CodexWebSocketError::Upstream {
                    status: classified.status,
                    retry_after_seconds: retry_after_seconds_from_body(&raw),
                    body: raw,
                });
            }

            let first_event = websocket_event_type(&raw);
            let first_chunk = websocket_sse_chunk(&raw, first_event.as_deref());
            let terminal = first_event
                .as_deref()
                .is_some_and(is_terminal_websocket_event);
            let (tx, rx) = mpsc::unbounded();
            if tx.unbounded_send(Ok(first_chunk)).is_err() {
                active.discard().await;
                return Err(CodexWebSocketError::EmptyResponse);
            }
            if terminal {
                active.finish().await;
            } else {
                let rate_limit_updates = rate_limit_updates.clone();
                tokio::spawn(async move {
                    forward_websocket_as_sse(active, tx, rate_limit_updates).await;
                });
            }
            return Ok(CodexWebSocketStreamResponse {
                body_stream: Box::pin(rx),
                turn_state: metadata.turn_state,
                set_cookie_headers: metadata.set_cookie_headers,
                rate_limit_headers: metadata.rate_limit_headers,
                rate_limit_updates,
            });
        }
    }
}

async fn acquire_websocket(
    base_url: &str,
    headers: HeaderMap,
    pool: Option<(Arc<CodexWebSocketPool>, CodexWebSocketPoolKey)>,
) -> Result<ActiveWebSocket, CodexWebSocketError> {
    let Some((pool, key)) = pool else {
        let created_at = Instant::now();
        let (websocket, metadata) = connect_websocket(base_url, headers).await?;
        return Ok(ActiveWebSocket {
            websocket,
            metadata,
            pool_return: None,
            reused: false,
            last_activity_at: created_at,
        });
    };

    match pool.acquire(&key).await {
        WebSocketPoolAcquire::Reused(connection) => {
            let PooledWebSocketConnection {
                websocket,
                metadata,
                created_at,
                last_activity_at,
                last_ping_at: _,
            } = *connection;
            return Ok(ActiveWebSocket {
                websocket,
                metadata,
                pool_return: Some(WebSocketPoolReturn {
                    pool,
                    key,
                    created_at,
                }),
                reused: true,
                last_activity_at,
            });
        }
        WebSocketPoolAcquire::FreshReserved => {}
        WebSocketPoolAcquire::Bypass => {
            let (websocket, metadata) = connect_websocket(base_url, headers).await?;
            return Ok(ActiveWebSocket {
                websocket,
                metadata,
                pool_return: None,
                reused: false,
                last_activity_at: Instant::now(),
            });
        }
    }

    let created_at = Instant::now();
    let (websocket, metadata) = match connect_websocket(base_url, headers).await {
        Ok(connection) => connection,
        Err(error) => {
            pool.discard(&key).await;
            return Err(error);
        }
    };
    Ok(ActiveWebSocket {
        websocket,
        metadata,
        pool_return: Some(WebSocketPoolReturn {
            pool,
            key,
            created_at,
        }),
        reused: false,
        last_activity_at: created_at,
    })
}

async fn connect_websocket(
    base_url: &str,
    headers: HeaderMap,
) -> Result<(CodexWsStream, CodexWebSocketConnectionMetadata), CodexWebSocketError> {
    let ws_request = build_ws_request(base_url, headers)?;
    let (websocket, handshake_response) = connect_async(ws_request)
        .await
        .map_err(codex_websocket_transport_error)?;
    Ok((
        websocket,
        CodexWebSocketConnectionMetadata::from_headers(handshake_response.headers()),
    ))
}

fn build_ws_request(
    base_url: &str,
    headers: HeaderMap,
) -> Result<WsRequest<()>, CodexWebSocketError> {
    let mut request = websocket_url(base_url)
        .into_client_request()
        .map_err(codex_websocket_transport_error)?;
    for (name, value) in &headers {
        let Ok(name) =
            tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(name.as_str().as_bytes())
        else {
            continue;
        };
        let Ok(value) =
            tokio_tungstenite::tungstenite::http::HeaderValue::from_bytes(value.as_bytes())
        else {
            continue;
        };
        request.headers_mut().insert(name, value);
    }
    Ok(request)
}

fn websocket_url(base_url: &str) -> String {
    let url = format!("{}/codex/responses", base_url.trim_end_matches('/'));
    if let Some(rest) = url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        url
    }
}

fn websocket_request_body(request: &CodexResponsesRequest) -> Value {
    let mut body = json!({
        "type": "response.create",
        "model": request.model,
        "instructions": request.instructions,
        "input": request.input,
        "stream": true,
        "store": false,
        "tool_choice": request.tool_choice.clone().unwrap_or_else(|| json!("auto")),
        "parallel_tool_calls": request.parallel_tool_calls.unwrap_or(true),
    });
    if let Some(previous_response_id) = &request.previous_response_id {
        body["previous_response_id"] = Value::String(previous_response_id.clone());
    }
    if let Some(reasoning) = &request.reasoning {
        body["reasoning"] = reasoning.clone();
    }
    if let Some(tools) = request.tools.as_ref().filter(|tools| !tools.is_empty()) {
        body["tools"] = Value::Array(tools.clone());
    }
    if let Some(text) = &request.text {
        body["text"] = text.clone();
    }
    if let Some(service_tier) = &request.service_tier {
        body["service_tier"] = Value::String(service_tier.clone());
    }
    if let Some(prompt_cache_key) = &request.prompt_cache_key {
        body["prompt_cache_key"] = Value::String(prompt_cache_key.clone());
    }
    if let Some(include) = request
        .include
        .as_ref()
        .filter(|include| !include.is_empty())
    {
        body["include"] = Value::Array(include.iter().cloned().map(Value::String).collect());
    }
    if let Some(client_metadata) = &request.client_metadata {
        body["client_metadata"] = client_metadata.clone();
    }
    body
}

fn websocket_message_text(message: Message) -> Option<String> {
    match message {
        Message::Text(text) => Some(text.to_string()),
        Message::Binary(bytes) => String::from_utf8(bytes.to_vec()).ok(),
        _ => None,
    }
}

fn websocket_event_type(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw).ok().and_then(|value| {
        value
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn websocket_sse_chunk(raw: &str, event: Option<&str>) -> String {
    encode_sse_event(event.unwrap_or_default(), raw)
}

fn is_internal_websocket_event(raw: &str) -> bool {
    websocket_event_type(raw).as_deref() == Some("codex.rate_limits")
}

fn is_terminal_websocket_event(event: &str) -> bool {
    event == "response.completed" || event == "response.failed" || event == "error"
}

async fn forward_websocket_as_sse(
    mut active: ActiveWebSocket,
    tx: mpsc::UnboundedSender<Result<String, CodexWebSocketError>>,
    rate_limit_updates: SharedRateLimitUpdates,
) {
    while let Some(message) = active.websocket.next().await {
        match message {
            Ok(message) => {
                active.last_activity_at = Instant::now();
                let Some(raw) = websocket_message_text(message) else {
                    continue;
                };
                if is_internal_websocket_event(&raw) {
                    capture_internal_rate_limit_event(&raw, &rate_limit_updates).await;
                    continue;
                }
                let event = websocket_event_type(&raw);
                let terminal = event.as_deref().is_some_and(is_terminal_websocket_event);
                let chunk = websocket_sse_chunk(&raw, event.as_deref());
                if tx.unbounded_send(Ok(chunk)).is_err() {
                    active.discard().await;
                    return;
                }
                if terminal {
                    active.finish().await;
                    return;
                }
            }
            Err(error) => {
                let _ = tx.unbounded_send(Err(CodexWebSocketError::Transport(error)));
                active.discard().await;
                return;
            }
        }
    }
    let _ = tx.unbounded_send(Err(CodexWebSocketError::ClosedBeforeTerminal));
    active.discard().await;
}

async fn capture_internal_rate_limit_event(raw: &str, updates: &SharedRateLimitUpdates) {
    if let Some(parsed) = parse_rate_limits_event_raw(raw) {
        updates.lock().await.push(parsed);
    }
}

pub async fn append_rate_limit_updates(
    headers: &mut Vec<(String, String)>,
    updates: &SharedRateLimitUpdates,
) {
    let updates = updates.lock().await;
    for update in updates.iter() {
        headers.extend(rate_limits_to_header_pairs(update));
    }
}

struct ClassifiedWebSocketError {
    status: StatusCode,
    connection_fatal: bool,
}

fn classify_ws_error_frame(raw: &str) -> Option<ClassifiedWebSocketError> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    let event_type = value.get("type").and_then(Value::as_str)?;
    if event_type != "error" && event_type != "response.failed" {
        return None;
    }
    let code = value
        .pointer("/response/error/code")
        .or_else(|| value.pointer("/response/error/type"))
        .or_else(|| value.pointer("/error/code"))
        .or_else(|| value.pointer("/error/type"))
        .and_then(Value::as_str)?
        .to_ascii_lowercase();
    let status = rotatable_error_status(&code)?;
    Some(ClassifiedWebSocketError {
        status,
        connection_fatal: code == "websocket_connection_limit_reached",
    })
}

fn rotatable_error_status(code: &str) -> Option<StatusCode> {
    match code {
        "usage_limit_reached" | "rate_limit_exceeded" | "rate_limit_reached" => {
            Some(StatusCode::TOO_MANY_REQUESTS)
        }
        "quota_exhausted" | "payment_required" => Some(StatusCode::PAYMENT_REQUIRED),
        "unauthorized" | "token_invalid" | "token_expired" | "account_deactivated" => {
            Some(StatusCode::UNAUTHORIZED)
        }
        "forbidden" | "account_banned" | "banned" => Some(StatusCode::FORBIDDEN),
        "previous_response_not_found" => Some(StatusCode::BAD_REQUEST),
        "websocket_connection_limit_reached" => Some(StatusCode::SERVICE_UNAVAILABLE),
        _ => None,
    }
}

fn codex_websocket_transport_error(error: WsError) -> CodexWebSocketError {
    match error {
        WsError::Http(response) => {
            let (parts, body) = (*response).into_parts();
            let body = body
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_default();
            CodexWebSocketError::Upstream {
                status: parts.status,
                retry_after_seconds: retry_after_seconds(&parts.headers)
                    .or_else(|| retry_after_seconds_from_body(&body)),
                body,
            }
        }
        error => CodexWebSocketError::Transport(error),
    }
}

fn turn_state(headers: &WsHeaderMap) -> Option<String> {
    headers
        .get("x-codex-turn-state")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

fn set_cookie_headers(headers: &WsHeaderMap) -> Vec<String> {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok().map(ToString::to_string))
        .collect()
}

fn rate_limit_headers(headers: &WsHeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(name, _)| is_rate_limit_header(name.as_str()))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_string(), value.to_string()))
        })
        .collect()
}

fn is_rate_limit_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == "retry-after"
        || name.contains("ratelimit")
        || name.contains("rate-limit")
        || name.starts_with("x-codex-primary-")
        || name.starts_with("x-codex-secondary-")
        || name.starts_with("x-codex-code-review-")
        || name.starts_with("x-codex-review-")
        || name.starts_with("x-code-review-")
}

fn retry_after_seconds(headers: &WsHeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
}

fn retry_after_seconds_from_body(body: &str) -> Option<u64> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let error = value
        .pointer("/response/error")
        .or_else(|| value.get("error"))
        .unwrap_or(&value);
    if let Some(seconds) = error
        .get("resets_in_seconds")
        .and_then(Value::as_u64)
        .filter(|seconds| *seconds > 0)
    {
        return Some(seconds);
    }
    let resets_at = error.get("resets_at").and_then(Value::as_u64)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    (resets_at > now).then_some(resets_at - now)
}
