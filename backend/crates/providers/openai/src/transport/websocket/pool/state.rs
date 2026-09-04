//! WebSocket 连接池状态和值对象。

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use sha2::{Digest, Sha256};
use tokio::{sync::watch, time::Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::super::super::{
    diagnostics::CodexUpstreamDiagnostics, response_meta::CodexResponseMetadata,
};
use super::super::pump::{PumpedWebSocket, WebSocketConnectionObservation};
use super::lease::WebSocketPoolConnectOutcome;

/// WebSocket 连接池 key。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CodexWebSocketPoolKey {
    base_url: String,
    account_id: String,
    conversation_id: String,
    connection_profile: String,
    downstream_connection_id: String,
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
            downstream_connection_id: String::new(),
        }
    }

    /// 区分实际 WebSocket opening 画像，防止复用旧 UA 或不同握手语义的连接。
    pub(crate) fn with_connection_profile(mut self, connection_profile: impl Into<String>) -> Self {
        self.connection_profile = connection_profile.into();
        self
    }

    /// 隔离同一逻辑会话中由不同下游 WebSocket 驱动的并发响应链。
    pub(crate) fn with_downstream_connection_id(
        mut self,
        connection_id: impl Into<String>,
    ) -> Self {
        self.downstream_connection_id = connection_id.into();
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
            self.downstream_connection_id.as_str(),
        ])
    }

    pub(super) fn same_logical_connection(&self, other: &Self) -> bool {
        self.base_url == other.base_url
            && self.account_id == other.account_id
            && self.conversation_id == other.conversation_id
    }
}

const CONTINUATION_TOMBSTONE_TTL: Duration = Duration::from_mins(30);
const MAX_CONTINUATION_TOMBSTONES: usize = 4_096;

#[derive(Default)]
pub(super) struct WebSocketPoolState {
    pub(super) slots: HashMap<CodexWebSocketPoolKey, WebSocketPoolSlot>,
    continuation_tombstones: VecDeque<WebSocketContinuationTombstone>,
    pub(super) shutting_down: bool,
}

struct WebSocketContinuationTombstone {
    key: CodexWebSocketPoolKey,
    response_id: String,
    observation: WebSocketConnectionObservation,
    expires_at: Instant,
}

impl WebSocketPoolState {
    pub(super) fn remember_continuation_loss(
        &mut self,
        key: &CodexWebSocketPoolKey,
        response_id: Option<&str>,
        observation: WebSocketConnectionObservation,
        now: Instant,
    ) {
        let Some(response_id) = response_id else {
            return;
        };
        self.prune_continuation_tombstones(now);
        self.continuation_tombstones.retain(|tombstone| {
            !(tombstone.key.same_logical_connection(key) && tombstone.response_id == response_id)
        });
        while self.continuation_tombstones.len() >= MAX_CONTINUATION_TOMBSTONES {
            self.continuation_tombstones.pop_front();
        }
        self.continuation_tombstones
            .push_back(WebSocketContinuationTombstone {
                key: key.clone(),
                response_id: response_id.to_owned(),
                observation,
                expires_at: now + CONTINUATION_TOMBSTONE_TTL,
            });
    }

    pub(super) fn continuation_loss(
        &mut self,
        key: &CodexWebSocketPoolKey,
        response_id: &str,
        now: Instant,
    ) -> Option<WebSocketConnectionObservation> {
        self.prune_continuation_tombstones(now);
        self.continuation_tombstones
            .iter()
            .rev()
            .find(|tombstone| {
                tombstone.key.same_logical_connection(key) && tombstone.response_id == response_id
            })
            .map(|tombstone| tombstone.observation.clone())
    }

    fn prune_continuation_tombstones(&mut self, now: Instant) {
        self.continuation_tombstones
            .retain(|tombstone| tombstone.expires_at > now);
    }
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
