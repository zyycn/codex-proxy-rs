//! 单连接共享的命令、运行预算与观测状态

use super::frame::{
    ConnectionConfig, ConnectionWriteError, OUTBOUND_COMMAND_BUFFER, WriteContext, WriteOutcome,
};
use axum::extract::ws::Message;
use gateway_core::{
    engine::middleware::{FrozenMiddlewarePlan, MiddlewareHeader},
    lifecycle::CancellationToken,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

pub(super) struct ConnectionCommand {
    pub(super) message: Message,
    pub(super) context: WriteContext,
    pub(super) acknowledged: oneshot::Sender<Result<WriteOutcome, ConnectionWriteError>>,
}

#[derive(Default)]
pub(super) struct ConnectionStats {
    pub(super) command_queue_high_water: AtomicUsize,
    pub(super) command_backpressure_count: AtomicU64,
    pub(super) ping_received_count: AtomicU64,
    pub(super) ping_written_count: AtomicU64,
    pub(super) pong_received_count: AtomicU64,
    pub(super) last_read_ms: AtomicU64,
    pub(super) last_write_ms: AtomicU64,
}

impl ConnectionStats {
    pub(super) fn record_read(&self, opened_at: Instant) {
        self.last_read_ms.store(
            u64::try_from(opened_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    pub(super) fn record_write(&self, opened_at: Instant) {
        self.last_write_ms.store(
            u64::try_from(opened_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    pub(super) fn observe_command_queue(&self, sender: &mpsc::Sender<ConnectionCommand>) {
        let queued = OUTBOUND_COMMAND_BUFFER
            .saturating_sub(sender.capacity())
            .saturating_add(1)
            .min(OUTBOUND_COMMAND_BUFFER);
        self.command_queue_high_water
            .fetch_max(queued, Ordering::Relaxed);
        if sender.capacity() == 0 {
            self.command_backpressure_count
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub(super) struct PumpContext {
    pub(super) extensions: gateway_core::engine::extensions::ExtensionCallScope,
    pub(super) middleware: Option<FrozenMiddlewarePlan>,
    pub(super) headers: Arc<[MiddlewareHeader]>,
    pub(super) connection_id: Arc<str>,
    pub(super) cancellation: CancellationToken,
    pub(super) opened_at: Instant,
    pub(super) expired: Arc<AtomicBool>,
    pub(super) stats: Arc<ConnectionStats>,
    pub(super) config: ConnectionConfig,
}
