//! 流式 WebSocket exchange 与连接回收。

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use gateway_core::diagnostics::{TraceContext, diagnostic_json};
use serde_json::json;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use super::super::{
    pool::{
        CodexWebSocketConnectionMetadata, PooledWebSocketConnection, WebSocketContinuationState,
        WebSocketPoolLease,
    },
    pump::{
        PumpExitReason, PumpedWebSocket, WebSocketConnectionObservation, transport_metric_reason,
    },
};
use super::io::{next_websocket_message, reused_stream_receive_error};
use super::reducer::{ExchangeAction, WebSocketTerminalKind, reduce_websocket_event};
use super::{
    CodexWebSocketExchangeError, CodexWebSocketRateLimitUpdates,
    CodexWebSocketResponseMetadataUpdate, CodexWebSocketResponseMetadataUpdates,
    CodexWebSocketStreamingExchange, WEBSOCKET_STREAM_BUFFER, reusable_websocket_metadata,
};

pub(in crate::transport::websocket) struct WebSocketStreamPoolReturn {
    pub(in crate::transport::websocket) lease: WebSocketPoolLease,
    pub(in crate::transport::websocket) created_at: tokio::time::Instant,
    pub(in crate::transport::websocket) continuation: WebSocketContinuationState,
}

#[derive(Debug, Clone, Copy)]
enum StreamWebSocketDiscardReason {
    ClientCancelled,
    DownstreamSendFailed,
    IncompleteResponse,
    FailedResponse,
    UnexpectedBinaryEvent,
    PoolShutdown,
    UpstreamClosed,
    UpstreamReceiveFailed,
}

impl StreamWebSocketDiscardReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::ClientCancelled => "client_cancelled",
            Self::DownstreamSendFailed => "downstream_send_failed",
            Self::IncompleteResponse => "incomplete_response",
            Self::FailedResponse => "failed_response",
            Self::UnexpectedBinaryEvent => "unexpected_binary_event",
            Self::PoolShutdown => "pool_shutdown",
            Self::UpstreamClosed => "upstream_closed",
            Self::UpstreamReceiveFailed => "upstream_receive_failed",
        }
    }
}

pub(in crate::transport::websocket) fn stream_websocket_response(
    websocket: PumpedWebSocket,
    metadata: CodexWebSocketConnectionMetadata,
    pool_return: Option<WebSocketStreamPoolReturn>,
    reused_connection: bool,
    stream_idle_timeout: Option<Duration>,
    trace: TraceContext,
) -> CodexWebSocketStreamingExchange {
    let websocket_connection_id = websocket.connection_id();
    let response_metadata = metadata.clone();
    let rate_limit_updates = Arc::new(Mutex::new(Vec::new()));
    let rate_limit_updates_for_task = Arc::clone(&rate_limit_updates);
    let response_metadata_updates = Arc::new(Mutex::new(CodexWebSocketResponseMetadataUpdate {
        turn_state: metadata.turn_state.clone(),
        reported_model: None,
    }));
    let response_metadata_updates_for_task = Arc::clone(&response_metadata_updates);
    let (tx, rx) = mpsc::channel(WEBSOCKET_STREAM_BUFFER);
    let (task_tracker, shutdown) = pool_return
        .as_ref()
        .map(|pool_return| pool_return.lease.stream_task_context())
        .map_or_else(
            || (None, CancellationToken::new()),
            |(tasks, shutdown)| (Some(tasks), shutdown),
        );
    let forward = async move {
        forward_websocket_response_stream(WebSocketStreamForwardState {
            websocket,
            metadata,
            pool_return,
            reused_connection,
            stream_idle_timeout,
            trace,
            shutdown,
            rate_limit_updates: rate_limit_updates_for_task,
            response_metadata_updates: response_metadata_updates_for_task,
            tx,
        })
        .await;
    };
    if let Some(task_tracker) = task_tracker {
        drop(task_tracker.spawn(forward));
    } else {
        drop(tokio::spawn(forward));
    }

    let body = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });

    CodexWebSocketStreamingExchange {
        websocket_connection_id,
        body: Box::pin(body),
        turn_state: response_metadata.turn_state,
        set_cookie_headers: response_metadata.set_cookie_headers,
        rate_limit_headers: response_metadata.rate_limit_headers,
        rate_limit_updates,
        response_metadata_updates,
        pool_decision: None,
        connection_local_continuation: false,
        diagnostics: response_metadata.diagnostics,
        response_metadata: response_metadata.response_metadata,
    }
}

struct WebSocketStreamForwardState {
    trace: TraceContext,
    websocket: PumpedWebSocket,
    metadata: CodexWebSocketConnectionMetadata,
    pool_return: Option<WebSocketStreamPoolReturn>,
    reused_connection: bool,
    stream_idle_timeout: Option<Duration>,
    shutdown: CancellationToken,
    rate_limit_updates: CodexWebSocketRateLimitUpdates,
    response_metadata_updates: CodexWebSocketResponseMetadataUpdates,
    tx: mpsc::Sender<Result<Bytes, CodexWebSocketExchangeError>>,
}

async fn forward_websocket_response_stream(state: WebSocketStreamForwardState) {
    let WebSocketStreamForwardState {
        trace,
        mut websocket,
        mut metadata,
        pool_return,
        reused_connection,
        stream_idle_timeout,
        shutdown,
        rate_limit_updates,
        response_metadata_updates,
        tx,
    } = state;
    let mut pool_return = pool_return;
    let mut continuation = pool_return
        .as_mut()
        .map(|pool_return| std::mem::take(&mut pool_return.continuation))
        .unwrap_or_default();
    let mut last_event_type = None;
    // 官方 run_websocket_response_stream 按响应消费 metadata 事件；它们不属于握手头。
    // 复用连接时恢复握手快照，避免上一轮模型信息及普通响应头随每次请求累积。
    let opening_response_metadata = metadata.response_metadata.clone();
    loop {
        let message = tokio::select! {
            biased;
            // 客户端断开下游 SSE 流：立即丢弃连接并释放池 slot，
            // 不再傻等上游 idle 超时（否则同会话后续请求会一直 bypass/busy）。
            () = tx.closed() => {
                trace.record("upstream.cancelled", json!({"reason": "receiver_dropped"}));
                discard_stream_websocket(
                    websocket,
                    pool_return,
                    StreamWebSocketDiscardReason::ClientCancelled,
                ).await;
                return;
            }
            () = shutdown.cancelled() => {
                trace.record("upstream.cancelled", json!({"reason": "pool_shutdown"}));
                discard_stream_websocket(
                    websocket,
                    pool_return,
                    StreamWebSocketDiscardReason::PoolShutdown,
                ).await;
                return;
            }
            message = next_websocket_message(
                &mut websocket,
                stream_idle_timeout.filter(|timeout| !timeout.is_zero()).unwrap_or(super::super::pool::DEFAULT_STREAM_IDLE_TIMEOUT),
            ) => message,
        };
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                let observation = connection_observation(&websocket, &error);
                trace.record(
                    "upstream.read.failed",
                    json!({
                        "lastEventType": last_event_type,
                        "failureReason": error.transport_failure_reason().unwrap_or_else(|| observation.exit_reason()),
                        "connectionId": observation.connection_id().to_string(),
                        "connectionAgeMs": observation.age_ms(),
                        "connectionIdleMs": observation.idle_ms(),
                        "reused": reused_connection,
                        "error": diagnostic_json(&json!({"message": error.to_string()})),
                    }),
                );
                discard_stream_websocket(
                    websocket,
                    pool_return,
                    StreamWebSocketDiscardReason::UpstreamReceiveFailed,
                )
                .await;
                let error = error.with_connection_observation(observation);
                let error = if reused_connection {
                    reused_stream_receive_error(error)
                } else {
                    error
                };
                let _ = tx.send(Err(error)).await;
                return;
            }
        };
        let Some(message) = message else {
            trace.record(
                "upstream.eof",
                json!({"lastEventType": last_event_type, "terminalSeen": false}),
            );
            break;
        };
        let raw = match message {
            tungstenite::Message::Text(text) => text.to_string(),
            tungstenite::Message::Binary(bytes) => {
                trace.capture("upstream.binary", &bytes);
                let error = CodexWebSocketExchangeError::UnexpectedBinaryEvent;
                let observation = connection_observation(&websocket, &error);
                discard_stream_websocket(
                    websocket,
                    pool_return,
                    StreamWebSocketDiscardReason::UnexpectedBinaryEvent,
                )
                .await;
                let _ = tx
                    .send(Err(error.with_connection_observation(observation)))
                    .await;
                return;
            }
            tungstenite::Message::Close(frame) => {
                if let Some(frame) = &frame {
                    trace.dump("upstream.close.reason", frame.reason.as_bytes());
                }
                trace.record("upstream.close", json!({
                    "code": frame.as_ref().map(|frame| u16::from(frame.code)),
                    "reason": frame.as_ref().map(|frame| diagnostic_json(&json!({"message": frame.reason.as_str()}))),
                    "lastEventType": last_event_type, "terminalSeen": false,
                    "connectionId": websocket.connection_id().to_string(),
                }));
                let websocket_connection_id = websocket.connection_id();
                let error = CodexWebSocketExchangeError::closed_before_terminal_on(
                    websocket_connection_id,
                    frame.as_ref().map(|frame| u16::from(frame.code)),
                    frame.map(|frame| frame.reason.to_string()),
                    last_event_type.clone(),
                );
                let observation = connection_observation(&websocket, &error);
                discard_stream_websocket(
                    websocket,
                    pool_return,
                    StreamWebSocketDiscardReason::UpstreamClosed,
                )
                .await;
                let error = error.with_connection_observation(observation);
                let error = if reused_connection {
                    reused_stream_receive_error(error)
                } else {
                    error
                };
                let _ = tx.send(Err(error)).await;
                return;
            }
            _ => continue,
        };
        trace.capture("upstream.event", raw.as_bytes());
        let reduced = match reduce_websocket_event(&raw, &mut metadata, &mut continuation) {
            Ok(reduced) => reduced,
            Err(error) => {
                let observation =
                    (!matches!(error.classified(), CodexWebSocketExchangeError::Upstream(_)))
                        .then(|| connection_observation(&websocket, &error));
                discard_stream_websocket(
                    websocket,
                    pool_return,
                    StreamWebSocketDiscardReason::UpstreamReceiveFailed,
                )
                .await;
                let error = match observation {
                    Some(observation) => error.with_connection_observation(observation),
                    None => error,
                };
                let _ = tx.send(Err(error)).await;
                return;
            }
        };
        if let Some(event_type) = reduced.diagnostic_event_type {
            last_event_type = Some(event_type);
        }
        if let Some(turn_state) = reduced.turn_state_update {
            let mut pending = response_metadata_updates.lock().await;
            if pending.turn_state.is_none() {
                pending.turn_state = Some(turn_state);
            }
        }
        if let Some(model) = metadata.response_metadata.effective_model.as_ref() {
            response_metadata_updates.lock().await.reported_model = Some(model.clone());
        }
        let (frame, terminal) = match reduced.action {
            ExchangeAction::RateLimits(rate_limits) => {
                rate_limit_updates.lock().await.push(rate_limits);
                continue;
            }
            ExchangeAction::Forward { frame, terminal } => (frame, terminal),
            ExchangeAction::Ignore => continue,
        };
        if tx.send(Ok(Bytes::from(frame))).await.is_err() {
            trace.record(
                "upstream.forward.failed",
                json!({"reason": "receiver_dropped"}),
            );
            discard_stream_websocket(
                websocket,
                pool_return,
                StreamWebSocketDiscardReason::DownstreamSendFailed,
            )
            .await;
            return;
        }
        if let Some(terminal) = terminal {
            trace.record(
                "upstream.terminal",
                json!({"kind": format!("{terminal:?}")}),
            );
            match terminal {
                WebSocketTerminalKind::Completed => {
                    metadata.response_metadata = opening_response_metadata;
                    finish_stream_websocket(websocket, metadata, continuation, pool_return.take())
                        .await;
                }
                WebSocketTerminalKind::Incomplete => {
                    discard_stream_websocket(
                        websocket,
                        pool_return,
                        StreamWebSocketDiscardReason::IncompleteResponse,
                    )
                    .await;
                }
                WebSocketTerminalKind::Failed => {
                    discard_stream_websocket(
                        websocket,
                        pool_return,
                        StreamWebSocketDiscardReason::FailedResponse,
                    )
                    .await;
                }
            }
            return;
        }
    }

    let websocket_connection_id = websocket.connection_id();
    let upstream_close = websocket
        .exit_reason()
        .and_then(|reason| reason.upstream_close().cloned())
        .map(|close| close.with_connection_id(websocket_connection_id));
    let error = match upstream_close {
        Some(close) => CodexWebSocketExchangeError::ClosedBeforeTerminal(
            close.with_last_event_type(last_event_type),
        ),
        None => CodexWebSocketExchangeError::closed_before_terminal_on(
            websocket_connection_id,
            None,
            None,
            last_event_type,
        ),
    };
    let observation = connection_observation(&websocket, &error);
    discard_stream_websocket(
        websocket,
        pool_return,
        StreamWebSocketDiscardReason::UpstreamClosed,
    )
    .await;
    let error = error.with_connection_observation(observation);
    let error = if reused_connection {
        reused_stream_receive_error(error)
    } else {
        error
    };
    let _ = tx.send(Err(error)).await;
}

async fn finish_stream_websocket(
    websocket: PumpedWebSocket,
    metadata: CodexWebSocketConnectionMetadata,
    continuation: WebSocketContinuationState,
    pool_return: Option<WebSocketStreamPoolReturn>,
) {
    let Some(pool_return) = pool_return else {
        websocket.close().await;
        return;
    };
    pool_return
        .lease
        .put(PooledWebSocketConnection {
            websocket,
            metadata: reusable_websocket_metadata(metadata),
            continuation,
            created_at: pool_return.created_at,
        })
        .await;
}

async fn discard_stream_websocket(
    websocket: PumpedWebSocket,
    pool_return: Option<WebSocketStreamPoolReturn>,
    reason: StreamWebSocketDiscardReason,
) {
    let websocket_connection_id = websocket.connection_id();
    let observation = websocket.observation();
    let tombstone_observation = if observation.exit_reason() == "running" {
        observation.clone().with_exit_reason(reason.as_str())
    } else {
        observation.clone()
    };
    let pump_exit = websocket.exit_reason();
    let pump_exit_reason = pump_exit
        .as_ref()
        .map(PumpExitReason::as_str)
        .unwrap_or("running");
    let pump_exit_detail = pump_exit
        .as_ref()
        .and_then(PumpExitReason::detail)
        .unwrap_or_default();
    tracing::info!(
        websocket_connection_id = %websocket_connection_id,
        reason = reason.as_str(),
        pump_exit_reason,
        pump_exit_detail,
        connection_exit_reason = observation.exit_reason(),
        connection_age_ms = observation.age_ms(),
        connection_idle_ms = observation.idle_ms(),
        pooled = pool_return.is_some(),
        "Discarding Responses WebSocket stream"
    );
    if let Some(pool_return) = pool_return {
        pool_return
            .lease
            .discard_with_observation(tombstone_observation)
            .await;
    }
    websocket.close().await;
}

fn connection_observation(
    websocket: &PumpedWebSocket,
    error: &CodexWebSocketExchangeError,
) -> WebSocketConnectionObservation {
    let observation = websocket.observation();
    if observation.exit_reason() != "running" {
        return observation;
    }
    observation.with_exit_reason(exchange_exit_reason(error))
}

fn exchange_exit_reason(error: &CodexWebSocketExchangeError) -> &'static str {
    match error.classified() {
        CodexWebSocketExchangeError::Transport(error) => transport_metric_reason(error),
        CodexWebSocketExchangeError::ClosedBeforeTerminal(close) => {
            if close.code() == Some(1000) {
                "normal_close"
            } else {
                "upstream_close"
            }
        }
        CodexWebSocketExchangeError::ReceiveIdleTimeout { .. } => "receive_idle_timeout",
        CodexWebSocketExchangeError::UnexpectedBinaryEvent => "unexpected_binary_event",
        _ => "exchange_failure",
    }
}
