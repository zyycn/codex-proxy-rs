//! Responses WebSocket pre-send/post-send 与 pool/breaker 编排。

use std::time::{Duration, Instant};

use tokio::{sync::oneshot, time::timeout};

use super::{
    breaker::{
        WebSocketOriginBreaker, WebSocketOriginBreakerDecision, WebSocketOriginBreakerPermit,
        WebSocketOriginFastPathReporter,
    },
    error::CodexWebSocketExchangeError,
    exchange::{
        CodexWebSocketStreamingExchange, WebSocketStreamPoolReturn, stream_websocket_response,
    },
    handshake::{connect_pumped_websocket, send_websocket_request, websocket_connection_metadata},
    model::{
        CodexWebSocketConnection, CodexWebSocketRequest, PreviousResponseUnavailableReason,
        WebSocketContinuationRequirement,
    },
    pool::{
        self, CodexWebSocketPool, CodexWebSocketPoolKey, PooledWebSocketConnection,
        WebSocketContinuationState, WebSocketPoolAcquire, WebSocketPoolBypassReason,
        WebSocketPoolConnectLease, WebSocketPoolConnectOutcome, WebSocketPoolDecision,
        WebSocketPoolLease,
    },
    pump::{PumpKeepalive, PumpedWebSocket},
};

pub(crate) const WEBSOCKET_FAST_PATH_BUDGET: Duration = Duration::from_millis(800);

/// 尚未发送 `response.create` 的 WebSocket。只有该类型可以安全切换到 HTTP。
pub(crate) struct PreparedWebSocket {
    connection: PooledWebSocketConnection,
    binding: PoolBinding,
    connect_elapsed: Option<Duration>,
    decision_wait_elapsed: Duration,
    initial_event_timeout: Option<Duration>,
}

enum PoolBinding {
    Unpooled,
    Pooled {
        lease: Box<WebSocketPoolLease>,
        reused: bool,
        decision: WebSocketPoolDecision,
    },
}

impl PoolBinding {
    fn decision(&self) -> Option<WebSocketPoolDecision> {
        match self {
            Self::Unpooled => None,
            Self::Pooled { decision, .. } => Some(*decision),
        }
    }

    fn reused(&self) -> bool {
        matches!(self, Self::Pooled { reused: true, .. })
    }

    fn into_parts(
        self,
    ) -> (
        Option<WebSocketPoolLease>,
        bool,
        Option<WebSocketPoolDecision>,
    ) {
        match self {
            Self::Unpooled => (None, false, None),
            Self::Pooled {
                lease,
                reused,
                decision,
            } => (Some(*lease), reused, Some(decision)),
        }
    }
}

impl PreparedWebSocket {
    pub(crate) fn pool_decision(&self) -> Option<WebSocketPoolDecision> {
        self.binding.decision()
    }

    pub(crate) fn reused(&self) -> bool {
        self.binding.reused()
    }

    pub(crate) fn connect_elapsed(&self) -> Option<Duration> {
        self.connect_elapsed
    }

    pub(crate) fn decision_wait_elapsed(&self) -> Duration {
        self.decision_wait_elapsed
    }
}

/// 只建立或租用 WebSocket，不发送 payload。
pub(crate) async fn prepare_response_create_request_with_pool(
    request: &CodexWebSocketRequest,
    pool: Option<(&CodexWebSocketPool, CodexWebSocketPoolKey)>,
    breaker: &WebSocketOriginBreaker,
    origin_key: &str,
    fast_path_budget: Option<Duration>,
    require_pool: bool,
    fallback_initial_event_timeout: Option<Duration>,
) -> Result<PreparedWebSocket, CodexWebSocketExchangeError> {
    let decision_started_at = Instant::now();
    let Some((pool, key)) = pool else {
        if require_pool || !request.continuation().permits_fresh_connection() {
            return Err(continuation_unavailable(
                PreviousResponseUnavailableReason::PoolUnavailable,
            ));
        }
        return prepare_unpooled_websocket(
            request,
            breaker,
            origin_key,
            fast_path_budget,
            fallback_initial_event_timeout,
            decision_started_at,
        )
        .await;
    };

    let required_response_id = match request.continuation() {
        WebSocketContinuationRequirement::ConnectionLocal { response_id } => {
            Some(response_id.as_str())
        }
        WebSocketContinuationRequirement::NewChain
        | WebSocketContinuationRequirement::Persisted { .. }
        | WebSocketContinuationRequirement::ExternalUnknown { .. } => None,
    };
    match pool.acquire(&key, required_response_id).await {
        WebSocketPoolAcquire::Reused { connection, lease } => {
            if let WebSocketContinuationRequirement::ConnectionLocal { response_id } =
                request.continuation()
                && connection.continuation.latest_response_id() != Some(response_id.as_str())
            {
                lease.put(*connection).await;
                return Err(continuation_unavailable(
                    PreviousResponseUnavailableReason::LatestResponseMismatch,
                ));
            }
            Ok(PreparedWebSocket {
                connection: *connection,
                binding: PoolBinding::Pooled {
                    lease: Box::new(lease),
                    reused: true,
                    decision: WebSocketPoolDecision::reuse(),
                },
                connect_elapsed: None,
                decision_wait_elapsed: decision_started_at.elapsed(),
                initial_event_timeout: pool.initial_event_timeout(),
            })
        }
        WebSocketPoolAcquire::Connect(connect_lease) => {
            if !request.continuation().permits_fresh_connection() {
                connect_lease.failed().await;
                return Err(continuation_unavailable(
                    PreviousResponseUnavailableReason::FreshConnectionRequired,
                ));
            }
            prepare_pooled_websocket(
                request,
                pool,
                connect_lease,
                breaker,
                origin_key,
                fast_path_budget,
                decision_started_at,
            )
            .await
        }
        WebSocketPoolAcquire::Wait(waiter) => {
            if require_pool || !request.continuation().permits_fresh_connection() {
                return Err(continuation_unavailable(
                    PreviousResponseUnavailableReason::ConnectionBusy,
                ));
            }
            let outcome = wait_for_shared_connect(waiter, fast_path_budget).await?;
            match outcome {
                WebSocketPoolConnectOutcome::Ready => Err(continuation_unavailable(
                    PreviousResponseUnavailableReason::ConnectionBusy,
                )),
                WebSocketPoolConnectOutcome::Failed | WebSocketPoolConnectOutcome::Pending => {
                    Err(CodexWebSocketExchangeError::SharedConnectFailed)
                }
            }
        }
        WebSocketPoolAcquire::Bypass(reason) => {
            Err(continuation_unavailable(bypass_unavailable_reason(reason)))
        }
    }
}

async fn prepare_unpooled_websocket(
    request: &CodexWebSocketRequest,
    breaker: &WebSocketOriginBreaker,
    origin_key: &str,
    fast_path_budget: Option<Duration>,
    initial_event_timeout: Option<Duration>,
    decision_started_at: Instant,
) -> Result<PreparedWebSocket, CodexWebSocketExchangeError> {
    let permit = acquire_breaker_permit(breaker, origin_key)?;
    let connected = connect_with_budget(
        request.connection(),
        PumpKeepalive::disabled(),
        fast_path_budget,
    )
    .await;
    let (connection, connect_elapsed) = finish_breaker_attempt(permit, connected)?;
    Ok(PreparedWebSocket {
        connection,
        binding: PoolBinding::Unpooled,
        connect_elapsed: Some(connect_elapsed),
        decision_wait_elapsed: decision_started_at.elapsed(),
        initial_event_timeout,
    })
}

async fn prepare_pooled_websocket(
    request: &CodexWebSocketRequest,
    pool: &CodexWebSocketPool,
    connect_lease: WebSocketPoolConnectLease,
    breaker: &WebSocketOriginBreaker,
    origin_key: &str,
    fast_path_budget: Option<Duration>,
    decision_started_at: Instant,
) -> Result<PreparedWebSocket, CodexWebSocketExchangeError> {
    let permit = match acquire_breaker_permit(breaker, origin_key) {
        Ok(permit) => permit,
        Err(error) => {
            connect_lease.failed().await;
            return Err(error);
        }
    };
    let mut waiter = start_pooled_websocket_connect(
        request.connection().clone(),
        pool.clone(),
        connect_lease,
        permit,
    );
    let handoff = waiter.wait(fast_path_budget).await?;
    Ok(PreparedWebSocket {
        connection: *handoff.connection,
        binding: PoolBinding::Pooled {
            lease: Box::new(handoff.lease),
            reused: false,
            decision: WebSocketPoolDecision::new(),
        },
        connect_elapsed: Some(handoff.connect_elapsed),
        decision_wait_elapsed: decision_started_at.elapsed(),
        initial_event_timeout: pool.initial_event_timeout(),
    })
}

struct PooledWebSocketConnectWaiter {
    started_at: tokio::time::Instant,
    receiver: oneshot::Receiver<Result<PooledWebSocketHandoff, CodexWebSocketExchangeError>>,
    fast_path_reporter: WebSocketOriginFastPathReporter,
}

struct PooledWebSocketHandoff {
    connection: Box<PooledWebSocketConnection>,
    lease: WebSocketPoolLease,
    connect_elapsed: Duration,
}

impl PooledWebSocketConnectWaiter {
    async fn wait(
        &mut self,
        fast_path_budget: Option<Duration>,
    ) -> Result<PooledWebSocketHandoff, CodexWebSocketExchangeError> {
        let received = match fast_path_budget {
            Some(budget) => {
                let remaining = budget.saturating_sub(self.started_at.elapsed());
                match timeout(remaining, &mut self.receiver).await {
                    Ok(received) => received,
                    Err(_) => {
                        self.fast_path_reporter.missed();
                        return Err(CodexWebSocketExchangeError::FastPathTimeout {
                            timeout: budget,
                        });
                    }
                }
            }
            None => (&mut self.receiver).await,
        };
        received.map_err(|_| CodexWebSocketExchangeError::SharedConnectFailed)?
    }
}

fn start_pooled_websocket_connect(
    connection: CodexWebSocketConnection,
    pool: CodexWebSocketPool,
    connect_lease: WebSocketPoolConnectLease,
    permit: WebSocketOriginBreakerPermit,
) -> PooledWebSocketConnectWaiter {
    let task_key = connect_lease.key().clone();
    let started_at = connect_lease.started_at();
    let cancellation = connect_lease.cancellation_token();
    let fast_path_reporter = permit.fast_path_reporter();
    let keepalive = pool.keepalive();
    let (sender, receiver) = oneshot::channel();
    pool.spawn_connect_task(async move {
        let connected = tokio::select! {
            biased;
            _ = cancellation.cancelled() => {
                permit.cancel();
                let _ = sender.send(Err(CodexWebSocketExchangeError::SharedConnectFailed));
                connect_lease.failed().await;
                tracing::info!(
                    account_id = task_key.account_id(),
                    conversation_id_hash = task_key.conversation_id_hash(),
                    ws_preconnect_duration_ms = duration_millis_u64(started_at.elapsed()),
                    ws_preconnect_outcome = "cancelled",
                    "WebSocket pool connect finished"
                );
                return;
            }
            result = connect_with_budget(&connection, keepalive, None) => result,
        };
        match finish_breaker_attempt(permit, connected) {
            Ok((connection, connect_elapsed)) => {
                match connect_lease.connected_reserved(connection).await {
                    Ok((connection, lease)) => {
                        let handoff = PooledWebSocketHandoff {
                            connection,
                            lease,
                            connect_elapsed,
                        };
                        let foreground_waiting = match sender.send(Ok(handoff)) {
                            Ok(()) => true,
                            Err(Ok(handoff)) => {
                                handoff.lease.put(*handoff.connection).await;
                                false
                            }
                            Err(Err(_)) => false,
                        };
                        tracing::info!(
                            account_id = task_key.account_id(),
                            conversation_id_hash = task_key.conversation_id_hash(),
                            ws_preconnect_duration_ms = duration_millis_u64(connect_elapsed),
                            foreground_waiting,
                            ws_preconnect_outcome = "ready",
                            "WebSocket pool connect finished"
                        );
                    }
                    Err(connection) => {
                        connection.websocket.close().await;
                        let _ = sender.send(Err(continuation_unavailable(
                            PreviousResponseUnavailableReason::PoolUnavailable,
                        )));
                        tracing::info!(
                            account_id = task_key.account_id(),
                            conversation_id_hash = task_key.conversation_id_hash(),
                            ws_preconnect_duration_ms = duration_millis_u64(connect_elapsed),
                            ws_preconnect_outcome = "rejected",
                            "WebSocket pool connect finished"
                        );
                    }
                }
            }
            Err(error) => {
                let error_message = error.to_string();
                // 先交付 opening 原始错误，避免连接池清理侵占前台 fast-path 预算。
                let foreground_waiting = sender.send(Err(error)).is_ok();
                connect_lease.failed().await;
                tracing::warn!(
                    account_id = task_key.account_id(),
                    conversation_id_hash = task_key.conversation_id_hash(),
                    ws_preconnect_duration_ms = duration_millis_u64(started_at.elapsed()),
                    foreground_waiting,
                    error = %error_message,
                    ws_preconnect_outcome = "failed",
                    "WebSocket pool connect finished"
                );
            }
        }
    });
    PooledWebSocketConnectWaiter {
        started_at,
        receiver,
        fast_path_reporter,
    }
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis())
        .unwrap_or(u64::MAX)
        .max(1)
}

async fn connect_with_budget(
    connection: &CodexWebSocketConnection,
    keepalive: PumpKeepalive,
    fast_path_budget: Option<Duration>,
) -> Result<(PooledWebSocketConnection, Duration), CodexWebSocketExchangeError> {
    let started_at = Instant::now();
    let connected = match fast_path_budget {
        Some(budget) => timeout(budget, connect_pumped_websocket(connection, keepalive))
            .await
            .map_err(|_| CodexWebSocketExchangeError::FastPathTimeout { timeout: budget })?,
        None => connect_pumped_websocket(connection, keepalive).await,
    }?;
    let (websocket, response) = connected;
    Ok((
        PooledWebSocketConnection {
            websocket,
            metadata: websocket_connection_metadata(&response),
            continuation: WebSocketContinuationState::default(),
            created_at: tokio::time::Instant::now(),
        },
        started_at.elapsed(),
    ))
}

fn acquire_breaker_permit(
    breaker: &WebSocketOriginBreaker,
    origin_key: &str,
) -> Result<WebSocketOriginBreakerPermit, CodexWebSocketExchangeError> {
    match breaker.try_acquire(origin_key) {
        WebSocketOriginBreakerDecision::Allowed(permit) => Ok(permit),
        WebSocketOriginBreakerDecision::Open => Err(CodexWebSocketExchangeError::OriginCircuitOpen),
        WebSocketOriginBreakerDecision::HalfOpenBusy => {
            Err(CodexWebSocketExchangeError::OriginHalfOpenBusy)
        }
    }
}

fn finish_breaker_attempt(
    permit: WebSocketOriginBreakerPermit,
    connected: Result<(PooledWebSocketConnection, Duration), CodexWebSocketExchangeError>,
) -> Result<(PooledWebSocketConnection, Duration), CodexWebSocketExchangeError> {
    match connected {
        Ok(connection) => {
            permit.succeed();
            Ok(connection)
        }
        Err(error @ CodexWebSocketExchangeError::FastPathTimeout { .. }) => {
            permit.fast_timeout();
            Err(error)
        }
        Err(CodexWebSocketExchangeError::Upstream(upstream)) if upstream.status_code < 500 => {
            // 账号或请求级 opening 响应证明 origin 可达，不得污染 transport 熔断器。
            permit.succeed();
            Err(CodexWebSocketExchangeError::Upstream(upstream))
        }
        Err(error) => {
            permit.fail();
            Err(error)
        }
    }
}

async fn wait_for_shared_connect(
    waiter: pool::WebSocketPoolConnectWaiter,
    fast_path_budget: Option<Duration>,
) -> Result<WebSocketPoolConnectOutcome, CodexWebSocketExchangeError> {
    match fast_path_budget {
        Some(budget) => {
            let remaining = waiter.remaining_budget(budget);
            timeout(remaining, waiter.wait())
                .await
                .map_err(|_| CodexWebSocketExchangeError::FastPathTimeout { timeout: budget })
        }
        None => Ok(waiter.wait().await),
    }
}

fn bypass_unavailable_reason(
    reason: WebSocketPoolBypassReason,
) -> PreviousResponseUnavailableReason {
    match reason {
        WebSocketPoolBypassReason::Busy => PreviousResponseUnavailableReason::ConnectionBusy,
        WebSocketPoolBypassReason::Disabled | WebSocketPoolBypassReason::Cap => {
            PreviousResponseUnavailableReason::PoolUnavailable
        }
        WebSocketPoolBypassReason::ContinuationNotFound => {
            PreviousResponseUnavailableReason::FreshConnectionRequired
        }
    }
}

fn continuation_unavailable(
    reason: PreviousResponseUnavailableReason,
) -> CodexWebSocketExchangeError {
    CodexWebSocketExchangeError::ContinuationUnavailable { reason }
}

pub(crate) async fn execute_prepared_response_create_request_stream(
    request: &CodexWebSocketRequest,
    prepared: PreparedWebSocket,
) -> Result<CodexWebSocketStreamingExchange, CodexWebSocketExchangeError> {
    let PreparedWebSocket {
        connection,
        binding,
        initial_event_timeout,
        ..
    } = prepared;
    let (lease, reused, pool_decision) = binding.into_parts();
    let PooledWebSocketConnection {
        websocket,
        metadata,
        continuation,
        created_at,
    } = connection;
    if let Err(error) = send_websocket_request(&websocket, request.payload_text()).await {
        discard_after_send(websocket, lease).await;
        return Err(post_send_ambiguous(error));
    }
    let connection_local_available = lease.is_some();
    let pool_return = lease.map(|lease| WebSocketStreamPoolReturn {
        lease,
        created_at,
        continuation,
    });
    let mut exchange = stream_websocket_response(
        websocket,
        metadata,
        pool_return,
        reused,
        initial_event_timeout,
    );
    exchange.pool_decision = pool_decision;
    exchange.connection_local_continuation = connection_local_available;
    Ok(exchange)
}

async fn discard_after_send(websocket: PumpedWebSocket, lease: Option<WebSocketPoolLease>) {
    if let Some(lease) = lease {
        lease.discard().await;
    }
    websocket.close().await;
}

pub(crate) fn post_send_ambiguous(
    error: CodexWebSocketExchangeError,
) -> CodexWebSocketExchangeError {
    match error {
        error @ CodexWebSocketExchangeError::Upstream(_)
        | error @ CodexWebSocketExchangeError::PostSendAmbiguous { .. } => error,
        error => CodexWebSocketExchangeError::PostSendAmbiguous {
            message: error.to_string(),
            source: Some(Box::new(error)),
        },
    }
}
