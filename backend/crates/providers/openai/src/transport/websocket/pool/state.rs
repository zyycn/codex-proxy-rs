//! WebSocket 连接池状态和值对象。

use std::{collections::HashMap, time::Duration};

use sha2::{Digest, Sha256};
use tokio::{sync::watch, time::Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::super::super::{
    diagnostics::CodexUpstreamDiagnostics, response_meta::CodexResponseMetadata,
};
use super::super::pump::PumpedWebSocket;
use super::lease::WebSocketPoolConnectOutcome;

/// WebSocket 连接池 key。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CodexWebSocketPoolKey {
    base_url: String,
    account_id: String,
    conversation_id: String,
    connection_profile: String,
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
            connection_profile: String::new(),
        }
    }

    /// 区分实际 WebSocket opening 画像，防止复用旧 UA 或不同握手语义的连接。
    pub(crate) fn with_connection_profile(mut self, connection_profile: impl Into<String>) -> Self {
        self.connection_profile = connection_profile.into();
        self
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
            self.connection_profile.as_str(),
        ])
    }

    /// 原生 continuation 不依赖客户端 conversation ID，只依赖上游 origin、
    /// 已冻结账号和握手画像；具体连接再由 latest_response_id 精确确认。
    pub(super) fn same_upstream_owner(&self, other: &Self) -> bool {
        self.base_url == other.base_url
            && self.account_id == other.account_id
            && self.connection_profile == other.connection_profile
    }
}

#[derive(Default)]
pub(super) struct WebSocketPoolState {
    pub(super) slots: HashMap<CodexWebSocketPoolKey, WebSocketPoolSlot>,
    pub(super) shutting_down: bool,
}

#[derive(Clone)]
pub(crate) struct CodexWebSocketConnectionMetadata {
    pub(crate) turn_state: Option<String>,
    pub(crate) set_cookie_headers: Vec<String>,
    pub(crate) rate_limit_headers: Vec<(String, String)>,
    pub(crate) response_metadata: CodexResponseMetadata,
    pub(crate) diagnostics: CodexUpstreamDiagnostics,
}

pub(crate) struct PooledWebSocketConnection {
    pub(crate) websocket: PumpedWebSocket,
    pub(crate) metadata: CodexWebSocketConnectionMetadata,
    pub(crate) continuation: WebSocketContinuationState,
    pub(crate) created_at: Instant,
}

/// 只随具体 WebSocket 生命周期存在的续接状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WebSocketContinuationState {
    latest_response_id: Option<String>,
}

impl WebSocketContinuationState {
    pub(crate) fn latest_response_id(&self) -> Option<&str> {
        self.latest_response_id.as_deref()
    }

    pub(crate) fn record_completed(&mut self, response_id: String) {
        self.latest_response_id = Some(response_id);
    }
}

pub(super) enum WebSocketPoolSlot {
    Idle {
        connection: Box<PooledWebSocketConnection>,
        last_used_at: Instant,
    },
    Busy(WebSocketPoolReservation),
    Connecting(WebSocketPoolConnecting),
}

impl WebSocketPoolSlot {
    pub(super) fn latest_response_id(&self) -> Option<&str> {
        match self {
            Self::Idle { connection, .. } => connection.continuation.latest_response_id(),
            Self::Busy(reservation) => reservation.latest_response_id.as_deref(),
            Self::Connecting(_) => None,
        }
    }
}

pub(super) struct WebSocketPoolConnecting {
    pub(super) id: Uuid,
    pub(super) started_at: Instant,
    pub(super) outcome: watch::Sender<WebSocketPoolConnectOutcome>,
    pub(super) cancellation: CancellationToken,
}

#[derive(Clone)]
pub(super) struct WebSocketPoolReservation {
    pub(super) id: Uuid,
    pub(super) reserved_at: Instant,
    pub(super) latest_response_id: Option<String>,
}

pub(super) fn account_slot_count(
    slots: &HashMap<CodexWebSocketPoolKey, WebSocketPoolSlot>,
    account_id: &str,
) -> usize {
    slots
        .keys()
        .filter(|key| key.account_id() == account_id)
        .count()
}

pub(super) fn take_lru_idle_connection(
    state: &mut WebSocketPoolState,
) -> Option<PooledWebSocketConnection> {
    let key = state
        .slots
        .iter()
        .filter_map(|(key, slot)| match slot {
            WebSocketPoolSlot::Idle { last_used_at, .. } => Some((key, *last_used_at)),
            WebSocketPoolSlot::Busy(_) | WebSocketPoolSlot::Connecting(_) => None,
        })
        .min_by_key(|(_, last_used_at)| *last_used_at)
        .map(|(key, _)| key.clone())?;
    let Some(WebSocketPoolSlot::Idle { connection, .. }) = state.slots.remove(&key) else {
        return None;
    };
    Some(*connection)
}

pub(super) async fn close_pooled_connection(connection: PooledWebSocketConnection) {
    connection.websocket.close().await;
}

pub(super) async fn close_pooled_connections(connections: Vec<PooledWebSocketConnection>) {
    for connection in connections {
        close_pooled_connection(connection).await;
    }
}

/// idle 连接是否应从池中摘除：被后台 pump 标记死亡，或已超过 `max_age`。
pub(super) fn should_close_idle_connection(
    connection: &PooledWebSocketConnection,
    now: Instant,
    max_age: Duration,
) -> bool {
    connection.websocket.is_closed() || now.duration_since(connection.created_at) >= max_age
}

fn short_sha256<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    hex::encode(hasher.finalize()).chars().take(12).collect()
}
