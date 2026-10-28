//! 单 owner 驱动连接读写、心跳与有界入站队列

use super::{
    frame::*,
    middleware,
    state::{ConnectionCommand, ConnectionStats, PumpContext},
};
use axum::extract::ws::Message;
use futures::{Sink, Stream, StreamExt};
use gateway_core::{
    engine::middleware::{FrozenMiddlewarePlan, MiddlewareHeader, websocket as contract},
    lifecycle::CancellationToken,
};
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{
    sync::{Semaphore, mpsc, oneshot, watch},
    time::Instant,
};

#[expect(clippy::too_many_arguments)]
pub(super) async fn run_pump<S, E>(
    socket: S,
    commands: mpsc::Receiver<ConnectionCommand>,
    incoming: mpsc::Sender<PendingConnectionEvent>,
    exited: oneshot::Sender<PumpExitReason>,
    connection_id: Arc<str>,
    cancellation: CancellationToken,
    opened_at: Instant,
    expired: Arc<AtomicBool>,
    stats: Arc<ConnectionStats>,
    config: ConnectionConfig,
    middleware: Option<FrozenMiddlewarePlan>,
    headers: Arc<[MiddlewareHeader]>,
    extensions: gateway_core::engine::extensions::ExtensionCallScope,
) where
    S: Stream<Item = Result<Message, E>> + Sink<Message, Error = E> + Unpin + Send + 'static,
    E: fmt::Display + Send + 'static,
{
    let _connection_lifetime = middleware::CancelOnDrop(Some(cancellation.clone()));
    let context = Arc::new(PumpContext {
        extensions,
        connection_id,
        cancellation: cancellation.child_token(),
        opened_at,
        expired,
        stats,
        config,
        middleware,
        headers,
    });
    let _lifetime = middleware::CancelOnDrop(Some(context.cancellation.clone()));
    let (writer, mut reader) = socket.split();
    let writer = Arc::new(middleware::Writer::new(writer, context.clone()));
    let sender: Arc<dyn contract::Sender> = writer.clone();
    let (activity, received_activity) = watch::channel(());
    // 心跳独立于业务写入等待，但共享唯一 writer，避免长写入或排队阻止失联检测
    let reason = tokio::select! {
        biased;
        reason = read_connection(&mut reader, &incoming, sender.clone(), &activity, &context) => reason,
        reason = write_connection(writer.clone(), commands, &context) => reason,
        reason = heartbeat_connection(writer.clone(), received_activity, &context) => reason,
    };
    if matches!(reason, PumpExitReason::ClientClose) {
        if let Err(error) = writer.finish_peer_close(&cancellation).await {
            tracing::debug!(websocket_connection_id = %context.connection_id, %error, "Responses WebSocket close acknowledgement could not be flushed");
        }
    } else {
        context.cancellation.cancel();
    }
    let _ = exited.send(reason);
    tracing::debug!(websocket_connection_id = %context.connection_id, pump_exit_reason = reason.as_str(), "Responses WebSocket pump exited");
}

async fn read_connection<S, E>(
    socket: &mut S,
    incoming: &mpsc::Sender<PendingConnectionEvent>,
    sender: Arc<dyn contract::Sender>,
    activity: &watch::Sender<()>,
    context: &PumpContext,
) -> PumpExitReason
where
    S: Stream<Item = Result<Message, E>> + Unpin,
    E: fmt::Display,
{
    let event_slots = Arc::new(Semaphore::new(INBOUND_EVENT_BUFFER));
    let deadline = tokio::time::sleep_until(context.opened_at + context.config.max_age);
    tokio::pin!(deadline);
    let mut deadline_elapsed = false;
    loop {
        tokio::select! {
            biased;
            () = context.cancellation.cancelled() => return PumpExitReason::LifecycleShutdown,
            () = &mut deadline, if !deadline_elapsed => {
                deadline_elapsed = true;
                context.expired.store(true, Ordering::Release);
                // 到期只阻止下一轮，当前响应仍由协调层按原有合同收尾
                match emit_incoming(incoming, &event_slots, ConnectionEvent::Expired) {
                    Ok(()) | Err(PumpExitReason::InboundOverload) => {}
                    Err(reason) => return reason,
                }
            }
            message = socket.next() => {
                if matches!(&message, Some(Ok(_))) { context.stats.record_read(context.opened_at); }
                if matches!(&message, Some(Ok(message)) if !matches!(message, Message::Close(_))) {
                    // 链路活动以真实入站为准，插件丢弃消息不应抹掉对端已经响应的事实
                    activity.send_replace(());
                }
                let message = match message {
                    Some(Ok(message)) => match middleware::transform(message, contract::Direction::Incoming, sender.clone(), context).await {
                        Ok(Some(message)) => Some(Ok(message)),
                        Ok(None) => continue,
                        Err(_) => return PumpExitReason::MiddlewareFailed,
                    },
                    other => other,
                };
                let event = match message {
                    Some(Ok(Message::Text(payload))) => Some(ConnectionEvent::Text(payload.to_string())),
                    Some(Ok(Message::Binary(_))) => Some(ConnectionEvent::Binary),
                    Some(Ok(Message::Ping(_))) => {
                        // Axum/tungstenite 在继续读取时自动刷新 Pong，不能再手工发送一份
                        context.stats.ping_received_count.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                    Some(Ok(Message::Pong(_))) => {
                        context.stats.pong_received_count.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                    Some(Ok(Message::Close(_))) => return PumpExitReason::ClientClose,
                    Some(Err(error)) => {
                        tracing::info!(websocket_connection_id = %context.connection_id, error = %error, "Responses WebSocket receive failed");
                        return PumpExitReason::ReadError;
                    }
                    None => return PumpExitReason::PeerEof,
                };
                if let Some(event) = event && let Err(reason) = emit_incoming(incoming, &event_slots, event) { return reason; }
            }
        }
    }
}

async fn write_connection<S, E>(
    writer: Arc<middleware::Writer<S>>,
    mut commands: mpsc::Receiver<ConnectionCommand>,
    context: &PumpContext,
) -> PumpExitReason
where
    S: Sink<Message, Error = E> + Unpin + Send + 'static,
    E: fmt::Display + Send + 'static,
{
    loop {
        let command = tokio::select! {
            biased;
            () = context.cancellation.cancelled() => return PumpExitReason::LifecycleShutdown,
            command = commands.recv() => command,
        };
        let Some(command) = command else {
            return PumpExitReason::CoordinatorDropped;
        };
        let ConnectionCommand {
            message,
            context: write_context,
            acknowledged,
        } = command;
        let closing = matches!(message, Message::Close(_));
        let result = match middleware::transform(
            message,
            contract::Direction::Outgoing,
            writer.clone(),
            context,
        )
        .await
        {
            Ok(Some(message)) => writer.write(message).await.map(|()| WriteOutcome::Written),
            Ok(None) => Ok(WriteOutcome::Suppressed),
            Err(error) => Err(ConnectionWriteError::Transport {
                message: error.to_string(),
            }),
        };
        let reason = result
            .as_ref()
            .err()
            .map(|error| write_exit_reason(error, context));
        if let Err(error) = &result {
            tracing::debug!(websocket_connection_id = %context.connection_id, request_id = write_context.request_id.as_deref().unwrap_or(""), frame_phase = write_context.phase.as_str(), error = %error, "Responses WebSocket pump transport write failed");
        }
        let _ = acknowledged.send(result);
        if let Some(reason) = reason {
            return reason;
        }
        if closing {
            return PumpExitReason::ServerClose;
        }
    }
}

async fn heartbeat_connection<S, E>(
    writer: Arc<middleware::Writer<S>>,
    mut activity: watch::Receiver<()>,
    context: &PumpContext,
) -> PumpExitReason
where
    S: Sink<Message, Error = E> + Unpin + Send + 'static,
    E: fmt::Display + Send + 'static,
{
    let mut heartbeat = tokio::time::interval_at(
        context.opened_at + DOWNSTREAM_PING_INTERVAL,
        DOWNSTREAM_PING_INTERVAL,
    );
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ping_sequence = 0_u64;
    let pong_deadline = tokio::time::sleep(DOWNSTREAM_PONG_TIMEOUT);
    tokio::pin!(pong_deadline);
    let mut pending_ping = false;
    loop {
        tokio::select! {
            biased;
            () = context.cancellation.cancelled() => return PumpExitReason::LifecycleShutdown,
            changed = activity.changed() => {
                if changed.is_err() { return PumpExitReason::PumpStopped; }
                pending_ping = false;
                continue;
            }
            () = &mut pong_deadline, if pending_ping => return PumpExitReason::PongTimeout,
            _ = heartbeat.tick() => {}
        }
        ping_sequence = ping_sequence.wrapping_add(1);
        let write = tokio::time::timeout(
            DOWNSTREAM_PING_WRITE_TIMEOUT,
            write_heartbeat(&writer, ping_sequence, context),
        );
        tokio::pin!(write);
        let mut received_during_write = false;
        loop {
            tokio::select! {
                biased;
                () = context.cancellation.cancelled() => return PumpExitReason::LifecycleShutdown,
                changed = activity.changed() => {
                    if changed.is_err() { return PumpExitReason::PumpStopped; }
                    pending_ping = false;
                    received_during_write = true;
                }
                () = &mut pong_deadline, if pending_ping => return PumpExitReason::PongTimeout,
                result = &mut write => {
                    match result {
                        Ok(Ok(true)) => {
                            context.stats.ping_written_count.fetch_add(1, Ordering::Relaxed);
                            // 重复 Ping 不顺延截止，写入完成前已经收到的响应也不能丢失
                            if !pending_ping && !received_during_write {
                                pong_deadline.as_mut().reset(Instant::now() + DOWNSTREAM_PONG_TIMEOUT);
                                pending_ping = true;
                            }
                        }
                        Ok(Ok(false)) => {}
                        Ok(Err(reason)) => return reason,
                        Err(_) => return PumpExitReason::WriteTimeout,
                    }
                    break;
                }
            }
        }
    }
}

async fn write_heartbeat<S, E>(
    writer: &Arc<middleware::Writer<S>>,
    sequence: u64,
    context: &PumpContext,
) -> Result<bool, PumpExitReason>
where
    S: Sink<Message, Error = E> + Unpin + Send + 'static,
    E: fmt::Display + Send + 'static,
{
    let message = middleware::transform(
        Message::Ping(sequence.to_be_bytes().to_vec().into()),
        contract::Direction::Outgoing,
        writer.clone(),
        context,
    )
    .await
    .map_err(|_| PumpExitReason::MiddlewareFailed)?;
    let Some(message) = message else {
        return Ok(false);
    };
    let is_ping = matches!(message, Message::Ping(_));
    writer.write(message).await.map_err(|error| {
        tracing::info!(websocket_connection_id = %context.connection_id, error = %error, "Responses WebSocket control write failed");
        write_exit_reason(&error, context)
    })?;
    Ok(is_ping)
}

fn write_exit_reason(error: &ConnectionWriteError, context: &PumpContext) -> PumpExitReason {
    match error {
        ConnectionWriteError::Timeout { .. } => PumpExitReason::WriteTimeout,
        ConnectionWriteError::Transport { .. } => PumpExitReason::WriteError,
        ConnectionWriteError::Closed if context.cancellation.is_cancelled() => {
            PumpExitReason::LifecycleShutdown
        }
        ConnectionWriteError::Closed => PumpExitReason::PumpStopped,
    }
}

fn emit_incoming(
    incoming: &mpsc::Sender<PendingConnectionEvent>,
    slots: &Arc<Semaphore>,
    event: ConnectionEvent,
) -> Result<(), PumpExitReason> {
    let permit = Arc::clone(slots)
        .try_acquire_owned()
        .map_err(|_| PumpExitReason::InboundOverload)?;
    incoming
        .try_send(PendingConnectionEvent {
            event,
            _permit: Some(permit),
        })
        .map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => PumpExitReason::InboundOverload,
            mpsc::error::TrySendError::Closed(_) => PumpExitReason::CoordinatorDropped,
        })
}
